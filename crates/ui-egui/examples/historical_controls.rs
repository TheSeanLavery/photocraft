//! Offscreen historical control gallery. No user settings, documents or windows are touched.
use egui::{Color32, vec2};
use egui_kittest::{Harness, kittest::Queryable};
use photocraft_ui_egui::{PhotocraftApp, theme, theme_pack::ThemePack, widgets};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let slug = args.get(1).map(String::as_str).unwrap_or("windows-98-se");
    let scale = args.get(2).and_then(|s| s.parse::<f32>().ok()).unwrap_or(1.0);
    let state = args.get(3).map(String::as_str).unwrap_or("normal");
    let out = args.get(4).map(String::as_str).unwrap_or("historical-controls.png");
    let text = match slug {
        "windows-98-se" => include_str!("../../../assets/themes/windows-98-se.json"),
        "mac-os-9-1" => include_str!("../../../assets/themes/mac-os-9-1.json"),
        "system-6" => include_str!("../../../assets/themes/system-6.json"),
        _ => return Err("unknown historical theme".into()),
    };
    let pack = ThemePack::parse(text)?.resolve()?;
    let mut initialized = false;
    let mut selected_tab = 0;
    let mut radio = 0;
    let mut checked = true;
    let mut unchecked = false;
    let mut number = 50.0;
    let mut choice = 0;
    let mut h = Harness::builder().with_size(vec2(740.0, 600.0)).with_pixels_per_point(scale).wgpu().build_ui(|ui| {
        if !initialized {
            PhotocraftApp::setup_context(ui.ctx(), theme::ThemeKind::Classic);
            initialized = true;
            return;
        }
        let t = theme::Tokens::get(ui.ctx());
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.chrome).inner_margin(20)).show(ui, |ui| {
            ui.heading(format!("PhotoCraft — {slug} control gallery"));
            ui.label("Original app-drawn controls; installed-font substitutions may apply.");
            ui.add_space(16.0);
            widgets::card(ui, "gallery-tabs", &["General", "Options", "Advanced"], &mut selected_tab, |ui, _| {
                ui.horizontal(|ui| {
                    widgets::primary_button(ui, "Default action", 120.0);
                    widgets::secondary_button(ui, "Other action", 110.0);
                    ui.add_enabled_ui(false, |ui| {
                        widgets::secondary_button(ui, "Disabled action", 110.0);
                    });
                });
                ui.add_space(12.0);
                widgets::checkbox(ui, &mut checked, "Checked option");
                widgets::checkbox(ui, &mut unchecked, "Unchecked option");
                ui.add_enabled_ui(false, |ui| {
                    let mut on = true;
                    widgets::checkbox(ui, &mut on, "Disabled option");
                });
                ui.add_space(6.0);
                widgets::radio_value(ui, &mut radio, 0, "First choice");
                widgets::radio_value(ui, &mut radio, 1, "Second choice");
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    widgets::value_field(ui, &mut number, 0.0..=100.0, "%", 80.0);
                    widgets::dropdown(ui, "gallery-choice", &mut choice, &[(0, "Normal"), (1, "Multiply"), (2, "Screen")], 160.0);
                });
                ui.label(egui::RichText::new("Document colors remain data: ").color(t.text));
                ui.horizontal(|ui| {
                    for color in [Color32::RED, Color32::GREEN, Color32::BLUE] {
                        let (r, _) = ui.allocate_exact_size(vec2(20.0, 20.0), egui::Sense::hover());
                        ui.painter().rect_filled(r, 0.0, color);
                    }
                });
            });
            ui.add_space(12.0);
            ui.label("Scrollable list");
            photocraft_ui_egui::scroll_area::ScrollArea::both().id_salt("gallery-scrollbars").max_height(150.0).auto_shrink([false, false]).show(ui, |ui| {
                ui.set_min_size(vec2(1100.0, 700.0));
                for i in 0..35 {
                    ui.label(format!("Row {i:02} — scroll with arrows, track clicks, wheel or thumb drag"));
                }
            });
        });
    });
    pack.apply(&h.ctx);
    h.run_steps(8);
    let at = h.get_by_label("Other action").rect().center();
    match state {
        "normal" => (),
        "scroll" => {
            for _ in 0..8 {
                h.get_by_label("Scroll down").click();
                h.run_steps(2);
            }
        }
        "arrow" => {
            let at = h.get_by_label("Scroll down").rect().center();
            h.event(egui::Event::PointerMoved(at));
            h.run_steps(1);
            h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
            h.run_steps(2);
        }
        "focus" => {
            h.get_by_label("Other action").focus();
            h.run_steps(2);
        }
        "hover" | "pressed" => {
            h.event(egui::Event::PointerMoved(at));
            h.run_steps(2);
            if state == "pressed" {
                h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
                h.run_steps(2);
            }
        }
        _ => return Err("unknown control state".into()),
    }
    h.render()?.save(out)?;
    println!("wrote {out}");
    Ok(())
}
