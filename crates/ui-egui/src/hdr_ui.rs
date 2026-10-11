//! A thin shell for HDR commands. Histogram work is bounded and cached per document revision.
use crate::PhotocraftApp;
use crate::theme::Tokens;
use egui::{Rect, Sense, Stroke, pos2, vec2};
use photocraft_color::hdr::{HdrWorkbench, ToneMap};
use serde_json::{Value, json};

pub fn export(app: &mut PhotocraftApp) -> Result<Value, String> {
    let pick = app.services.pick_save.as_mut().ok_or("no save picker configured")?;
    let Some(path) = pick("SDR-preview.png") else { return Ok(Value::Null) };
    let bytes = photocraft_engine::hdr_cmds::sdr_png(&app.session).map_err(|e| e.to_string())?;
    app.services.write.as_mut().ok_or("no writer configured")?(&path, &bytes)?;
    Ok(json!({"path":path,"bytes":bytes.len(),"colorSpace":"sRGB"}))
}

/// Genuine float swatch on the GPU; SDR fallback is explicitly a clipped approximation.
/// The document's composite RGB encoding and ICC profile are used, as by paint/eyedropper.
pub fn swatch(app: &PhotocraftApp, ui: &mut egui::Ui, rect: Rect, color: [f32; 4], key: u64) {
    if let (Some(g), Some(st)) = (&app.gpu, app.session.active())
        && g.format_for(photocraft_color::SampleType::F32, [1, 1]) == eframe::wgpu::TextureFormat::Rgba16Float
    {
        let b = photocraft_compose::Buffer::filled(photocraft_geom::Rect::new(0, 0, 1, 1), color);
        if let Ok(display) = app.session.color.canvas_display(&st.doc) {
            let signature = egui::Id::new((app.session.color.display_signature(&st.doc), color.map(f32::to_bits))).value();
            let mode = if let Some((_, mode)) = g.display_lut_signature(key, 0).filter(|(saved, _)| *saved == signature) {
                mode
            } else {
                let b = display.texture_buffer(&b);
                g.upload_buffer_full(key, &b, photocraft_color::SampleType::F32);
                let mode = match app.session.color.gpu_canvas_lut(&st.doc, 33) {
                    Ok(Some(lut)) => {
                        g.set_display_lut(key, 0, 33, Some(&lut));
                        1
                    }
                    _ => {
                        g.set_display_lut(key, 0, 33, None);
                        0
                    }
                };
                g.cache_display_lut_signature(key, 0, signature, mode);
                mode
            };
            crate::gpu_canvas::GpuCanvas::paint(
                &ui.painter().with_clip_rect(rect.intersect(ui.clip_rect())),
                rect,
                crate::gpu_canvas::ViewParams {
                    doc: key,
                    output: 0,
                    doc_size: [1, 1],
                    zoom: rect.width().max(rect.height()),
                    center: [0.5, 0.5],
                    shadow: false,
                    pixel_grid: false,
                    view_key: key,
                    display: mode,
                    hdr: None,
                    output_headroom: if app.ui.sdr_output { 1.0 } else { g.output_headroom() },
                    workbench: None,
                },
            );
            return;
        }
    }
    ui.painter().rect_filled(
        rect,
        0.0,
        egui::Color32::from_rgb((color[0].clamp(0.0, 1.0) * 255.0) as u8, (color[1].clamp(0.0, 1.0) * 255.0) as u8, (color[2].clamp(0.0, 1.0) * 255.0) as u8),
    );
}

pub fn show(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if !app.ui.hdr_workbench_open {
        return;
    }
    let mut open = true;
    egui::Window::new("HDR Workbench")
        .id(egui::Id::new("hdr-workbench"))
        .open(&mut open)
        .default_width(360.0)
        .default_height(790.0)
        .default_pos(pos2(1040.0, 80.0))
        .vscroll(true)
        .resizable(false)
        .show(ctx, |ui| {
            let t = Tokens::get(ctx);
            let Some(st) = app.session.active() else {
                ui.label("Open an image to inspect HDR.");
                return;
            };
            let doc = st.doc.clone();
            let revision = st.revision;
            let mut w = photocraft_engine::hdr_cmds::settings(&app.session, &doc);
            let original = w;
            let headroom = app.gpu.as_ref().map_or(1.0, |g| g.output_headroom().max(1.0));
            if cfg!(target_arch = "wasm32") && headroom > 1.0 {
                ui.label(format!("Browser-managed HDR · preview ceiling {headroom:.0}× white"));
            } else {
                ui.label(format!("Display: {:.2}× white · +{:.2} EV", headroom, headroom.log2()));
            }
            ui.weak(if app.gpu.is_some() { "Float document → ICC → display transform" } else { "CPU SDR preview · HDR presentation unavailable" });
            if doc.depth != photocraft_color::SampleType::F32 {
                ui.weak("HDR canvas controls require a 32-bit document.");
            }
            let mut hdr = !app.ui.sdr_output;
            if ui.checkbox(&mut hdr, "HDR Output").changed() {
                app.ui.sdr_output = !hdr;
            }
            if photocraft_engine::proof_sim::hdr_active(&app.session.color, &doc) {
                ui.weak("Legacy 32-bit Preview is active. Reset it to match SDR export.");
                if ui.button("Reset legacy 32-bit preview").clicked() {
                    let _ = app.run("view.thirtyTwoBitPreviewOptions", json!({"method":"exposureGamma","exposure":0,"gamma":1}));
                }
            }
            ui.separator();
            ui.label("SDR tone mapping");
            ui.horizontal(|ui| {
                for (method, label) in [(ToneMap::Natural, "Natural"), (ToneMap::Filmic, "Filmic"), (ToneMap::Clip, "Clip")] {
                    ui.selectable_value(&mut w.method, method, label);
                }
            });
            ui.weak(match w.method {
                ToneMap::Natural => "Neutral midtones, smooth highlight shoulder",
                ToneMap::Filmic => "Soft toe with a film-like highlight shoulder",
                ToneMap::Clip => "Diagnostic: highlights above white are clipped",
            });
            ui.add(egui::Slider::new(&mut w.exposure, -20.0..=20.0).text("Exposure (EV)"));
            ui.add(egui::Slider::new(&mut w.shoulder, 0.1..=0.95).text("Shoulder start"));
            ui.add(egui::Slider::new(&mut w.contrast, 0.25..=2.0).text("Contrast"));
            ui.add(egui::Slider::new(&mut w.saturation, 0.0..=2.0).text("Saturation"));
            ui.checkbox(&mut w.split, "Split preview · HDR left / SDR right");
            ui.checkbox(&mut w.clipping, "Highlight clipping stripes");
            if ui.button("Reset display controls").clicked() {
                w = HdrWorkbench::default();
            }
            if w != original
                && let Ok(p) = serde_json::to_value(w)
            {
                let _ = app.run("view.hdrWorkbench", p);
            }
            ui.separator();
            ui.label("HDR histogram · peak channel · linear sRGB");
            let cache_id = egui::Id::new(("hdr-histogram", doc.id.0));
            let signature = (revision, doc.icc_profile.clone());
            let cached = ctx.data(|d| d.get_temp::<((u64, Option<std::sync::Arc<Vec<u8>>>), Value)>(cache_id));
            let histogram = match cached {
                Some((key, v)) if key == signature => Some(v),
                _ => match photocraft_engine::hdr_cmds::diagnostics(&doc) {
                    Ok(v) => {
                        ctx.data_mut(|d| d.insert_temp(cache_id, (signature, v.clone())));
                        Some(v)
                    }
                    Err(e) => {
                        ui.colored_label(t.accent, e.to_string());
                        None
                    }
                },
            };
            if let Some(h) = histogram {
                let bins: Vec<u64> = h.get("bins").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default();
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 84.0), Sense::hover());
                ui.painter().rect_filled(rect, 0.0, t.field);
                let max = bins.iter().copied().max().unwrap_or(1).max(1) as f32;
                for (i, count) in bins.iter().enumerate() {
                    let x = rect.left() + i as f32 / 128.0 * rect.width();
                    let height = (*count as f32 + 1.0).ln() / (max + 1.0).ln() * rect.height();
                    ui.painter()
                        .line_segment([pos2(x, rect.bottom()), pos2(x, rect.bottom() - height)], Stroke::new(2.0, if i < 64 { t.text_dim } else { t.accent }));
                }
                for (ev, label) in [(0.0, "white"), (headroom.log2(), "display")] {
                    let x = rect.left() + (ev + 8.0) / 16.0 * rect.width();
                    ui.painter().line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.0, t.text));
                    ui.painter().text(pos2(x + 3.0, rect.top()), egui::Align2::LEFT_TOP, label, egui::FontId::proportional(10.0), t.text);
                }
                ui.horizontal(|ui| {
                    ui.weak("−8 EV");
                    ui.add_space(90.0);
                    ui.weak("0 EV");
                    ui.add_space(80.0);
                    ui.weak("+8 EV");
                });
                ui.weak(format!("Proxy peak: {:.3}× white · sampled, not a full-resolution clipping count", h["peak"].as_f64().unwrap_or(0.0)));
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("HDR color picker…").clicked() {
                    crate::color_picker_ui::open(app, "foreground");
                }
                if ui.button("Save float swatch").clicked() {
                    let _ = app.run("tools.hdrSwatches", json!({"action":"save"}));
                }
            });
            let colors = photocraft_engine::hdr_cmds::swatch_colors(&app.session, &doc).unwrap_or_default();
            ui.horizontal_wrapped(|ui| {
                for (i, color) in colors.into_iter().enumerate() {
                    let (rect, response) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::click());
                    swatch(app, ui, rect, color, egui::Id::new(("hdr-saved-swatch", i)).value());
                    if response.on_hover_text(format!("Float RGB {:?}", color)).clicked() {
                        let _ = app.run("tools.hdrSwatches", json!({"action":"recall","index":i}));
                    }
                }
            });
            if ui.button("Clear saved HDR swatches").clicked() {
                let _ = app.run("tools.hdrSwatches", json!({"action":"clear"}));
            }
            if ui.button("Export matching SDR PNG…").clicked()
                && let Err(e) = export(app)
            {
                app.ui.status = e;
                app.ui.status_error = true;
            }
            ui.weak("View settings leave the document unchanged. Export bakes the SDR transform into an sRGB PNG.");
        });
    app.ui.hdr_workbench_open = open;
}

/// Shared display for float filter-dialog images. Pixel upload is cached by revision;
/// exposure, ICC handling and tone mapping match the canvas, including its CPU fallback.
pub(crate) struct PreviewDisplay<'a> {
    pub session: &'a photocraft_engine::Session,
    pub gpu: Option<&'a crate::gpu_canvas::GpuCanvas>,
    pub sdr: bool,
}
pub(crate) const FILTER_PREVIEW_KEYS: [&str; 5] = ["hdr-camera-before", "hdr-camera-after", "hdr-camera-detail", "hdr-gallery", "hdr-liquify"];
impl PreviewDisplay<'_> {
    pub fn paint(&self, painter: &egui::Painter, rect: Rect, pixels: &[[f32; 4]], size: [usize; 2], stamp: u64, name: &str) -> bool {
        let Some(st) = self.session.active().filter(|s| s.doc.depth == photocraft_color::SampleType::F32) else { return false };
        let [w, h] = size;
        if w == 0 || h == 0 || w.checked_mul(h) != Some(pixels.len()) {
            return false;
        }
        let Ok(w32) = u32::try_from(w) else { return false };
        let Ok(h32) = u32::try_from(h) else { return false };
        let Ok(display) = self.session.color.canvas_display(&st.doc) else { return false };
        let key = egui::Id::new(name).value();
        let signature = egui::Id::new((st.doc.id.0, st.revision, stamp, size, self.session.color.display_signature(&st.doc))).value();
        let settings = (!self.session.color.proof(st.doc.id).enabled).then(|| photocraft_engine::hdr_cmds::settings(self.session, &st.doc));
        let legacy = self.session.color.hdr_preview(&st.doc);
        let buffer = || photocraft_compose::Buffer { rect: photocraft_geom::Rect::from_xywh(0, 0, w32, h32), px: pixels.to_vec() };
        if let Some(g) = self.gpu.filter(|g| g.format_for(st.doc.depth, [w32, h32]) == eframe::wgpu::TextureFormat::Rgba16Float) {
            let mode = if let Some((_, mode)) = g.display_lut_signature(key, 0).filter(|(s, _)| *s == signature && g.has(key, [w32, h32])) {
                mode
            } else {
                g.upload_buffer_full(key, &display.texture_buffer(&buffer()), st.doc.depth);
                let mode = match self.session.color.gpu_canvas_lut(&st.doc, 33) {
                    Ok(Some(lut)) => {
                        g.set_display_lut(key, 0, 33, Some(&lut));
                        if self.session.color.proof(st.doc.id).gamut_warning { 2 } else { 1 }
                    }
                    _ => {
                        g.set_display_lut(key, 0, 33, None);
                        0
                    }
                };
                g.cache_display_lut_signature(key, 0, signature, mode);
                mode
            };
            crate::gpu_canvas::GpuCanvas::paint(
                painter,
                rect,
                crate::gpu_canvas::ViewParams {
                    doc: key,
                    output: 0,
                    doc_size: [w32, h32],
                    zoom: (rect.width() / w as f32).min(rect.height() / h as f32),
                    center: [w as f32 / 2.0, h as f32 / 2.0],
                    shadow: false,
                    pixel_grid: false,
                    view_key: key,
                    display: mode,
                    hdr: legacy.map(|h| [h.exposure, h.gamma]),
                    workbench: settings,
                    output_headroom: if !g.hdr_surface() {
                        0.0
                    } else if self.sdr || settings.is_none() {
                        1.0
                    } else {
                        g.output_headroom()
                    },
                },
            );
        } else {
            let settings_key = egui::Id::new(format!("{settings:?}{legacy:?}")).value();
            let id = egui::Id::new((name, "cpu"));
            let cached = painter.ctx().data(|d| d.get_temp::<(u64, egui::TextureHandle)>(id)).filter(|(s, _)| *s == signature ^ settings_key);
            let texture = if let Some((_, tex)) = cached {
                tex
            } else {
                let mut b = buffer();
                if let Some(settings) = settings {
                    if let Ok(shown) = photocraft_engine::hdr_cmds::preview_buffer(&st.doc, &b, settings, legacy) {
                        b = shown;
                    }
                    let _ = photocraft_engine::hdr_cmds::to_monitor(&mut b, Some(&display));
                    let image = b.to_rgba8();
                    painter.ctx().load_texture(name, egui::ColorImage::from_rgba_unmultiplied(size, &image.pixels), egui::TextureOptions::LINEAR)
                } else {
                    let image = display.to_rgba8(&b);
                    painter.ctx().load_texture(name, egui::ColorImage::from_rgba_unmultiplied(size, &image.pixels), egui::TextureOptions::LINEAR)
                }
            };
            painter.ctx().data_mut(|d| d.insert_temp(id, (signature ^ settings_key, texture.clone())));
            painter.image(texture.id(), rect, Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
        }
        true
    }
}

/// Bounded swatch resources; unused keys allocate nothing.
pub(crate) fn gpu_keys() -> impl Iterator<Item = u64> {
    [egui::Id::new("hdr-picker-new").value(), egui::Id::new("hdr-picker-old").value()]
        .into_iter()
        .chain((0..32usize).map(|i| egui::Id::new(("hdr-saved-swatch", i)).value()))
        .chain(FILTER_PREVIEW_KEYS.into_iter().map(|s| egui::Id::new(s).value()))
        .chain(photocraft_algo::GalleryFilter::ALL.iter().map(|f| egui::Id::new(format!("hdr-gallery-thumb-{}", f.key())).value()))
}

#[cfg(test)]
mod preview_tests {
    use super::*;
    #[test]
    fn float_filter_cpu_fallback_tone_maps_before_quantization_and_caches() {
        let ctx = egui::Context::default();
        let mut session = photocraft_engine::Session::new();
        session.add_document(
            photocraft_doc::Document::new("HDR", photocraft_doc::Size::new(1, 1), photocraft_color::ColorMode::Rgb, photocraft_color::SampleType::F32),
            None,
        );
        let draw = |session: &photocraft_engine::Session| {
            ctx.run_ui(Default::default(), |ui| {
                let display = PreviewDisplay { session, gpu: None, sdr: true };
                assert!(display.paint(
                    ui.painter(),
                    Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(16.0, 16.0)),
                    &[[2.0, 4.0, 8.0, 1.0]],
                    [1, 1],
                    1,
                    "hdr-gallery"
                ));
            })
        };
        let mut out = draw(&session);
        let id = egui::Id::new(("hdr-gallery", "cpu"));
        let (_, tex) = ctx.data(|d| d.get_temp::<(u64, egui::TextureHandle)>(id)).unwrap();
        let delta = out.textures_delta.set.get(&tex.id()).unwrap().first().unwrap();
        let egui::ImageData::Color(image) = &delta.image;
        let color = image.pixels[0];
        assert!(color.r() < color.g() && color.g() < color.b(), "HDR must retain chromaticity instead of turning white: {color:?}");
        out.textures_delta.clear();
        let mut unchanged = draw(&session);
        assert!(!unchanged.textures_delta.set.contains_key(&tex.id()));
        unchanged.textures_delta.clear();
        session.execute("view.hdrWorkbench", json!({"exposure":-2.0})).unwrap();
        let mut changed = draw(&session);
        changed.textures_delta.clear();
        let (_, next) = ctx.data(|d| d.get_temp::<(u64, egui::TextureHandle)>(id)).unwrap();
        assert_ne!(next.id(), tex.id(), "changed HDR controls must invalidate the CPU display texture");
        drop(tex);
        drop(next);
        ctx.data_mut(|d| {
            d.remove::<(u64, egui::TextureHandle)>(id);
        });
        let mut cleanup = ctx.run_ui(Default::default(), |_| {});
        cleanup.textures_delta.clear();
    }
}
