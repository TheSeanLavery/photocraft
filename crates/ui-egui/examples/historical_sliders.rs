//! Offscreen slider-state gallery; no user app, documents or settings are touched.
use egui::{Color32, vec2};
use egui_kittest::Harness;
use photocraft_ui_egui::{PhotocraftApp, theme, theme_pack::ThemePack, widgets};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let slug = args.get(1).map(String::as_str).unwrap_or("system-6");
    let scale = args.get(2).and_then(|s| s.parse::<f32>().ok()).unwrap_or(1.0);
    let state = args.get(3).map(String::as_str).unwrap_or("normal");
    let out = args.get(4).map(String::as_str).unwrap_or("historical-sliders.png");
    let text = match slug {
        "windows-98-se" => include_str!("../../../assets/themes/windows-98-se.json"),
        "mac-os-9-1" => include_str!("../../../assets/themes/mac-os-9-1.json"),
        "system-6" => include_str!("../../../assets/themes/system-6.json"),
        _ => return Err("unknown historical theme".into()),
    };
    let pack = ThemePack::parse(text)?.resolve()?;
    let mut initialized = false;
    let mut value = 50.0;
    let mut gradient = 50.0;
    let mut low = 0.0;
    let mut high = 100.0;
    let mut disabled = 50.0;
    let active_id = egui::Id::new("slider-gallery-active-rect");
    let mut h = Harness::builder().with_size(vec2(440.0, 320.0)).with_pixels_per_point(scale).wgpu().build_ui(|ui| {
        if !initialized {
            PhotocraftApp::setup_context(ui.ctx(), theme::ThemeKind::Classic);
            initialized = true;
            return;
        }
        let t = theme::Tokens::get(ui.ctx());
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.chrome).inner_margin(16)).show(ui, |ui| {
            ui.label(format!("{slug}: {state}"));
            ui.add_space(10.0);
            ui.label("Setting");
            ui.add_enabled_ui(state != "disabled", |ui| {
                let response = widgets::slider(ui, &mut value, 0.0..=100.0, None);
                ui.data_mut(|d| d.insert_temp(active_id, response.rect));
                if state == "focus" {
                    response.request_focus();
                }
            });
            ui.label("Edited color range");
            widgets::slider(ui, &mut gradient, 0.0..=100.0, Some(&[Color32::RED, Color32::GREEN, Color32::BLUE]));
            ui.label("Minimum");
            widgets::slider(ui, &mut low, 0.0..=100.0, None);
            ui.label("Maximum");
            widgets::slider(ui, &mut high, 0.0..=100.0, None);
            ui.label("Disabled");
            ui.add_enabled_ui(false, |ui| {
                widgets::slider(ui, &mut disabled, 0.0..=100.0, None);
            });
        });
    });
    pack.apply(&h.ctx);
    h.run_steps(8);
    let rect = h.ctx.data(|d| d.get_temp::<egui::Rect>(active_id)).ok_or("missing slider rect")?;
    let at = rect.center();
    match state {
        "normal" | "focus" | "disabled" => {}
        "hover" | "pressed" => {
            h.event(egui::Event::PointerMoved(at));
            h.run_steps(2);
            if state == "pressed" {
                h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
                h.run_steps(2);
            }
        }
        _ => return Err("unknown slider state".into()),
    }
    h.render()?.save(out)?;
    println!("wrote {out}");
    Ok(())
}
