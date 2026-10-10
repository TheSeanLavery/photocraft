//! Offscreen demonstration of arithmetic editing in the actual shared numeric field.
use egui_kittest::{Harness, kittest::Queryable};
use photocraft_ui_egui::{PhotocraftApp, widgets};

fn main() {
    let mut h = Harness::builder().with_size(egui::vec2(480.0, 180.0)).wgpu().build_ui_state(
        |ui, value| {
            ui.heading("PhotoCraft numeric expressions");
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.label("Width:");
                widgets::value_field(ui, value, 1.0..=10000.0, "px", 180.0);
            });
            ui.add_space(12.0);
            ui.label("Type arithmetic, then press Enter to apply.");
        },
        1920.0_f32,
    );
    PhotocraftApp::setup_context(&h.ctx, Default::default());
    h.run_steps(4);
    let center = h.get_by_role(egui::accesskit::Role::SpinButton).rect().center();
    h.hover_at(center);
    h.run_steps(1);
    h.drag_at(center);
    h.run_steps(1);
    h.drop_at(center);
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run();
    h.event(egui::Event::Text("1920/2".to_owned()));
    h.run();
    h.render().expect("render expression").save("/tmp/photocraft-math-expression.png").expect("save expression");
    h.key_press(egui::Key::Enter);
    h.run();
    assert_eq!(*h.state(), 960.0);
    h.render().expect("render result").save("/tmp/photocraft-math-result.png").expect("save result");
}
