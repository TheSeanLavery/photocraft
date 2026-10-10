//! Actual app layout must contain compact chrome controls at every display scale.
use egui_kittest::{Harness, kittest::Queryable};
use photocraft_engine::Session;
use photocraft_ui_egui::{
    PhotocraftApp, Services,
    control::{ControlRequest, Outcome, handle},
    theme::LayoutTokens,
};
use serde_json::json;

#[test]
fn historical_bars_contain_their_controls_and_layer_rows_at_all_scales() {
    for id in ["builtin.windows-98-se", "builtin.mac-os-9-1", "builtin.system-6"] {
        for scale in [1.0, 1.5, 2.0] {
            let mut h = Harness::builder().with_size(egui::vec2(1280.0, 900.0)).with_pixels_per_point(scale).build_eframe(|cc| {
                PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
                let mut app = PhotocraftApp::new(Session::new(), Services::default());
                app.run("file.new", json!({"width":640,"height":480})).unwrap();
                app
            });
            h.run_steps(4);
            let ctx = h.ctx.clone();
            let (request, _) = ControlRequest::new("ui.theme.select", json!({"id":id}));
            let Outcome::Done(result) = handle(h.state_mut(), &ctx, &request) else { panic!("selection did not complete") };
            assert_eq!(result.get("ok"), Some(&json!(true)));
            h.run_steps(6);
            let layout = LayoutTokens::get(&ctx);
            let brush = h.get_by_label("Brush Preset picker").rect();
            let top = layout.title_bar_height + layout.menu_bar_height;
            assert!(brush.top() >= top - 0.5, "{id} {scale}: {brush:?}");
            assert!(brush.bottom() <= top + layout.options_bar_height + 0.5, "{id} {scale}: {brush:?}");
            let fit = h.get_by_label("Fit").rect();
            assert!(fit.top() >= 900.0 - layout.status_bar_height - 0.5, "{id} {scale}: {fit:?}");
            assert!(fit.bottom() <= 900.5, "{id} {scale}: {fit:?}");
            assert_eq!(h.get_by_label("Background").rect().height(), layout.layer_row_height);
            let color_height = |ctx: &egui::Context| {
                photocraft_ui_egui::dock::last_rects(ctx).into_iter().find(|(group, _)| *group == photocraft_ui_egui::dock::Group::Color).unwrap().1.height()
            };
            assert_eq!(color_height(&ctx), layout.color_panel_height);
            assert!(!h.state().ui.dock.heights.contains_key(&photocraft_ui_egui::dock::Group::Color));
            h.state_mut().ui.dock.heights.insert(photocraft_ui_egui::dock::Group::Color, 150.0);
            h.run_steps(4);
            assert_eq!(color_height(&ctx), 150.0);
            h.state_mut().ui.dock.set_collapsed(photocraft_ui_egui::dock::Group::Color, true);
            h.run_steps(4);
            assert_eq!(color_height(&ctx), layout.tab_height + layout.panel_padding * 2.0);
        }
    }
}
