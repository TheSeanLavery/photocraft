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
fn historical_dock_content_uses_authored_spacing_without_modern_padding() {
    for source in [
        include_str!("../../../assets/themes/windows-98-se.json"),
        include_str!("../../../assets/themes/mac-os-9-1.json"),
        include_str!("../../../assets/themes/system-6.json"),
    ] {
        for scale in [1.0, 1.5, 2.0] {
            for gap in [2.0, 7.0] {
                let ctx = egui::Context::default();
                PhotocraftApp::setup_context(&ctx, Default::default());
                ctx.set_pixels_per_point(scale);
                let mut pack = photocraft_ui_egui::theme_pack::ThemePack::parse(source).unwrap();
                pack.geometry.row_gap = Some(gap);
                pack.resolve().unwrap().apply(&ctx);
                let mut app = PhotocraftApp::new(Session::new(), Services::default());
                let mut rows = Vec::new();
                let mut output = ctx.run_ui(
                    egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(380.0, 500.0))), ..Default::default() },
                    |ui| {
                        rows.clear();
                        egui::CentralPanel::default().show(ui, |ui| {
                            photocraft_ui_egui::dock::show(&mut app, ui, &[photocraft_ui_egui::dock::Group::Properties], |_, ui, _, _| {
                                for _ in 0..2 {
                                    rows.push(ui.allocate_exact_size(egui::vec2(80.0, 16.0), egui::Sense::hover()).0);
                                }
                            });
                        });
                    },
                );
                output.textures_delta.clear();
                assert_eq!(rows.len(), 2);
                let actual = rows[1].top() - rows[0].bottom();
                assert!((actual - gap).abs() < 0.01, "{} at {scale}x: authored {gap}, actual {actual}", pack.id);
            }
        }
    }
}

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
            // The scrubber paints the root as Original; its accessibility name remains
            // the engine's Open history label. Verify the actual rendered text colour.
            let original = photocraft_ui_egui::i18n::t("Original");
            assert_eq!(foreground(&h.output().shapes, original), Some(tokens.row_selected_text), "{id} {scale}");
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

#[test]
fn every_historical_adjustment_properties_card_stays_inside_the_viewport() {
    let commands: Vec<_> = photocraft_engine::command_specs().iter().filter(|c| c.id.starts_with("layer.newAdjustmentLayer.")).map(|c| c.id).collect();
    assert_eq!(commands.len(), 16);
    let mut failures = Vec::new();
    for id in ["builtin.windows-98-se", "builtin.mac-os-9-1", "builtin.system-6"] {
        for (scale, height) in [(1.0, 900.0), (1.5, 900.0), (2.0, 900.0), (1.0, 640.0), (1.5, 640.0), (2.0, 640.0)] {
            for command in &commands {
                let mut h = Harness::builder().with_size(egui::vec2(1280.0, height)).with_pixels_per_point(scale).build_eframe(|cc| {
                    PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
                    let mut app = PhotocraftApp::new(Session::new(), Services::default());
                    app.run("file.new", json!({"width":64,"height":48})).unwrap();
                    app.run(command, json!({})).unwrap();
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
                if rect.width() > LayoutTokens::get(&ctx).dock_width + 2.0 || !ctx.content_rect().contains_rect(rect) {
                    failures.push(format!("{id} {scale} height{height} {command}: {rect:?}"));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn long_gradient_properties_scroll_to_reset_without_leaving_the_viewport() {
    for id in ["builtin.windows-98-se", "builtin.mac-os-9-1", "builtin.system-6"] {
        let mut h = Harness::builder().with_size(egui::vec2(1280.0, 640.0)).build_eframe(|cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
            let mut app = PhotocraftApp::new(Session::new(), Services::default());
            app.run("file.new", json!({"width":64,"height":48})).unwrap();
            let stops: Vec<_> = (0..64).map(|i| json!([f64::from(i) / 63.0, if i % 2 == 0 { "#000000" } else { "#FFFFFF" }])).collect();
            app.run("layer.newAdjustmentLayer.gradientMap", json!({"stops":stops})).unwrap();
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
        assert!(ctx.content_rect().contains_rect(rect), "{id}: {rect:?}");
        h.event(egui::Event::PointerMoved(rect.center()));
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            phase: egui::TouchPhase::Move,
            delta: egui::vec2(0.0, -10000.0),
            modifiers: egui::Modifiers::NONE,
        });
        h.run_steps(24);
        let reset = h.get_by_label("Reset to defaults");
        assert!(rect.contains_rect(reset.rect()), "{id}: reset {:?} vs {rect:?}", reset.rect());
        reset.click();
        h.run_steps(4);
        let state = h.state().session.active().unwrap();
        let layer = state.doc.layer(state.active_layer.unwrap()).unwrap();
        let photocraft_doc::LayerContent::Adjustment(adj) = &layer.content else { panic!("lost adjustment") };
        let params = photocraft_engine::adjust_params::to_params(adj);
        assert_eq!(params["stops"].as_array().unwrap().len(), 2);
    }
}

#[test]
fn historical_slider_keyboard_edits_dispatch_to_the_adjustment_layer() {
    let mut h = Harness::builder().with_size(egui::vec2(1280.0, 900.0)).build_eframe(|cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        let mut app = PhotocraftApp::new(Session::new(), Services::default());
        app.run("file.new", json!({"width":64,"height":48})).unwrap();
        app.run("layer.newAdjustmentLayer.brightnessContrast", json!({})).unwrap();
        app.ui.panels.properties = true;
        app
    });
    h.run_steps(4);
    let ctx = h.ctx.clone();
    let (request, _) = ControlRequest::new("ui.theme.select", json!({"id":"builtin.system-6"}));
    let Outcome::Done(result) = handle(h.state_mut(), &ctx, &request) else { panic!("did not complete") };
    assert_eq!(result.get("ok"), Some(&json!(true)));
    h.run_steps(6);
    let card = ctx.memory(|m| m.area_rect(egui::Id::new("properties-card"))).unwrap();
    let label = h.get_by_label("Brightness").rect();
    let layout = LayoutTokens::get(&ctx);
    let at = egui::pos2(card.center().x, label.center().y + layout.field_height / 2.0 + layout.row_gap + 9.0);
    h.event(egui::Event::PointerMoved(at));
    h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.run_steps(2);
    h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run_steps(2);
    h.event(egui::Event::Key { key: egui::Key::End, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE });
    h.run_steps(4);
    let state = h.state().session.active().unwrap();
    let layer = state.doc.layer(state.active_layer.unwrap()).unwrap();
    let photocraft_doc::LayerContent::Adjustment(adj) = &layer.content else { panic!("lost adjustment") };
    assert_eq!(photocraft_engine::adjust_params::to_params(adj)["brightness"], json!(150.0));
}
