//! Historical selection ink covers nested controls, while thumbnail data stays unchanged.
use egui::{Color32, Rect, Shape, pos2, vec2};
use egui_kittest::{Harness, kittest::Queryable};
use photocraft_engine::Session;
use photocraft_ui_egui::{
    PhotocraftApp, Services,
    control::{ControlRequest, Outcome, handle},
    theme::{Appearance, Tokens},
};
use serde_json::json;

fn fixture() -> (Session, u64, u64, u64) {
    let mut s = Session::new();
    s.execute("file.new", json!({"width":64,"height":48,"mode":"rgb","depth":8})).unwrap();
    let blue = s.execute("layer.new.layer", json!({"name":"Visible blue"})).unwrap()["layer"].as_u64().unwrap();
    s.execute("edit.fill", json!({"contents":"color","color":"#336699"})).unwrap();
    s.execute("layer.layerMask.revealAll", json!({"layer":blue})).unwrap();
    s.execute("layer.vectorMask.revealAll", json!({"layer":blue})).unwrap();
    s.execute("layer.layerStyle.dropShadow", json!({"layer":blue})).unwrap();
    s.execute("layer.setProps", json!({"layer":blue,"locks":{"all":true},"blend":"Multiply"})).unwrap();
    let hidden = s.execute("layer.new.layer", json!({"name":"Hidden clipped"})).unwrap()["layer"].as_u64().unwrap();
    s.execute("edit.fill", json!({"contents":"color","color":"#CC3333"})).unwrap();
    s.execute("layer.setProps", json!({"layer":hidden,"visible":false,"clipped":true})).unwrap();
    s.execute("layer.select", json!({"layer":blue})).unwrap();
    s.execute("layer.select", json!({"layer":hidden,"mode":"add"})).unwrap();
    s.execute("layer.linkLayers", json!({})).unwrap();
    let group = s.execute("layer.groupLayers", json!({"name":"Folder"})).unwrap()["layer"].as_u64().unwrap();
    s.execute("layer.layerStyle.dropShadow", json!({"layer":group})).unwrap();
    (s, blue, hidden, group)
}

fn visit(shape: &Shape, f: &mut impl FnMut(&Shape)) {
    if let Shape::Vec(children) = shape {
        for child in children {
            visit(child, f);
        }
    } else {
        f(shape);
    }
}
fn foreground(h: &Harness<'_, PhotocraftApp>, name: &str) -> Option<Color32> {
    let mut result = None;
    for s in &h.output().shapes {
        visit(&s.shape, &mut |s| {
            if let Shape::Text(t) = s
                && t.galley.job.text == name
            {
                result = t.galley.job.sections.first().map(|s| s.format.color);
            }
        });
    }
    result
}
fn fill(h: &Harness<'_, PhotocraftApp>, row: Rect) -> Option<Color32> {
    let mut result = None;
    for s in &h.output().shapes {
        visit(&s.shape, &mut |s| {
            if let Shape::Rect(r) = s
                && r.rect == row
                && r.fill.a() > 0
            {
                result = Some(r.fill);
            }
        });
    }
    result
}
fn ink(h: &Harness<'_, PhotocraftApp>, region: Rect, color: Color32) -> bool {
    let mut result = false;
    for s in &h.output().shapes {
        visit(&s.shape, &mut |s| {
            if !region.expand(1.0).contains_rect(s.visual_bounding_rect()) {
                return;
            }
            result |= match s {
                Shape::Mesh(m) => !m.vertices.is_empty() && m.vertices.iter().all(|v| v.color == color),
                Shape::Path(p) => p.fill == color || p.stroke.color == egui::epaint::ColorMode::Solid(color),
                Shape::Rect(r) => r.stroke.color == color || (r.brush.is_some() && r.fill == color),
                Shape::LineSegment { stroke, .. } => stroke.color == color,
                Shape::Text(t) => t.galley.job.sections.iter().any(|s| s.format.color == color),
                _ => false,
            };
        });
    }
    result
}
fn execute(h: &mut Harness<'_, PhotocraftApp>, method: &str, params: serde_json::Value) {
    let (r, _) = ControlRequest::new(method, params);
    let ctx = h.ctx.clone();
    assert!(matches!(handle(h.state_mut(),&ctx,&r), Outcome::Done(v) if v.get("ok") == Some(&json!(true))));
    h.run_steps(4);
}

#[test]
fn historical_layer_selection_is_readable_and_nested_controls_remain_interactive() {
    let evidence = std::env::var_os("PHOTOCRAFT_HISTORICAL_LAYER_EVIDENCE").map(std::path::PathBuf::from);
    for theme in ["windows-98-se", "mac-os-9-1", "system-6", "classic", "pro"] {
        for scale in [1.0, 1.5, 2.0] {
            for width in [240.0, 380.0] {
                let (session, blue, hidden, group) = fixture();
                let builder = Harness::builder().with_size(vec2(1280.0, 900.0)).with_pixels_per_point(scale).with_max_steps(64);
                let builder = if evidence.is_some() && width == 380.0 { builder.wgpu() } else { builder };
                let mut h = builder.build_eframe(move |cc| {
                    PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
                    PhotocraftApp::new(session, Services::default())
                });
                h.run_steps(4);
                execute(&mut h, "ui.theme.select", json!({"id":format!("builtin.{theme}")}));
                execute(&mut h, "ui.set", json!({"dockWidth":width,"panels":{"color":false,"properties":false,"history":false,"navigator":false}}));
                let tokens = Tokens::get(&h.ctx);
                let historical = tokens.appearance != Appearance::Modern;
                let mut thumbnail_sample = None;
                for state in ["group", "single", "multiple", "collapsed"] {
                    let selected = match state {
                        "group" | "collapsed" => vec![group],
                        "single" => vec![blue],
                        _ => vec![blue, hidden],
                    };
                    execute(&mut h, "engine.execute", json!({"command":"layer.select","params":{"layer":selected[0]}}));
                    if selected.len() > 1 {
                        execute(&mut h, "engine.execute", json!({"command":"layer.select","params":{"layer":hidden,"mode":"add"}}));
                    }
                    if state == "collapsed" {
                        execute(&mut h, "engine.execute", json!({"command":"layer.setExpanded","params":{"layer":group,"expanded":false}}));
                    }
                    let rows = photocraft_ui_egui::layer_row_ui::recorded(&h.ctx);
                    for (id, name) in [(group, "Folder"), (blue, "Visible blue"), (hidden, "Hidden clipped")] {
                        if state == "collapsed" && id != group {
                            continue;
                        }
                        let row = rows.iter().find(|r| r.layer == id).unwrap();
                        if selected.contains(&id) {
                            let expected_fill = if historical || tokens.pro { tokens.row_selected } else { tokens.hover };
                            assert_eq!(fill(&h, row.row), Some(expected_fill), "{theme} {scale} {width} {state} {name}");
                            let expected_text = if historical {
                                tokens.row_selected_text
                            } else if id == hidden {
                                tokens.text_faint
                            } else {
                                tokens.text
                            };
                            assert_eq!(foreground(&h, name), Some(expected_text), "{theme} {scale} {width} {state} {name}");
                            if historical {
                                let eye = Rect::from_min_size(pos2(row.row.left() + 6.0, row.row.center().y - 11.0), vec2(22.0, 22.0));
                                // Hidden layers keep their clickable eye column empty on main.
                                if id != hidden {
                                    assert!(ink(&h, eye, tokens.row_selected_text), "{theme}: eye {eye:?}");
                                }
                                for (_, r) in &row.indicators {
                                    assert!(ink(&h, *r, tokens.row_selected_text), "{theme}: indicator {r:?}");
                                }
                                if id == blue {
                                    let (_, chains) = photocraft_ui_egui::mask_thumbs_ui::recorded(&h.ctx, blue).unwrap();
                                    assert_eq!(chains.len(), 2);
                                    for (_, r) in chains {
                                        assert!(ink(&h, r, tokens.row_selected_text), "{theme}: mask chain");
                                    }
                                }
                                if id == group {
                                    let triangle = h.get_by_label(if state == "collapsed" { "Expand group Folder" } else { "Collapse group Folder" }).rect();
                                    assert!(ink(&h, triangle, tokens.row_selected_text), "{theme}: disclosure");
                                }
                            }
                        }
                    }
                    if let Some(root) = &evidence
                        && width == 380.0
                    {
                        let dir = root.join(theme);
                        std::fs::create_dir_all(&dir).unwrap();
                        let image = h.render().unwrap();
                        if state != "collapsed" {
                            let (thumbs, _) = photocraft_ui_egui::mask_thumbs_ui::recorded(&h.ctx, blue).unwrap();
                            let mask = thumbs.first().unwrap().1;
                            let center = pos2(mask.left() - 16.0 - mask.width() / 2.0, mask.center().y);
                            let x = (center.x * scale).round() as u32;
                            let y = (center.y * scale).round() as u32;
                            let mut samples = vec![*image.get_pixel(x, y)];
                            for (_, mask) in &thumbs {
                                samples.push(*image.get_pixel((mask.center().x * scale).round() as u32, (mask.center().y * scale).round() as u32));
                                if historical {
                                    samples.push(*image.get_pixel((mask.center().x * scale).round() as u32, ((mask.top() + 1.0) * scale).round() as u32));
                                }
                            }
                            if let Some(ref before) = thumbnail_sample {
                                assert_eq!(&samples, before, "{theme} {state}: selection changed image/mask thumbnail data");
                            } else {
                                thumbnail_sample = Some(samples);
                            }
                        }
                        image.save(dir.join(format!("{state}-{scale}x.png"))).unwrap();
                    }
                }
                h.get_by_label("Expand group Folder").click();
                h.run_steps(4);
                assert!(h.get_by_label("Visible blue").rect().height() > 0.0);
                execute(&mut h, "engine.execute", json!({"command":"layer.select","params":{"layer":blue}}));
                execute(&mut h, "engine.execute", json!({"command":"layer.setProps","params":{"layer":blue,"locks":{"all":false}}}));
                h.get_by_label("Unlink layer mask Visible blue").click();
                h.run_steps(4);
                assert!(!h.state().session.active().unwrap().doc.layer(photocraft_doc::LayerId(blue)).unwrap().mask.as_ref().unwrap().linked);
            }
        }
    }
}

#[test]
fn historical_icon_tiles_invert_without_tinting_fill_previews() {
    let evidence = std::env::var_os("PHOTOCRAFT_HISTORICAL_LAYER_EVIDENCE").map(std::path::PathBuf::from);
    for theme in ["windows-98-se", "mac-os-9-1", "system-6", "classic", "pro"] {
        for scale in [1.0, 1.5, 2.0] {
            let mut s = Session::new();
            s.execute("file.new", json!({"width":128,"height":96,"mode":"rgb","depth":8})).unwrap();
            let text = s.execute("type.create", json!({"name":"Type tile","text":"Label","x":5,"y":30,"size":12})).unwrap()["layer"].as_u64().unwrap();
            let adjustment = s.execute("layer.newAdjustmentLayer.brightnessContrast", json!({})).unwrap()["layer"].as_u64().unwrap();
            s.execute("layer.setProps", json!({"layer":adjustment,"name":"Tone tile"})).unwrap();
            let solid = s.execute("layer.newFillLayer.solidColor", json!({"color":"#33CC66"})).unwrap()["layer"].as_u64().unwrap();
            s.execute("layer.setProps", json!({"layer":solid,"name":"Solid tile"})).unwrap();
            let gradient = s.execute("layer.newFillLayer.gradient", json!({"from":"#336699","to":"#CC6633"})).unwrap()["layer"].as_u64().unwrap();
            s.execute("layer.setProps", json!({"layer":gradient,"name":"Gradient tile"})).unwrap();
            let builder = Harness::builder().with_size(vec2(1280.0, 900.0)).with_pixels_per_point(scale);
            let builder = if evidence.is_some() { builder.wgpu() } else { builder };
            let mut h = builder.build_eframe(move |cc| {
                PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
                PhotocraftApp::new(s, Services::default())
            });
            h.run_steps(4);
            execute(&mut h, "ui.theme.select", json!({"id":format!("builtin.{theme}")}));
            execute(&mut h, "ui.set", json!({"dockWidth":380,"panels":{"color":false,"properties":false,"history":false,"navigator":false}}));
            let tokens = Tokens::get(&h.ctx);
            for (id, name, icon) in
                [(text, "Type tile", Some("type")), (adjustment, "Tone tile", Some("contrast")), (solid, "Solid tile", None), (gradient, "Gradient tile", None)]
            {
                execute(&mut h, "engine.execute", json!({"command":"layer.select","params":{"layer":id}}));
                let row = photocraft_ui_egui::layer_row_ui::recorded(&h.ctx).into_iter().find(|r| r.layer == id).unwrap().row;
                let expected = if tokens.appearance == Appearance::System6 {
                    tokens.row_selected_text
                } else if icon == Some("type") && tokens.appearance == Appearance::Modern {
                    Color32::from_gray(40)
                } else {
                    tokens.icon
                };
                let mut found = false;
                let mut fill_preserved = false;
                for shape in &h.output().shapes {
                    visit(&shape.shape, &mut |s| {
                        if id == gradient
                            && let Shape::Mesh(m) = s
                            && row.contains_rect(s.visual_bounding_rect())
                            && h.ctx.tex_manager().read().meta(m.texture_id).is_some_and(|m| m.name.starts_with("gradient-thumb-"))
                        {
                            assert!(m.vertices.iter().all(|v| v.color == Color32::WHITE), "{theme}: gradient tint");
                            fill_preserved = true;
                        }
                        if let Shape::Rect(r) = s
                            && row.contains_rect(r.rect)
                        {
                            if id == solid && r.rect.width() <= 36.0 && r.rect.height() <= 36.0 && r.fill == Color32::from_rgb(51, 204, 102) {
                                fill_preserved = true;
                            }
                            if let Some(icon) = icon
                                && let Some(brush) = &r.brush
                                && h.ctx.tex_manager().read().meta(brush.fill_texture_id).is_some_and(|m| m.name.contains(&format!("icons/{icon}.svg")))
                            {
                                assert_eq!(r.fill, expected, "{theme} {name}: icon tint");
                                found = true;
                            }
                            if tokens.appearance != Appearance::Modern && r.rect.width() <= 36.0 && r.rect.height() <= 36.0 {
                                assert_eq!(r.corner_radius, egui::CornerRadius::ZERO, "{theme} {name}: thumbnail radius");
                            }
                        }
                    });
                }
                if icon.is_some() {
                    assert!(found, "{theme} {name}: icon texture missing");
                } else {
                    assert!(fill_preserved, "{theme} {name}: fill preview changed");
                }
                if let Some(root) = &evidence {
                    let dir = root.join(theme);
                    std::fs::create_dir_all(&dir).unwrap();
                    h.render().unwrap().save(dir.join(format!("{}-{scale}x.png", name.split_whitespace().next().unwrap().to_lowercase()))).unwrap();
                }
            }
        }
    }
}
