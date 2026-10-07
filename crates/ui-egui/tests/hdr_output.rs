//! Test the final canvas shader on a float framebuffer, rather than just its stored tiles.
use eframe::{egui_wgpu, wgpu};
use photocraft_color::{ColorMode, PixelFormat, SampleType};
use photocraft_doc::{Document, Layer, LayerContent, Size};
use photocraft_geom::Rect;
use photocraft_raster::Surface;
use photocraft_ui_egui::gpu_canvas::{GpuCanvas, ViewParams};

fn render(headroom: f32, preview: Option<[f32; 2]>, alpha: f32) -> Vec<[f32; 4]> {
    let mut rs = egui_kittest::wgpu::create_render_state(photocraft_ui_egui::gpu_canvas::wgpu_setup(), Default::default());
    rs.target_format = wgpu::TextureFormat::Rgba16Float;
    rs.output_color_space = wgpu::SurfaceColorSpace::ExtendedSrgb;
    rs.renderer = std::sync::Arc::new(epaint_lock(egui_wgpu::Renderer::new(&rs.device, rs.target_format, Default::default())));
    let g = GpuCanvas::new(&rs);
    let mut doc = Document::new("HDR steps", Size::new(160, 32), ColorMode::Rgb, SampleType::F32);
    doc.icc_profile = Some(photocraft_cms::Builtin::LinearSrgb.profile().to_bytes());
    let mut surface = Surface::new(PixelFormat::new(ColorMode::Rgb, SampleType::F32, true));
    for (i, value) in [0.0, 0.18, 1.0, 2.0, 4.0].into_iter().enumerate() {
        surface.fill_rect(Rect::new(i as i32 * 32, 0, i as i32 * 32 + 32, 32), &[value, value, value, alpha]);
    }
    doc.layers.push(Layer::new("steps", LayerContent::Raster(surface)));
    let mut color = photocraft_engine::color_cmds::ColorState::default();
    color.surface_srgb = true;
    let display = color.canvas_display(&doc).unwrap();
    g.upload_composite(doc.id.0, &doc, Some(&display));
    let ctx = egui::Context::default();
    let mut full =
        ctx.run_ui(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(160.0, 32.0))), ..Default::default() }, |ui| {
            let painter = ui.ctx().layer_painter(egui::LayerId::background());
            GpuCanvas::paint(
                &painter,
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(160.0, 32.0)),
                ViewParams {
                    doc: doc.id.0,
                    doc_size: [160, 32],
                    zoom: 1.0,
                    center: [80.0, 16.0],
                    shadow: false,
                    pixel_grid: false,
                    view_key: 1,
                    display: 0,
                    hdr: preview,
                    output_headroom: headroom,
                },
            );
        });
    let primitives = ctx.tessellate(full.shapes, 1.0);
    let screen = egui_wgpu::ScreenDescriptor { size_in_pixels: [160, 32], pixels_per_point: 1.0 };
    let texture = rs.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("HDR test framebuffer"),
        size: wgpu::Extent3d { width: 160, height: 32, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: rs.target_format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let buffer = rs.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 1280 * 32,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = rs.device.create_command_encoder(&Default::default());
    let mut renderer = rs.renderer.write();
    for (id, deltas) in full.textures_delta.set.drain() {
        for delta in deltas {
            renderer.update_texture(&rs.device, &rs.queue, id, &delta);
        }
    }
    full.textures_delta.clear();
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
        wgpu::TexelCopyBufferInfo { buffer: &buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(1280), rows_per_image: None } },
        texture.size(),
    );
    rs.queue.submit(command_buffers.into_iter().chain([encoder.finish()]));
    let (tx, rx) = std::sync::mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    rs.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(std::time::Duration::from_secs(30)) }).unwrap();
    rx.recv().unwrap().unwrap();
    let bytes = buffer.slice(..).get_mapped_range().unwrap();
    let values: Vec<[f32; 4]> = bytes
        .chunks_exact(8)
        .map(|p| {
            let mut rgba = [0.0; 4];
            for (v, b) in rgba.iter_mut().zip(p.chunks_exact(2)) {
                *v = photocraft_codecs::f16::from_bits(u16::from_le_bytes([b[0], b[1]])).to_f32();
            }
            rgba
        })
        .collect();
    drop(bytes);
    buffer.unmap();
    values
}

// The lock type is inferred from the renderer field without depending directly on epaint.
fn epaint_lock(value: egui_wgpu::Renderer) -> egui::mutex::RwLock<egui_wgpu::Renderer> {
    egui::mutex::RwLock::new(value)
}

#[test]
fn hdr_highlights_sdr_toggle_preview_and_transparency() {
    // Serialize devices within this test; missing GPUs fail rather than silently skip validation.
    for (headroom, preview, alpha, expected) in [(4.0, None, 1.0, 4.0), (1.0, None, 1.0, 1.0), (2.0, None, 1.0, 2.0), (4.0, Some([-1.0, 1.0]), 1.0, 2.0)] {
        let pixels = render(headroom, preview, alpha);
        let p = pixels[16 * 160 + 144];
        let linear = photocraft_color::convert::srgb_to_linear(p[0]);
        assert!((linear - expected).abs() < 0.025, "headroom={headroom}, preview={preview:?}: {p:?} -> {linear}");
        let white = photocraft_color::convert::srgb_to_linear(pixels[16 * 160 + 80][0]);
        assert!((white - if preview.is_some() { 0.5 } else { 1.0 }).abs() < 0.01);
    }
    let transparent = render(4.0, None, 0.0);
    assert!(transparent.iter().all(|p| p.iter().all(|v| v.is_finite() && *v <= 1.001)));
}

#[test]
fn hdr_view_control_is_serialized_and_rejects_bad_types() {
    use photocraft_ui_egui::control::{ControlRequest, Outcome, handle};
    let mut app = photocraft_ui_egui::PhotocraftApp::new(Default::default(), Default::default());
    let ctx = egui::Context::default();
    let (r, _) = ControlRequest::new("ui.set", serde_json::json!({"hdrOutput": false}));
    assert!(matches!(handle(&mut app, &ctx, &r), Outcome::Done(_)));
    assert!(app.ui.sdr_output);
    let item = photocraft_ui_egui::menus::menu_items(&app).into_iter().find(|i| i.id == "view.hdrOutput").unwrap();
    assert!(item.enabled, "HDR view command must be clickable");
    let (menu, _) = ControlRequest::new("ui.menu.invoke", serde_json::json!({"id": "view.hdrOutput"}));
    let Outcome::Done(v) = handle(&mut app, &ctx, &menu) else { panic!("expected menu reply") };
    assert_eq!(v["ok"], true);
    assert!(!app.ui.sdr_output);
    app.ui.sdr_output = true;
    let old: photocraft_ui_egui::state::UiState = serde_json::from_value(serde_json::to_value(&app.ui).unwrap()).unwrap();
    assert!(old.sdr_output);
    let (r, _) = ControlRequest::new("ui.set", serde_json::json!({"hdrOutput": "yes"}));
    let Outcome::Done(v) = handle(&mut app, &ctx, &r) else { panic!("expected reply") };
    assert_eq!(v["ok"], false);
}
