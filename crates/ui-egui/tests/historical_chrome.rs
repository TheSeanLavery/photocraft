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
            let dock_width = |ctx: &egui::Context| egui::containers::panel::PanelState::load(ctx, egui::Id::new("dock")).unwrap().size().x;
            assert_eq!(dock_width(&ctx), layout.dock_width);
            photocraft_ui_egui::panels::request_dock_width(&ctx, 210.0);
            h.run_steps(4);
            assert_eq!(dock_width(&ctx), layout.dock_min_width);
            photocraft_ui_egui::panels::request_dock_width(&ctx, 330.0);
            h.run_steps(4);
            assert_eq!(dock_width(&ctx), 330.0);
            let (request, _) = ControlRequest::new("ui.theme.select", json!({"id":"builtin.classic"}));
            handle(h.state_mut(), &ctx, &request);
            h.run_steps(4);
            assert_eq!(dock_width(&ctx), 330.0);
            let (request, _) = ControlRequest::new("ui.theme.select", json!({"id":id}));
            handle(h.state_mut(), &ctx, &request);
            h.run_steps(4);
            assert_eq!(dock_width(&ctx), 330.0);
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

#[test]
fn historical_color_components_fit_the_compact_panel_and_remain_interactive() {
    for id in ["builtin.windows-98-se", "builtin.mac-os-9-1", "builtin.system-6"] {
        for scale in [1.0, 1.5, 2.0] {
            let mut h = Harness::builder().with_size(egui::vec2(1280.0, 900.0)).with_pixels_per_point(scale).build_eframe(|cc| {
                PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
                PhotocraftApp::new(Session::new(), Services::default())
            });
            h.run_steps(4);
            let ctx = h.ctx.clone();
            let (request, _) = ControlRequest::new("ui.theme.select", json!({"id":id}));
            handle(h.state_mut(), &ctx, &request);
            h.state_mut().ui.dock_tabs.color = 1;
            h.state_mut().session.tools.foreground = [1.0, 0.0, 0.0, 1.0];
            h.run_steps(6);
            let panel = photocraft_ui_egui::dock::last_rects(&ctx).into_iter().find(|(g, _)| *g == photocraft_ui_egui::dock::Group::Color).unwrap().1;
            for label in ["Hue", "Saturation", "Brightness", "#FF0000"] {
                let rect = h.get_all_by_label(label).next().unwrap().rect();
                assert!(panel.contains_rect(rect), "{id} {scale}: {label} {rect:?} outside {panel:?}");
            }
            h.get_all_by_label("Hue").last().unwrap().click();
            h.run_steps(4);
            let color = h.state().session.tools.foreground;
            assert!(color[0] < 0.05 && color[1] > 0.95 && color[2] > 0.95, "{id} {scale}: hue midpoint did not select cyan: {color:?}");
        }
    }
}

#[test]
fn all_historical_dock_groups_fit_and_share_the_theme_gap_at_all_scales() {
    for id in ["builtin.windows-98-se", "builtin.mac-os-9-1", "builtin.system-6"] {
        for scale in [1.0, 1.5, 2.0] {
            let mut h = Harness::builder().with_size(egui::vec2(1280.0, 900.0)).with_pixels_per_point(scale).build_eframe(|cc| {
                PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
                PhotocraftApp::new(Session::new(), Services::default())
            });
            h.run_steps(4);
            let ctx = h.ctx.clone();
            for (method, params) in [
                ("ui.theme.select", json!({"id":id})),
                ("ui.set", json!({"panels":{"color":true,"properties":true,"character":true,"navigator":true,"history":true,"layers":true}})),
            ] {
                let (request, _) = ControlRequest::new(method, params);
                let Outcome::Done(result) = handle(h.state_mut(), &ctx, &request) else { panic!("did not complete") };
                assert_eq!(result.get("ok"), Some(&json!(true)));
            }
            h.run_steps(6);
            let rects = photocraft_ui_egui::dock::last_rects(&ctx);
            // Historical profiles inherit Classic: Properties remains a floating card.
            assert_eq!(rects.len(), 5, "{id} {scale}");
            for (_, rect) in &rects {
                assert!(rect.height() >= 80.0 && rect.bottom() <= 900.0, "{id} {scale} {rect:?}");
            }
            for pair in rects.windows(2) {
                assert!((pair[1].1.top() - pair[0].1.bottom() - LayoutTokens::get(&ctx).dock_group_gap).abs() < 0.01);
            }
            assert!(h.state().ui.dock.heights.is_empty());
        }
    }
}

#[test]
fn selected_history_labels_use_the_theme_foreground_at_all_scales() {
    fn foreground(shapes: &[egui::epaint::ClippedShape], label: &str) -> Option<egui::Color32> {
        fn find(shape: &egui::Shape, label: &str) -> Option<egui::Color32> {
            match shape {
                egui::Shape::Text(text) if text.galley.job.text == label => text.galley.job.sections.first().map(|s| s.format.color),
                egui::Shape::Vec(shapes) => shapes.iter().find_map(|s| find(s, label)),
                _ => None,
            }
        }
        shapes.iter().find_map(|s| find(&s.shape, label))
    }
    for id in ["builtin.windows-98-se", "builtin.mac-os-9-1", "builtin.system-6", "builtin.classic", "builtin.pro"] {
        for scale in [1.0, 1.5, 2.0] {
            let mut h = Harness::builder().with_size(egui::vec2(1280.0, 900.0)).with_pixels_per_point(scale).build_eframe(|cc| {
                PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
                let mut app = PhotocraftApp::new(Session::new(), Services::default());
                app.run("file.new", json!({"width":640,"height":480})).unwrap();
                app.ui.panels.history = true;
                app
            });
            h.run_steps(4);
            let ctx = h.ctx.clone();
            let (request, _) = ControlRequest::new("ui.theme.select", json!({"id":id}));
            let Outcome::Done(result) = handle(h.state_mut(), &ctx, &request) else { panic!("did not complete") };
            assert_eq!(result.get("ok"), Some(&json!(true)));
            h.run_steps(6);
            let tokens = photocraft_ui_egui::theme::Tokens::get(&ctx);
            assert_eq!(foreground(&h.output().shapes, "Open"), Some(tokens.row_selected_text), "{id} {scale}");
            assert!(h.get_by_label("Open").rect().height() > 0.0);
            if id == "builtin.system-6" {
                assert_eq!(tokens.row_selected_text, egui::Color32::WHITE);
                assert_eq!(tokens.accent_soft, egui::Color32::BLACK);
            }
        }
    }
}

#[test]
fn historical_floating_properties_fit_and_visibility_controls_still_dispatch() {
    for id in ["builtin.windows-98-se", "builtin.mac-os-9-1", "builtin.system-6"] {
        for scale in [1.0, 1.5, 2.0] {
            let mut h = Harness::builder().with_size(egui::vec2(1280.0, 900.0)).with_pixels_per_point(scale).build_eframe(|cc| {
                PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
                let mut app = PhotocraftApp::new(Session::new(), Services::default());
                app.run("file.new", json!({"width":640,"height":480})).unwrap();
                app.run("layer.newAdjustmentLayer.brightnessContrast", json!({})).unwrap();
                app.ui.panels.properties = true;
                app
            });
            h.run_steps(4);
            let ctx = h.ctx.clone();
            let (request, _) = ControlRequest::new("ui.theme.select", json!({"id":id}));
            let Outcome::Done(result) = handle(h.state_mut(), &ctx, &request) else { panic!("did not complete") };
            assert_eq!(result.get("ok"), Some(&json!(true)));
            h.run_steps(6);
            let rect = ctx.memory(|m| m.area_rect(egui::Id::new("properties-card"))).unwrap();
            assert!(rect.width() <= LayoutTokens::get(&ctx).dock_width + 2.0, "{id} {scale}: {rect:?}");
            for label in ["Use Legacy", "Adjustment visible", "Clip to layer below", "Reset to defaults"] {
                let control = h.get_by_label(label).rect();
                assert!(rect.contains_rect(control), "{id} {scale} {label}: {control:?} vs {rect:?}");
            }
            let layer_id = h.state().session.active().unwrap().active_layer.unwrap();
            let visible = h.state().session.active().unwrap().doc.layer(layer_id).unwrap().visible;
            h.get_by_label("Adjustment visible").click();
            h.run_steps(4);
            assert_ne!(h.state().session.active().unwrap().doc.layer(layer_id).unwrap().visible, visible);
            h.get_by_label("Close").click();
            h.run_steps(4);
            assert!(!h.state().ui.panels.properties);
        }
    }
}
