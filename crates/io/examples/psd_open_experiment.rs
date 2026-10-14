//! Fixed-file PSD import experiment. Fixture creation is deliberately separate from timings.
//! generate SOURCE.png OUT_DIR; bench FIXTURE.psd REPORT.json [REPS] [--full]
//! No timing assertions. Compare fixture, structure, raster and composite hashes across builds.
use photocraft_color::{BlendMode, ColorMode, PixelFormat, SampleType};
use photocraft_doc::{Adjustment, Document, Layer, LayerContent, LayerMask};
use photocraft_geom::{Rect, Size};
use photocraft_io::{ExportOptions, export, import, psd_to_document};
use photocraft_psd::PsdFile;
use photocraft_raster::Surface;
use serde_json::{Value, json};
use std::{error::Error, path::Path, time::Instant};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const WIDTH: u32 = 7360;
const HEIGHT: u32 = 4912;
fn hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}
fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}
fn png(path: &Path, buffer: &photocraft_compose::Buffer) -> Result<()> {
    let image = buffer.to_rgba8();
    let mut d = Document::new("Preview", Size::new(image.width, image.height), ColorMode::Rgb, SampleType::U8);
    let mut s = Surface::new(d.pixel_format());
    s.write_interleaved(d.bounds(), &image.pixels);
    d.layers.push(Layer::new("Composite", LayerContent::Raster(s)));
    std::fs::write(path, export(&d, "preview.png", &ExportOptions::default())?.bytes)?;
    Ok(())
}
fn surface_hash(s: &Surface) -> String {
    let mut h = blake3::Hasher::new();
    h.update(format!("{:?}|{:?}", s.format(), s.default_pixel()).as_bytes());
    for (coord, tile) in s.tiles() {
        h.update(format!("{coord:?}").as_bytes());
        h.update(tile.bytes());
    }
    h.finalize().to_hex().to_string()
}
fn layers(stack: &[Layer]) -> Vec<Value> {
    stack.iter().map(|l| {
        let content = match &l.content {
            LayerContent::Raster(s) => json!({"kind":"raster", "hash":surface_hash(s), "tiles":s.tile_count()}),
            LayerContent::Group(g) => json!({"kind":"group", "children":layers(&g.children)}),
            LayerContent::Adjustment(a) => json!({"kind":"adjustment", "parameters":format!("{a:?}")}),
            other => json!({"kind":"other", "parameters":format!("{other:?}")}),
        };
        json!({"name":l.name,"visible":l.visible,"blend":format!("{:?}",l.blend),"opacity":l.opacity,"fill_opacity":l.fill_opacity,"clipped":l.clipped,"content":content,
               "mask":l.mask.as_ref().map(|m| json!({"hash":surface_hash(&m.surface),"enabled":m.enabled,"linked":m.linked,"density":m.density,"feather":m.feather}))})
    }).collect()
}
fn signature(d: &Document) -> Value {
    json!({"size":[d.size.width,d.size.height],"mode":format!("{:?}",d.mode),"depth":format!("{:?}",d.depth),"layers":layers(&d.layers)})
}
fn composite_hash(buffer: &photocraft_compose::Buffer) -> String {
    let mut h = blake3::Hasher::new();
    for p in &buffer.px {
        for v in p {
            h.update(&v.to_bits().to_le_bytes());
        }
    }
    h.finalize().to_hex().to_string()
}
fn crop_rect(d: &Document) -> Rect {
    Rect::from_xywh((d.size.width / 2) as i32 - 640, (d.size.height / 2) as i32 - 426, 1280, 853)
}
// Nearest-neighbour sampling is explicit: generated detail is native 1536x1024, not 36 MP.
// Deterministic per-pixel alpha makes the layers nonuniform; row writes bound temporary memory.
fn textured(fmt: PixelFormat, source: &[[u8; 4]], sw: u32, sh: u32, rect: Rect, variation: u32, alpha: bool) -> Result<Surface> {
    let mut s = Surface::new(fmt);
    for y in rect.y0..rect.y1 {
        let mut row = Vec::with_capacity(rect.width() as usize * 4);
        for x in rect.x0..rect.x1 {
            let sx = ((u64::from(x.rem_euclid(WIDTH as i32) as u32) * u64::from(sw)) / u64::from(WIDTH)) as usize;
            let sy = ((u64::from(y.rem_euclid(HEIGHT as i32) as u32) * u64::from(sh)) / u64::from(HEIGHT)) as usize;
            let p = source.get(sy * sw as usize + sx).ok_or("source sample outside image")?;
            let offset = ((x.rem_euclid(257) as u32 * 13 + y.rem_euclid(251) as u32 * 7 + variation * 31) % 256) as u8;
            let a = if alpha { 48 + offset % 176 } else { 255 };
            row.extend_from_slice(&[p.first().copied().unwrap_or(0), p.get(1).copied().unwrap_or(0), p.get(2).copied().unwrap_or(0), a]);
        }
        s.write_interleaved(Rect::from_xywh(rect.x0, y, rect.width(), 1), &row);
    }
    Ok(s)
}
fn mask(sample: SampleType, rect: Rect) -> Surface {
    let mut s = Surface::with_default(PixelFormat::new(ColorMode::Grayscale, sample, false), &[0.0]);
    for y in rect.y0..rect.y1 {
        let row: Vec<f32> = (rect.x0..rect.x1).map(|x| ((x.rem_euclid(511) * 7 + y.rem_euclid(509) * 3) % 256) as f32 / 255.0).collect();
        s.write_region(Rect::from_xywh(rect.x0, y, rect.width(), 1), &row);
    }
    s
}
fn generate(source_path: &Path, out: &Path) -> Result<()> {
    std::fs::create_dir_all(out)?;
    let source_bytes = std::fs::read(source_path)?;
    let source_doc = import("source.png", &source_bytes)?.document;
    let source_buffer = photocraft_compose::render(&source_doc, source_doc.bounds());
    let source_pixels = source_buffer.to_rgba8();
    let samples = source_pixels.pixels.as_chunks::<4>().0;
    let (sw, sh) = (source_pixels.width, source_pixels.height);
    let fmt = PixelFormat::new(ColorMode::Rgb, SampleType::U8, true);
    let full = Rect::from_xywh(0, 0, WIDTH, HEIGHT);
    for complex in [false, true] {
        let name = if complex { "complex-8layer" } else { "p44-3layer" };
        let path = out.join(format!("{name}.psd"));
        if path.exists() {
            return Err(format!("fixture exists; refusing to replace frozen {}", path.display()).into());
        }
        let mut doc = Document::new(name, Size::new(WIDTH, HEIGHT), ColorMode::Rgb, SampleType::U8);
        doc.layers.push(Layer::new("Full source image", LayerContent::Raster(textured(fmt, samples, sw, sh, full, 0, false)?)));
        if !complex {
            for (i, blend) in [(1, BlendMode::Multiply), (2, BlendMode::Screen)] {
                let mut l = Layer::new(format!("Full textured overlay {i}"), LayerContent::Raster(textured(fmt, samples, sw, sh, full, i, true)?));
                l.blend = blend;
                l.opacity = 128.0 / 255.0;
                doc.layers.push(l);
            }
        } else {
            let mut group_children = Vec::new();
            for i in 0..6u32 {
                let rect = Rect::from_xywh(180 + (i * 913) as i32, 120 + (i * 617) as i32, 1600, 1100);
                let mut l = Layer::new(format!("Cropped source texture {i}"), LayerContent::Raster(textured(fmt, samples, sw, sh, rect, i + 1, true)?));
                l.blend = if i % 2 == 0 { BlendMode::Multiply } else { BlendMode::Screen };
                l.opacity = (96 + i * 19) as f32 / 255.0;
                if i == 1 || i == 4 {
                    l.mask = Some(LayerMask { surface: mask(SampleType::U8, rect), enabled: true, linked: true, density: 1.0, feather: 0.0 });
                }
                if i == 5 {
                    l.visible = false;
                }
                if i == 3 || i == 4 {
                    group_children.push(l);
                } else {
                    doc.layers.push(l);
                }
            }
            let mut group = Layer::group("Isolated screened detail", group_children);
            group.blend = BlendMode::Screen;
            group.opacity = 192.0 / 255.0;
            doc.layers.push(group);
            doc.layers.push(Layer::new(
                "Brightness contrast adjustment",
                LayerContent::Adjustment(Adjustment::BrightnessContrast { brightness: 4.0, contrast: 8.0, legacy: false }),
            ));
        }
        let crop_before = photocraft_compose::render(&doc, crop_rect(&doc));
        let output = export(&doc, "fixture.psd", &ExportOptions::default())?;
        std::fs::write(&path, &output.bytes)?;
        // First-roundtrip equality uses the CPU oracle, independent of import/export metadata.
        let imported = import("fixture.psd", &output.bytes)?;
        let crop_after = photocraft_compose::render(&imported.document, crop_rect(&imported.document));
        let max_diff = crop_before.px.iter().zip(&crop_after.px).flat_map(|(a, b)| a.iter().zip(b).map(|(x, y)| (x - y).abs())).fold(0.0f32, f32::max);
        if max_diff > 1.0 / 255.0 + 1e-5 {
            return Err(format!("fixture {name} CPU crop roundtrip difference {max_diff}").into());
        }
        png(&out.join(format!("{name}-crop.png")), &crop_after)?;
        let preview = photocraft_compose::render_reduced(&imported.document, 1472, 982);
        png(&out.join(format!("{name}-preview.png")), &preview)?;
        let metadata = json!({"fixture":path,"fixture_blake3":hash(&output.bytes),"fixture_bytes":output.bytes.len(),"source_path":source_path,"source_blake3":hash(&source_bytes),"source_native_dimensions":[sw,sh],"fixture_dimensions":[WIDTH,HEIGHT],"scaling":"nearest neighbour, once during fixture generation; generated source is not native36MP","generator":"psd_open_experiment generate","cpu_crop_roundtrip_max_diff":max_diff,"cpu_crop_hash":composite_hash(&crop_after),"preview_hash":composite_hash(&preview),"export_warnings":output.warnings,"import_warnings":imported.warnings,"structure":signature(&imported.document),"timing_contract":"All generation/export/source scaling excluded; imports read frozen same bytes."});
        std::fs::write(out.join(format!("{name}-fixture.json")), serde_json::to_vec_pretty(&metadata)?)?;
        println!("frozen {} ({} bytes)", path.display(), output.bytes.len());
    }
    Ok(())
}
fn bench(path: &Path, report: &Path, reps: usize, full: bool) -> Result<()> {
    if !(1..=20).contains(&reps) {
        return Err("reps must be1..20".into());
    }
    let bytes = std::fs::read(path)?;
    let warm = import("fixture.psd", &bytes)?;
    drop(warm); // warm code/cache; retained PSD bytes are identical.
    let mut runs = Vec::new();
    let mut reference = None;
    for _ in 0..reps {
        let t = Instant::now();
        let parsed = PsdFile::from_bytes(&bytes)?;
        let parse_ms = ms(t);
        let t = Instant::now();
        let (mapped, map_warnings) = psd_to_document(&parsed);
        let map_ms = ms(t);
        drop(parsed);
        let mapped_signature = signature(&mapped);
        drop(mapped);
        let total_start = Instant::now();
        let imported = import("fixture.psd", &bytes)?;
        let import_ms = ms(total_start);
        let t = Instant::now();
        let crop = photocraft_compose::render(&imported.document, crop_rect(&imported.document));
        let crop_ms = ms(t);
        let import_plus_crop_ms = ms(total_start);
        let sig = signature(&imported.document);
        if sig != mapped_signature {
            return Err("direct document mapping and public import differ".into());
        }
        let correctness = json!({"structure":sig,"cpu_crop_hash":composite_hash(&crop),"warnings":imported.warnings});
        if let Some(first) = &reference {
            if first != &correctness {
                return Err("fresh import correctness changed between repetitions".into());
            }
        } else {
            reference = Some(correctness);
        }
        let mut row = json!({"parse_ms":parse_ms,"document_mapping_ms":map_ms,"total_import_ms":import_ms,"cpu_crop_ms":crop_ms,"import_plus_cpu_crop_ms":import_plus_crop_ms,"mapping_warnings":map_warnings});
        if full {
            let t = Instant::now();
            let buffer = photocraft_compose::render(&imported.document, imported.document.bounds());
            let full_ms = ms(t);
            row["cpu_full_compose_ms"] = json!(full_ms);
            row["cpu_full_compose_hash"] = json!(composite_hash(&buffer));
        }
        println!("{}", row);
        runs.push(row);
    }
    let value = json!({"fixture":path,"fixture_blake3":hash(&bytes),"fixture_bytes":bytes.len(),"repetitions":reps,"warmup":1,"disk_read_in_timing":false,"parse_plus_map_note":"Separately timed operations, fresh document each time; total import measured independently.","p44_note":"3layer fixture matches P44 dimensions/layer count but uses real complex source and CPU crop; it does not include GPU refresh.","correctness":reference,"runs":runs});
    if let Some(parent) = report.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(report, serde_json::to_vec_pretty(&value)?)?;
    Ok(())
}
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("generate") => {
            generate(Path::new(args.get(1).ok_or("generate requires source PNG")?), Path::new(args.get(2).ok_or("generate requires output directory")?))
        }
        Some("bench") => bench(
            Path::new(args.get(1).ok_or("bench requires fixture PSD")?),
            Path::new(args.get(2).ok_or("bench requires report path")?),
            args.get(3).filter(|s| !s.starts_with("--")).map(|s| s.parse()).transpose()?.unwrap_or(3),
            args.iter().any(|s| s == "--full"),
        ),
        _ => Err("usage: generate SOURCE.png OUT_DIR | bench FIXTURE.psd REPORT.json [REPS] [--full]".into()),
    }
}
