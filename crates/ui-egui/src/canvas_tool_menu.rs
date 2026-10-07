//! Canvas context actions for selection tools. Entries reuse the Select menu's command ids and
//! its enablement/dispatch, so dialogs and automation take the same path as the menu bar.

use egui::Context;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{PhotocraftApp, state::Tool};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CanvasToolMenu {
    pub pos: [f32; 2],
    pub tool: Tool,
    pub has_selection: bool,
}

/// Tools whose plain canvas right-click offers selection actions.
pub fn applies(tool: Tool) -> bool {
    matches!(tool, Tool::RectMarquee | Tool::EllipseMarquee | Tool::Lasso | Tool::PolygonLasso | Tool::MagicWand | Tool::ObjectSelection)
}

/// Command ids are shared with the Select menu. Disabled actions remain visible.
pub fn entries(has_selection: bool) -> &'static [(&'static str, &'static str)] {
    if has_selection {
        &[
            ("Deselect", "select.deselect"),
            ("Inverse Selection", "select.inverse"),
            ("Feather…", "select.modify.feather"),
            ("Select and Mask…", "select.selectAndMask"),
            ("Transform Selection", "select.transformSelection"),
        ]
    } else {
        &[("Reselect", "select.reselect")]
    }
}

pub fn open(app: &mut PhotocraftApp, tool: Tool, pos: [f32; 2]) -> bool {
    if !applies(tool) || !pos.iter().all(|v| v.is_finite()) {
        return false;
    }
    app.ui.brush_picker = None;
    app.ui.layer_menu = None;
    let has_selection = app.session.active().is_some_and(|s| s.doc.selection.is_some());
    app.ui.canvas_tool_menu = Some(CanvasToolMenu { pos, tool, has_selection });
    true
}

pub fn choose(app: &mut PhotocraftApp, ctx: &Context, command: &str) {
    let Some(menu) = app.ui.canvas_tool_menu.take() else { return };
    if !entries(menu.has_selection).iter().any(|&(_, id)| id == command) || !crate::menus::is_enabled(app, command) {
        return;
    }
    if let Err(e) = crate::menus::invoke(app, ctx, command, json!({})) {
        app.ui.status = e;
    }
}

pub fn show(app: &mut PhotocraftApp, ctx: &Context) {
    let Some(menu) = app.ui.canvas_tool_menu.clone() else { return };
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) || app.ui.tool != menu.tool {
        app.ui.canvas_tool_menu = None;
        return;
    }
    let id = egui::Id::new("canvas-selection-menu");
    let screen = ctx.content_rect();
    let size = ctx.memory(|m| m.area_rect(id)).map_or(egui::vec2(210.0, 140.0), |r| r.size());
    let pos = egui::pos2(menu.pos[0].min(screen.right() - size.x).max(screen.left()), menu.pos[1].min(screen.bottom() - size.y).max(screen.top()));
    let mut selected = None;
    let area = egui::Area::new(id).order(egui::Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
        egui::Frame::menu(ui.style()).show(ui, |ui| {
            let t = crate::theme::Tokens::get(ui.ctx());
            let v = &mut ui.style_mut().visuals;
            v.widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
            v.widgets.inactive.bg_stroke = egui::Stroke::NONE;
            v.widgets.hovered.weak_bg_fill = t.accent;
            v.widgets.hovered.bg_fill = t.accent;
            v.widgets.hovered.bg_stroke = egui::Stroke::NONE;
            v.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
            v.widgets.hovered.corner_radius = egui::CornerRadius::same(3);
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.set_width(200.0);
            for &(label, command) in entries(menu.has_selection) {
                let item = egui::Button::selectable(false, tl!(&label)).min_size(egui::vec2(200.0, 22.0));
                if ui.add_enabled(crate::menus::is_enabled(app, command), item).clicked() {
                    selected = Some(command);
                }
            }
        });
    });
    if let Some(command) = selected {
        choose(app, ctx, command);
    } else if ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !area.response.rect.contains(p))) {
        app.ui.canvas_tool_menu = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Modifiers, PointerButton, Pos2, vec2};
    use egui_kittest::Harness;

    fn app() -> PhotocraftApp {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 32, "height": 32})).unwrap();
        app
    }

    fn harness(mut app: PhotocraftApp) -> Harness<'static, PhotocraftApp> {
        app.sync_views();
        let mut h = Harness::builder().with_size(vec2(1000.0, 700.0)).build_ui_state(
            |ui, app: &mut PhotocraftApp| {
                let ctx = ui.ctx().clone();
                if !ctx.fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                    return;
                }
                egui::CentralPanel::default().show(ui, |ui| crate::canvas::document_area(app, ui));
            },
            app,
        );
        PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::default());
        h.run_steps(4);
        h
    }

    fn right_click(h: &mut Harness<'static, PhotocraftApp>, p: Pos2, modifiers: Modifiers) {
        h.event(egui::Event::ModifiersChanged(modifiers));
        h.event(egui::Event::PointerMoved(p));
        h.run_steps(1);
        h.event(egui::Event::PointerButton { pos: p, button: PointerButton::Secondary, pressed: true, modifiers });
        h.run_steps(1);
        h.event(egui::Event::PointerButton { pos: p, button: PointerButton::Secondary, pressed: false, modifiers });
        h.event(egui::Event::ModifiersChanged(Modifiers::NONE));
        h.run_steps(2);
    }

    #[test]
    fn canvas_gesture_preserves_selection_and_command_override() {
        let mut app = app();
        app.ui.tool = Tool::Lasso;
        app.run("select.rect", json!({"x": 1, "y": 1, "width": 8, "height": 8})).unwrap();
        let mut h = harness(app);
        let center = h.state().last_canvas_rect.center();
        let revision = h.state().session.active().unwrap().revision;
        right_click(&mut h, center, Modifiers::NONE);
        assert!(h.state().ui.canvas_tool_menu.is_some());
        assert_eq!(h.state().session.active().unwrap().revision, revision, "right-click cannot alter selection");
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        assert!(h.state().ui.canvas_tool_menu.is_none());
        right_click(&mut h, center, Modifiers::COMMAND);
        assert!(h.state().ui.canvas_tool_menu.is_none(), "command right-click belongs to layer picker");
    }

    #[test]
    fn selection_menu_routes_actions_through_commands() {
        let mut app = app();
        app.ui.tool = Tool::RectMarquee;
        app.run("select.rect", json!({"x": 1, "y": 1, "width": 8, "height": 8})).unwrap();
        assert!(open(&mut app, Tool::RectMarquee, [10.0, 10.0]));
        assert!(entries(true).iter().any(|(_, id)| *id == "select.inverse"));
        let before = app.session.journal.len();
        choose(&mut app, &Context::default(), "select.inverse");
        assert!(app.ui.canvas_tool_menu.is_none());
        assert!(app.session.journal[before..].iter().any(|(id, _)| id == "select.inverse"));
        let before = app.session.journal.len();
        assert!(open(&mut app, Tool::RectMarquee, [10.0, 10.0]));
        choose(&mut app, &Context::default(), "select.deselect");
        assert!(app.session.active().unwrap().doc.selection.is_none());
        assert!(app.session.journal[before..].iter().any(|(id, _)| id == "select.deselect"));
        assert_eq!(entries(false), &[("Reselect", "select.reselect")]);
        assert!(open(&mut app, Tool::RectMarquee, [10.0, 10.0]));
        choose(&mut app, &Context::default(), "select.reselect");
        assert!(app.session.active().unwrap().doc.selection.is_some());
        assert!(app.session.journal.iter().any(|(id, _)| id == "select.reselect"));
    }

    #[test]
    fn unsupported_tools_and_nonfinite_positions_do_not_open_menu() {
        let mut app = app();
        assert!(!open(&mut app, Tool::Brush, [1.0, 1.0]));
        assert!(!open(&mut app, Tool::Lasso, [f32::NAN, 1.0]));
        assert!(app.ui.canvas_tool_menu.is_none());
        assert!(open(&mut app, Tool::MagicWand, [1.0, 1.0]));
        let before = app.session.journal.len();
        choose(&mut app, &Context::default(), "file.new");
        assert_eq!(app.session.journal.len(), before, "context menu cannot invoke an unrelated command");
    }
}
