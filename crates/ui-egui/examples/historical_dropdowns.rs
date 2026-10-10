//! Offscreen compact dropdown gallery; leaves the user's app untouched.
use egui::vec2;
use egui_kittest::Harness;
use photocraft_ui_egui::{PhotocraftApp, theme, theme_pack::ThemePack, widgets};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let slug = args.get(1).map(String::as_str).unwrap_or("system-6");
    let scale = args.get(2).and_then(|s| s.parse::<f32>().ok()).unwrap_or(1.0);
    let out = args.get(3).map(String::as_str).unwrap_or("historical-dropdowns.png");
    let text = match slug {
        "windows-98-se" => include_str!("../../../assets/themes/windows-98-se.json"),
        "mac-os-9-1" => include_str!("../../../assets/themes/mac-os-9-1.json"),
        "system-6" => include_str!("../../../assets/themes/system-6.json"),
        _ => return Err("unknown historical theme".into()),
    };
    let pack = ThemePack::parse(text)?.resolve()?;
    let mut initialized = false;
    let mut value = 0;
    let mut h = Harness::builder().with_size(vec2(320.0, 180.0)).with_pixels_per_point(scale).wgpu().build_ui(|ui| {
        if !initialized {
            PhotocraftApp::setup_context(ui.ctx(), theme::ThemeKind::Classic);
            initialized = true;
            return;
        }
        let t = theme::Tokens::get(ui.ctx());
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.chrome).inner_margin(12)).show(ui, |ui| {
            ui.label(slug);
            ui.label("Long preset in a narrow row");
            ui.horizontal(|ui| {
                widgets::dropdown(ui, "long", &mut value, &[(0, "A very long color lookup preset with a translated description"), (1, "Other")], 120.0);
                ui.label("Following control");
            });
            ui.label("Long multilingual label");
            widgets::dropdown(ui, "translated", &mut value, &[(0, "非常に長いカラー設定の説明 / Sehr lange Farbvorgabe"), (1, "Other")], 180.0);
            ui.label("Short label keeps its geometry");
            widgets::dropdown(ui, "short", &mut value, &[(0, "Linear"), (1, "Other")], 120.0);
        });
    });
    pack.apply(&h.ctx);
    h.run_steps(8);
    h.render()?.save(out)?;
    Ok(())
}
