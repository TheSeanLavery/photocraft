//! Real narrow history UI acceptance using original synthetic artwork; no server or user settings.
use egui::{Color32, vec2};
use egui_kittest::{Harness, kittest::Queryable};
use photocraft_color::PixelFormat;
use photocraft_doc::{ColorMode, Document, Layer, LayerContent, SampleType, Size, Surface};
use photocraft_engine::Session;
use photocraft_ui_egui::{PhotocraftApp, Services, history_panel, theme};
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = std::env::args().nth(1).ok_or("output PNG path required")?;
    let mut pixels = Surface::new(PixelFormat::RGBA8);
    for y in 0..160 {
        for x in 0..240 {
            let mountain = y > 110 - (x - 70i32).abs() / 2 || y > 130 - (x - 180i32).abs() / 2;
            let c = if y > 128 {
                [0.12, 0.30, 0.26, 1.0]
            } else if mountain {
                [0.24, 0.39, 0.41, 1.0]
            } else {
                [0.51, 0.69, 0.78, 1.0]
            };
            pixels.write_pixel(x, y, &c);
        }
    }
    let mut doc = Document::new("History landscape", Size::new(240, 160), ColorMode::Rgb, SampleType::U8);
    doc.layers.push(Layer::new("Landscape", LayerContent::Raster(pixels)));
    let mut session = Session::new();
    let landscape = doc.layers.first().ok_or("landscape")?.id;
    session.add_document(doc, None);
    session.edit("Brush paint", |doc, _| {
        let surface = doc.layers.first_mut().and_then(Layer::surface_mut).ok_or(photocraft_engine::EngineError::NoDocument)?;
        for y in 40..65 {
            for x in 173..198 {
                if (x - 185i32).pow(2) + (y - 52i32).pow(2) < 144 {
                    surface.write_pixel(x, y, &[1.0, 0.86, 0.48, 1.0]);
                }
            }
        }
        Ok(())
    })?;
    session.edit("Erase", |doc, _| {
        let surface = doc.layers.first_mut().and_then(Layer::surface_mut).ok_or(photocraft_engine::EngineError::NoDocument)?;
        for y in 132..148 {
            for x in 60..80 {
                surface.write_pixel(x, y, &[0.0, 0.0, 0.0, 0.0]);
            }
        }
        Ok(())
    })?;
    session.execute("edit.undo", json!({}))?;
    session.execute("layer.new.layer", json!({"name":"Light"}))?;
    session.execute("layer.select", json!({"layer":landscape.0}))?;
    session.execute("edit.transform", json!({"matrix":[1,0,0,1,24,0]}))?;
    if let Some(active) = session.active_mut() {
        active.history.set_current_label("Move layer");
    }
    session.execute("layer.setProps", json!({"opacity":0.7}))?;
    let app = PhotocraftApp::new(session, Services::default());
    let mut h = Harness::builder().with_size(vec2(310.0, 820.0)).wgpu().build_ui_state(
        |ui, app: &mut PhotocraftApp| {
            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(Color32::from_gray(36)).inner_margin(10)).show(ui, |ui| {
                ui.heading("History");
                ui.add_space(8.0);
                history_panel::show(app, ui);
            });
        },
        app,
    );
    PhotocraftApp::setup_context(&h.ctx, theme::ThemeKind::Pro);
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(2) {
        h.step();
        std::thread::sleep(Duration::from_millis(2));
    }
    h.render()?.save(&out)?;
    let current = h.state().session.active().ok_or("active document")?.doc.clone();
    let rows = h.state().session.active().ok_or("active history")?.history.timeline();
    if rows.len() != 6 {
        return Err(format!("expected six retained states, got {}", rows.len()).into());
    }
    h.get_by_label("Return to Original").click();
    h.run_steps(4);
    if h.state().session.active().ok_or("active document")?.history.current_id() != 0 {
        return Err("Original button did not restore root".into());
    }
    h.state_mut().run("history.jump", json!({"state":2}))?;
    if h.state().session.active().ok_or("active history")?.history.current_id() != 2 {
        return Err("Alternative erase branch unavailable".into());
    }
    h.state_mut().run("history.jump", json!({"state":5}))?;
    if !Arc::ptr_eq(&h.state().session.active().ok_or("active document")?.doc, &current) {
        return Err("Current state did not restore exactly".into());
    }
    println!("Accepted 310x820 scrubber: six thumbnails/actions, alternate branch, exact current and original restoration. {out}");
    Ok(())
}
