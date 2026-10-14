//! Fixed-file P44 experiment. GPU setup, disk reads, readback and validation are outside timings.
#![forbid(unsafe_code)]
use photocraft_engine::Session;
use photocraft_geom::Rect;
use photocraft_gpu::Compositor;
use serde_json::{Value, json};
use std::{error::Error, path::Path, time::Instant};

fn elapsed(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}
fn configure_cache(comp: &mut Compositor, mode: &str) {
    #[cfg(not(feature = "legacy-compositor"))]
    if mode == "cache-off" {
        comp.set_composite_cache_budget(0);
    }
    #[cfg(feature = "legacy-compositor")]
    let _ = (comp, mode);
}
fn cache_metrics(comp: &Compositor, stats: &photocraft_gpu::Stats) -> Value {
    #[cfg(not(feature = "legacy-compositor"))]
    {
        json!({"bytes":comp.composite_cache_bytes(),"hits":stats.prefix_hits})
    }
    #[cfg(feature = "legacy-compositor")]
    {
        let _ = (comp, stats);
        json!(null)
    }
}
fn median(v: &[Value], key: &str) -> Option<f64> {
    let mut xs: Vec<_> = v.iter().filter_map(|v| v.get(key)?.as_f64()).collect();
    xs.sort_by(f64::total_cmp);
    xs.get(xs.len() / 2).copied()
}
fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let fixture = args.first().ok_or("usage: psd-open-gpu-experiment PSD OUTPUT [reps] [cache-on|cache-off] [shared|fresh]")?;
    let output = args.get(1).ok_or("missing output JSON path")?;
    let reps: usize = args.get(2).map_or(Ok(5), |s| s.parse())?;
    if !(1..=30).contains(&reps) {
        return Err("reps must be 1..30".into());
    }
    let cache = args.get(3).map(String::as_str).unwrap_or("cache-on");
    let fresh = args.get(4).map(String::as_str).unwrap_or("shared") == "fresh";
    if !matches!(cache, "cache-on" | "cache-off") {
        return Err("invalid cache mode".into());
    }
    let bytes = std::fs::read(fixture)?;
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(
        instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() }),
    )?;
    let info = adapter.get_info();
    let a = adapter.limits();
    let limits = wgpu::Limits { max_texture_dimension_2d: a.max_texture_dimension_2d, max_buffer_size: a.max_buffer_size, ..wgpu::Limits::default() };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor { required_limits: limits, ..Default::default() }))?;
    let mut comp = Compositor::new(&device);
    configure_cache(&mut comp, cache);
    let mut samples = Vec::new();
    let mut cold = Value::Null;
    for i in 0..=reps {
        if fresh {
            comp = Compositor::new(&device);
            configure_cache(&mut comp, cache);
        }
        let t = Instant::now();
        let imported = photocraft_io::import(fixture, &bytes)?;
        let import_ms = elapsed(t);
        if !imported.warnings.is_empty() {
            return Err(format!("unexpected import warnings: {:?}", imported.warnings).into());
        }
        let t = Instant::now();
        let mut session = Session::new();
        let (_, color) = session.open_document(imported.document, None);
        let admission_ms = elapsed(t);
        let state = session.active().ok_or("no imported document")?;
        let t = Instant::now();
        let stats = comp.render(&device, &queue, &state.doc, state.doc.bounds(), |_, _| {})?;
        let encode_submit_ms = elapsed(t);
        let t = Instant::now();
        device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None })?;
        let wait_ms = elapsed(t);
        let sample = json!({"import_ms":import_ms,"admission_ms":admission_ms,
            "encode_submit_ms":encode_submit_ms,"wait_ms":wait_ms,
            "total_ms":import_ms+admission_ms+encode_submit_ms+wait_ms,
            "passes":stats.passes,"uploads":stats.tiles_uploaded,"bytes_uploaded":stats.bytes_uploaded,
            "evicted":stats.evicted,"flushes":stats.flushes,"color":color,"prefix":cache_metrics(&comp,&stats)});
        println!("sample {i}: {sample}");
        if i == 0 {
            cold = sample;
        } else {
            samples.push(sample);
        }
    }
    // Separate correctness pass, not included in benchmark timings.
    let doc = photocraft_io::import(fixture, &bytes)?.document;
    let crop = Rect::from_xywh(0, 0, 960, 640).intersect(&doc.bounds());
    let cpu = photocraft_compose::render(&doc, crop);
    let gpu = photocraft_gpu::render_to_vec(&mut comp, &device, &queue, &doc, crop)?;
    if cpu.px.len() != gpu.len() {
        return Err("CPU/GPU output size differs".into());
    }
    let mut max_error = 0.0f32;
    let mut cpu_hash = blake3::Hasher::new();
    let mut preview = Vec::with_capacity(gpu.len().saturating_mul(4));
    for (a, b) in cpu.px.iter().zip(&gpu) {
        for (a, b) in a.iter().zip(b) {
            if !a.is_finite() || !b.is_finite() {
                return Err("nonfinite composite pixel".into());
            }
            max_error = max_error.max((a - b).abs());
            cpu_hash.update(&a.to_le_bytes());
            preview.push((b.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        }
    }
    if max_error > 1.0 / 255.0 + 1e-5 {
        return Err(format!("GPU/CPU mismatch: max {max_error}").into());
    }
    let image =
        photocraft_codecs::Image::from_raw(crop.width(), crop.height(), photocraft_codecs::ChannelLayout::Rgba, photocraft_codecs::SampleType::U8, preview)?;
    let preview_path = Path::new(output).with_extension("png");
    if let Some(p) = Path::new(output).parent() {
        std::fs::create_dir_all(p)?;
    }
    let png = photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default())?;
    std::fs::write(&preview_path, png)?;
    let mut checks = Vec::new();
    // The top-left crop alone misses this fixture's masks and grouped layers.
    for (label, rect) in [
        ("mask", Rect::from_xywh(1093, 737, 960, 640)),
        ("group", Rect::from_xywh(2919, 1971, 960, 640)),
        ("group-mask", Rect::from_xywh(3832, 2588, 960, 640)),
    ] {
        let rect = rect.intersect(&doc.bounds());
        if rect.is_empty() {
            continue;
        }
        let reference = photocraft_compose::render(&doc, rect);
        let actual = photocraft_gpu::render_to_vec(&mut comp, &device, &queue, &doc, rect)?;
        if reference.px.len() != actual.len() {
            return Err("representative GPU crop size differs".into());
        }
        let mut error = 0.0f32;
        let mut h = blake3::Hasher::new();
        let mut rgba = Vec::with_capacity(actual.len().saturating_mul(4));
        for (a, b) in reference.px.iter().zip(&actual) {
            for (a, b) in a.iter().zip(b) {
                if !a.is_finite() || !b.is_finite() {
                    return Err("nonfinite representative crop".into());
                }
                error = error.max((a - b).abs());
                h.update(&a.to_le_bytes());
                rgba.push((b.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
            }
        }
        if error > 1.0 / 255.0 + 1e-5 {
            return Err(format!("{label} GPU/CPU crop mismatch: {error}").into());
        }
        let image =
            photocraft_codecs::Image::from_raw(rect.width(), rect.height(), photocraft_codecs::ChannelLayout::Rgba, photocraft_codecs::SampleType::U8, rgba)?;
        let path = Path::new(output).with_extension(format!("{label}.png"));
        std::fs::write(&path, photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default())?)?;
        checks.push(
            json!({"region":label,"rect":[rect.x0,rect.y0,rect.x1,rect.y1],"max_error":error,"cpu_blake3":h.finalize().to_hex().to_string(),"preview":path}),
        );
    }
    let result = json!({"fixture":fixture,"fixture_blake3":blake3::hash(&bytes).to_hex().to_string(),
        "fixture_bytes":bytes.len(),"adapter":info.name,"cache":cache,"fresh":fresh,"reps":reps,
        "size":[doc.size.width,doc.size.height],"cold":cold,"samples":samples,
        "p50_total_ms":median(&samples,"total_ms"),"p50_import_ms":median(&samples,"import_ms"),
        "p50_admission_ms":median(&samples,"admission_ms"),"p50_encode_submit_ms":median(&samples,"encode_submit_ms"),
        "p50_wait_ms":median(&samples,"wait_ms"),"cpu_crop_blake3":cpu_hash.finalize().to_hex().to_string(),
        "gpu_cpu_max_error":max_error,"representative_checks":checks,"preview":preview_path});
    std::fs::write(output, serde_json::to_vec_pretty(&result)?)?;
    println!("RESULT {result}");
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
