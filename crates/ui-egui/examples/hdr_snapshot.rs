//! Full editor screenshots plus float EXR framebuffer captures on a controlled offscreen GPU.
//! This simulates headroom; it does not measure the connected display's brightness.
use eframe::{egui_wgpu, wgpu};
use photocraft_ui_egui::control::{ControlRequest, Outcome, handle};
use photocraft_ui_egui::{PhotocraftApp, Services};
use serde_json::json;
fn main() {
    if let Err(e) = run() {
        eprintln!("HDR snapshot: {e}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |key: &str| args.iter().position(|a| a == key).and_then(|i| args.get(i + 1)).cloned();
    let path = arg("--open").ok_or("--open EXR required")?;
    let out = arg("--out").ok_or("--out prefix required")?;
    let mode = arg("--mode").unwrap_or_else(|| "hdr".into());
    let mut rs = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        egui_kittest::wgpu::create_render_state(photocraft_ui_egui::gpu_canvas::wgpu_setup(), Default::default())
    }))
    .map_err(|_| "no GPU adapter")?;
    if !photocraft_ui_egui::gpu_canvas::supports_f16_canvas(&rs.adapter) {
        return Err("float GPU output unavailable".into());
    }
    rs.target_format = wgpu::TextureFormat::Rgba16Float;
    rs.output_color_space = wgpu::SurfaceColorSpace::ExtendedSrgb;
    rs.renderer = std::sync::Arc::new(egui::mutex::RwLock::new(egui_wgpu::Renderer::new(&rs.device, rs.target_format, Default::default())));
    rs.display_hdr_info.write().headroom = Some(wgpu::DisplayHeadroom { current: Some(4.0), ..Default::default() });
    let renderer = egui_kittest::wgpu::WgpuTestRenderer::from_render_state(rs.clone());
    let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(1440.0, 900.0)).with_pixels_per_point(1.0).renderer(renderer).build_eframe(|cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        let services = Services {
            import: Some(Box::new(|name, bytes| photocraft_io::import(name, bytes).map(|r| (r.document, r.warnings)).map_err(|e| e.to_string()))),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(Default::default(), services);
        app.set_wgpu(rs.clone());
        app.session.color.surface_srgb = true;
        app
    });
    harness.state_mut().open_path(&path)?;
    harness.state_mut().ui.hdr_workbench_open = true;
    harness.state_mut().ui.sdr_output = mode != "hdr" && mode != "split";
    harness.state_mut().ui.status = "Offscreen validation · simulated 4× headroom".into();
    harness.state_mut().session.execute(
        "view.hdrWorkbench",
        json!({"method":if mode=="clip"{"clip"}else if mode=="filmic"{"filmic"}else{"natural"},"split":mode=="split","clipping":mode=="clipping"}),
    )?;
    harness.run_steps(4);
    if args.iter().any(|a| a == "--picker") {
        let app = harness.state_mut();
        app.session.execute("tools.setColors", json!({"foreground":[4.0,2.0,1.0,1.0]}))?;
        let ctx = harness.ctx.clone();
        let (request, _) = ControlRequest::new("ui.dialog.open", json!({"kind":"colorPicker","target":"foreground"}));
        let result = handle(harness.state_mut(), &ctx, &request);
        if let Outcome::Done(v) = result
            && v["ok"] == false
        {
            return Err(v.to_string().into());
        }
    }
    harness.run_steps(12);
    let values = render(&rs, &harness.ctx, harness.output())?;
    let peak = values.iter().flat_map(|p| p[..3].iter().copied()).fold(0.0f32, f32::max);
    let encoded: Vec<f32> = values.iter().flatten().copied().collect();
    let linear: Vec<f32> = values
        .iter()
        .flat_map(|p| {
            [
                photocraft_color::convert::srgb_to_linear(p[0]),
                photocraft_color::convert::srgb_to_linear(p[1]),
                photocraft_color::convert::srgb_to_linear(p[2]),
                p[3],
            ]
        })
        .collect();
    let png = photocraft_codecs::Image::from_normalized(1440, 900, photocraft_codecs::ChannelLayout::Rgba, photocraft_codecs::SampleType::U8, &encoded)?;
    let exr = photocraft_codecs::Image::from_f32(1440, 900, photocraft_codecs::ChannelLayout::Rgba, &linear)?;
    std::fs::write(format!("{out}.png"), photocraft_codecs::encode(&png, photocraft_codecs::Format::Png, &Default::default())?)?;
    std::fs::write(format!("{out}.exr"), photocraft_codecs::encode(&exr, photocraft_codecs::Format::OpenExr, &Default::default())?)?;
    println!("mode={mode}, controlled_headroom=4, peak_linear={:.5}", photocraft_color::convert::srgb_to_linear(peak));
    Ok(())
}
fn render(rs: &egui_wgpu::RenderState, ctx: &egui::Context, full: &egui::FullOutput) -> Result<Vec<[f32; 4]>, Box<dyn std::error::Error>> {
    let primitives = ctx.tessellate(full.shapes.clone(), 1.0);
    let screen = egui_wgpu::ScreenDescriptor { size_in_pixels: [1440, 900], pixels_per_point: 1.0 };
    let texture = rs.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("HDR test framebuffer"),
        size: wgpu::Extent3d { width: 1440, height: 900, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: rs.target_format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let buffer = rs.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 11520 * 900,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = rs.device.create_command_encoder(&Default::default());
    let mut renderer = rs.renderer.write();
    let command_buffers = renderer.update_buffers(&rs.device, &rs.queue, &mut encoder, &primitives, &screen);
    let view = texture.create_view(&Default::default());
    {
        let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        renderer.render(&mut pass.forget_lifetime(), &primitives, &screen);
    }
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo { buffer: &buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(11520), rows_per_image: None } },
        texture.size(),
    );
    rs.queue.submit(command_buffers.into_iter().chain([encoder.finish()]));
    let (tx, rx) = std::sync::mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    rs.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(std::time::Duration::from_secs(30)) })?;
    rx.recv()??;
    let bytes = buffer.slice(..).get_mapped_range()?;
    let values: Vec<[f32; 4]> = bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|p| {
            let mut rgba = [0.0; 4];
            for (v, b) in rgba.iter_mut().zip(p.as_chunks::<2>().0.iter()) {
                *v = photocraft_codecs::f16::from_bits(u16::from_le_bytes([b[0], b[1]])).to_f32();
            }
            rgba
        })
        .collect();
    drop(bytes);
    buffer.unmap();
    Ok(values)
}
