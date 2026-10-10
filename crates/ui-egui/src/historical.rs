//! Clean-room historical control painters. No proprietary images, code or fonts are embedded.
use crate::{
    PhotocraftApp,
    theme::{self, Appearance, Tokens},
    theme_pack::ThemePack,
};
use egui::{Color32, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2};
use std::sync::Arc;

pub const IDS: [&str; 3] = ["builtin.windows-98-se", "builtin.mac-os-9-1", "builtin.system-6"];

pub fn preset(id: &str) -> Option<ThemePack> {
    let text = match id {
        "builtin.windows-98-se" => include_str!("../../../assets/themes/windows-98-se.json"),
        "builtin.mac-os-9-1" => include_str!("../../../assets/themes/mac-os-9-1.json"),
        "builtin.system-6" => include_str!("../../../assets/themes/system-6.json"),
        _ => return None,
    };
    ThemePack::parse(text).ok()
}

fn bevel(ui: &Ui, rect: Rect, fill: Color32, raised: bool, t: Tokens) {
    let p = ui.painter();
    p.rect_filled(rect, 0.0, fill);
    let pairs = if raised {
        [(t.bevel_light, t.bevel_dark), (t.bevel_highlight, t.bevel_shadow)]
    } else {
        [(t.bevel_shadow, t.bevel_highlight), (t.bevel_dark, t.bevel_light)]
    };
    for (i, (hi, lo)) in pairs.into_iter().take(if t.bevel_width >= 2.0 { 2 } else { 1 }).enumerate() {
        let r = rect.shrink(i as f32);
        p.line_segment([r.left_bottom(), r.left_top()], Stroke::new(1.0, hi));
        p.line_segment([r.left_top(), r.right_top()], Stroke::new(1.0, hi));
        p.line_segment([r.right_top(), r.right_bottom()], Stroke::new(1.0, lo));
        p.line_segment([r.right_bottom(), r.left_bottom()], Stroke::new(1.0, lo));
    }
}

pub fn surface(ui: &Ui, rect: Rect, fill: Color32, raised: bool) -> bool {
    let t = Tokens::get(ui.ctx());
    match t.appearance {
        Appearance::Modern => return false,
        Appearance::Windows98 | Appearance::Platinum => bevel(ui, rect, fill, raised, t),
        Appearance::System6 => {
            ui.painter().rect_filled(rect, 0.0, fill);
            ui.painter().rect_stroke(rect, 0.0, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
        }
    }
    true
}

fn focus(ui: &Ui, rect: Rect, color: Color32) {
    // One-point RectShapes disappear under egui's edge feathering at 1x. A mesh keeps
    // the period's discrete dots visible, and batches the complete ring in one shape.
    let mut mesh = egui::Mesh::default();
    for k in 0..((rect.width() / 2.0).ceil() as usize).min(2048) {
        let x = (rect.left() + k as f32 * 2.0).round();
        for y in [rect.top().round(), rect.bottom().round()] {
            mesh.add_colored_rect(Rect::from_min_size(pos2(x, y), vec2(1.0, 1.0)), color);
        }
    }
    for k in 0..((rect.height() / 2.0).ceil() as usize).min(2048) {
        let y = (rect.top() + k as f32 * 2.0).round();
        for x in [rect.left().round(), rect.right().round()] {
            mesh.add_colored_rect(Rect::from_min_size(pos2(x, y), vec2(1.0, 1.0)), color);
        }
    }
    ui.painter().add(mesh);
}

/// System 6 disabled ink uses a checker mask rather than transparent gray chrome.
pub fn disabled_stipple(ui: &Ui, rect: Rect, background: Color32) {
    if ui.is_enabled() || Tokens::get(ui.ctx()).appearance != Appearance::System6 {
        return;
    }
    let r = rect.intersect(ui.clip_rect());
    let width = r.width().ceil().clamp(0.0, 2048.0) as usize;
    let height = r.height().ceil().clamp(0.0, 128.0) as usize;
    let mut mesh = egui::Mesh::default();
    for y in 0..height {
        for x in ((y % 2)..width).step_by(2) {
            mesh.add_colored_rect(Rect::from_min_size(r.min + vec2(x as f32, y as f32), vec2(1.0, 1.0)), background);
        }
    }
    ui.painter().add(mesh);
}

pub fn button(ui: &Ui, rect: Rect, response: &Response, primary: bool, galley: Arc<egui::Galley>) -> bool {
    let t = Tokens::get(ui.ctx());
    if t.appearance == Appearance::Modern {
        return false;
    }
    let down = response.is_pointer_button_down_on();
    let p = ui.painter();
    let mut text = t.text;
    let offset = if down && t.appearance == Appearance::Windows98 { vec2(1.0, 1.0) } else { vec2(0.0, 0.0) };
    match t.appearance {
        Appearance::Windows98 => {
            let r = if primary {
                p.rect_stroke(rect, 0.0, Stroke::new(1.0, t.bevel_dark), StrokeKind::Inside);
                rect.shrink(1.0)
            } else {
                rect
            };
            bevel(ui, r, t.chrome, !down, t);
            if response.has_focus() {
                focus(ui, r.shrink(4.0), t.text);
            }
        }
        Appearance::Platinum | Appearance::System6 => {
            let inset = ((rect.height() - galley.size().y - 2.0) / 2.0).clamp(1.0, 4.0);
            let r = if primary {
                p.rect_stroke(rect, 8.0, Stroke::new(if t.appearance == Appearance::System6 { 3.0 } else { 2.0 }, t.text), StrokeKind::Inside);
                rect.shrink(inset)
            } else {
                rect.shrink(inset)
            };
            let fill = if down { t.text } else { t.chrome };
            p.rect_filled(r, 5.0, fill);
            p.rect_stroke(r, 5.0, Stroke::new(1.0, t.text), StrokeKind::Inside);
            if t.appearance == Appearance::Platinum && !down {
                let inset = r.shrink(1.0);
                p.line_segment([pos2(inset.left() + 4.0, inset.top()), pos2(inset.right() - 4.0, inset.top())], Stroke::new(1.0, t.bevel_highlight));
                p.line_segment([pos2(inset.left() + 4.0, inset.bottom()), pos2(inset.right() - 4.0, inset.bottom())], Stroke::new(1.0, t.bevel_shadow));
            }
            if down {
                text = t.field;
            }
            if response.has_focus() && !primary {
                p.rect_stroke(rect, 8.0, Stroke::new(1.0, t.text), StrokeKind::Inside);
            }
        }
        Appearance::Modern => (),
    }
    if !ui.is_enabled() {
        text = t.text_faint;
    }
    let at = rect.center() - galley.size() / 2.0 + offset;
    let text_rect = Rect::from_min_size(at, galley.size());
    p.galley_with_override_text_color(at, galley, text);
    disabled_stipple(ui, text_rect, t.chrome);
    true
}

pub fn checkbox(ui: &Ui, rect: Rect, on: bool, response: &Response) -> bool {
    let t = Tokens::get(ui.ctx());
    if t.appearance == Appearance::Modern {
        return false;
    }
    let size = if t.appearance == Appearance::Windows98 { 13.0 } else { 12.0 };
    let r = Rect::from_center_size(rect.center(), vec2(size, size));
    if t.appearance == Appearance::Windows98 {
        bevel(ui, r, t.field, false, t);
    } else {
        ui.painter().rect_filled(r, if t.appearance == Appearance::Platinum { 2.0 } else { 0.0 }, t.field);
        ui.painter().rect_stroke(r, if t.appearance == Appearance::Platinum { 2.0 } else { 0.0 }, Stroke::new(1.0, t.text), StrokeKind::Inside);
    }
    if on {
        let c = if ui.is_enabled() { t.text } else { t.text_faint };
        if t.appearance == Appearance::Windows98 {
            let a = r.left_center() + vec2(3.0, 0.0);
            let b = r.center_bottom() + vec2(-1.0, -3.0);
            let cpt = r.right_top() + vec2(-3.0, 3.0);
            ui.painter().line_segment([a, b], Stroke::new(2.0, c));
            ui.painter().line_segment([b, cpt], Stroke::new(2.0, c));
        } else {
            let inner = r.shrink(3.0);
            ui.painter().line_segment([inner.left_top(), inner.right_bottom()], Stroke::new(1.0, c));
            ui.painter().line_segment([inner.right_top(), inner.left_bottom()], Stroke::new(1.0, c));
        }
    }
    disabled_stipple(ui, r.shrink(1.0), t.field);
    if response.has_focus() {
        focus(ui, rect.expand(2.0), t.text);
    }
    true
}

/// Radio marks retain the shell's selection semantics and keyboard interaction.
pub fn radio(ui: &Ui, rect: Rect, on: bool, response: &Response) {
    let t = Tokens::get(ui.ctx());
    let radius = if t.appearance == Appearance::Windows98 { 6.5 } else { 6.0 };
    let p = ui.painter();
    p.circle_filled(rect.center(), radius, t.field);
    p.circle_stroke(rect.center(), radius - 0.5, Stroke::new(1.0, t.text));
    if t.appearance != Appearance::System6 {
        // Two-tone recessed circle, without a modern accent-colored selection.
        for (upper, color) in [(true, t.bevel_shadow), (false, t.bevel_highlight)] {
            let start = if upper { std::f32::consts::PI } else { 0.0 };
            let points = (0..=12)
                .map(|i| {
                    let angle = start + i as f32 * std::f32::consts::PI / 12.0;
                    rect.center() + vec2(angle.cos(), angle.sin()) * (radius - 1.0)
                })
                .collect();
            p.add(Shape::line(points, Stroke::new(1.0, color)));
        }
    }
    if on {
        p.circle_filled(rect.center(), 2.5, t.text);
    }
    disabled_stipple(ui, Rect::from_center_size(rect.center(), vec2(radius * 2.0, radius * 2.0)), t.chrome);
    if response.has_focus() {
        focus(ui, response.rect.shrink(1.0), t.text);
    }
}

/// Adapt panel tabs to the selected era while keeping overflow and docking behavior.
pub fn tab(ui: &Ui, rect: Rect, active: bool, galley: Arc<egui::Galley>) -> bool {
    let t = Tokens::get(ui.ctx());
    if t.appearance == Appearance::Modern {
        return false;
    }
    let r = Rect::from_min_max(rect.min + vec2(0.0, if active { 0.0 } else { 2.0 }), rect.max);
    let color = if t.appearance == Appearance::System6 && active { t.accent_text } else { t.text };
    if t.appearance == Appearance::System6 {
        ui.painter().rect_filled(r, 0.0, if active { t.accent } else { t.chrome });
        ui.painter().rect_stroke(r, 0.0, Stroke::new(1.0, t.text), StrokeKind::Inside);
    } else if t.appearance == Appearance::Platinum {
        let slope = (r.height() * 0.35).min(r.width() * 0.2);
        let left = r.left_top() + vec2(slope, 0.0);
        let right = r.right_top() - vec2(slope, 0.0);
        ui.painter().add(Shape::convex_polygon(vec![r.left_bottom(), left, right, r.right_bottom()], t.chrome, Stroke::NONE));
        ui.painter().line_segment([r.left_bottom(), left], Stroke::new(1.0, t.bevel_dark));
        ui.painter().line_segment([left, right], Stroke::new(1.0, t.bevel_dark));
        ui.painter().line_segment([right, r.right_bottom()], Stroke::new(1.0, t.bevel_dark));
        ui.painter().line_segment([r.left_bottom() + vec2(2.0, -1.0), left + vec2(1.0, 1.0)], Stroke::new(1.0, t.bevel_highlight));
        ui.painter().line_segment([left + vec2(1.0, 1.0), right + vec2(-1.0, 1.0)], Stroke::new(1.0, t.bevel_highlight));
        if !active {
            ui.painter().line_segment([r.left_bottom(), r.right_bottom()], Stroke::new(1.0, t.bevel_shadow));
        }
    } else {
        let corners = vec![
            r.left_bottom(),
            r.left_top() + vec2(0.0, 3.0),
            r.left_top() + vec2(3.0, 0.0),
            r.right_top() - vec2(3.0, 0.0),
            r.right_top() + vec2(0.0, 3.0),
            r.right_bottom(),
        ];
        ui.painter().add(Shape::convex_polygon(corners, t.chrome, Stroke::NONE));
        ui.painter().line_segment([r.left_bottom(), r.left_top() + vec2(0.0, 3.0)], Stroke::new(1.0, t.bevel_highlight));
        ui.painter().line_segment([r.left_top() + vec2(0.0, 3.0), r.left_top() + vec2(3.0, 0.0)], Stroke::new(1.0, t.bevel_highlight));
        ui.painter().line_segment([r.left_top() + vec2(3.0, 0.0), r.right_top() - vec2(3.0, 0.0)], Stroke::new(1.0, t.bevel_highlight));
        ui.painter().line_segment([r.right_top() - vec2(3.0, 0.0), r.right_top() + vec2(0.0, 3.0)], Stroke::new(1.0, t.bevel_dark));
        ui.painter().line_segment([r.right_top() + vec2(0.0, 3.0), r.right_bottom()], Stroke::new(1.0, t.bevel_dark));
        if !active {
            ui.painter().line_segment([r.left_bottom(), r.right_bottom()], Stroke::new(1.0, t.bevel_shadow));
        }
    }
    ui.painter().galley_with_override_text_color(r.center() - galley.size() / 2.0, galley, color);
    true
}

pub fn dropdown_arrow(ui: &Ui, rect: Rect) -> bool {
    let t = Tokens::get(ui.ctx());
    if t.appearance == Appearance::Modern {
        return false;
    }
    let r = rect.expand2(vec2(2.0, 2.0));
    if t.appearance == Appearance::Windows98 {
        bevel(ui, r, t.chrome, true, t);
    } else {
        ui.painter().rect_stroke(r, 0.0, Stroke::new(1.0, t.text), StrokeKind::Inside);
    }
    let c = rect.center();
    ui.painter().add(Shape::convex_polygon(vec![c + vec2(-3.0, -1.0), c + vec2(3.0, -1.0), c + vec2(0.0, 2.0)], t.text, Stroke::NONE));
    true
}

fn stripes(ui: &Ui, rect: Rect, t: Tokens) {
    ui.painter().rect_filled(rect, 0.0, t.chrome);
    for i in 0..6 {
        let y = rect.center().y - 5.0 + i as f32 * 2.0;
        ui.painter().line_segment(
            [pos2(rect.left() + 4.0, y), pos2(rect.right() - 4.0, y)],
            Stroke::new(1.0, if t.appearance == Appearance::System6 { t.text } else { t.bevel_shadow }),
        );
    }
}

pub fn title_bar(app: &mut PhotocraftApp, ui: &mut Ui) -> bool {
    let t = Tokens::get(ui.ctx());
    if t.appearance == Appearance::Modern {
        return false;
    }
    let native_inset = if cfg!(target_os = "macos") && app.integrated_titlebar { 78.0 } else { 0.0 };
    let height = caption_height(ui).max(if native_inset > 0.0 { 28.0 } else { 0.0 });
    egui::Panel::top("historical-caption").exact_size(height).frame(egui::Frame::NONE.fill(t.chrome).inner_margin(0)).show(ui, |ui| {
        let r = ui.max_rect();
        let drag = ui.interact(r, ui.id().with("caption-drag"), Sense::click_and_drag());
        if drag.drag_started() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        if drag.double_clicked() {
            let maximized = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
        }
        let p = ui.painter();
        if t.appearance == Appearance::Windows98 {
            // Vertex colors interpolate the classic active-caption gradient.
            let mut mesh = egui::Mesh::default();
            mesh.colored_vertex(r.left_top(), t.accent);
            mesh.colored_vertex(r.right_top(), t.accent_soft);
            mesh.colored_vertex(r.right_bottom(), t.accent_soft);
            mesh.colored_vertex(r.left_bottom(), t.accent);
            mesh.add_triangle(0, 1, 2);
            mesh.add_triangle(0, 2, 3);
            p.add(mesh);
        } else {
            stripes(ui, r, t);
        }
        let font = crate::theme_fonts::control(ui.ctx(), theme::LayoutTokens::get(ui.ctx()).caption_font_size, true);
        let text = app.session.active().map_or_else(|| "PhotoCraft".into(), |s| format!("{} — PhotoCraft", s.doc.name));
        let max = (r.width() - 80.0 - native_inset).max(1.0);
        let galley = crate::tab_strip::elided(ui, &text, font, if t.appearance == Appearance::Windows98 { t.accent_text } else { t.text }, max);
        let at = if t.appearance == Appearance::Windows98 {
            pos2(r.left() + native_inset + 6.0, r.center().y - galley.size().y / 2.0)
        } else {
            r.center() - galley.size() / 2.0
        };
        if t.appearance != Appearance::Windows98 {
            p.rect_filled(Rect::from_min_size(at - vec2(6.0, 2.0), galley.size() + vec2(12.0, 4.0)), 0.0, t.chrome);
        }
        p.galley(at, galley, if t.appearance == Appearance::Windows98 { t.accent_text } else { t.text });
        if native_inset > 0.0 && t.appearance != Appearance::Windows98 {
            return;
        }
        let close = Rect::from_center_size(
            pos2(if t.appearance == Appearance::Windows98 { r.right() - 11.0 } else { r.left() + 11.0 }, r.center().y),
            vec2(13.0, 13.0),
        );
        let response = ui.interact(close, ui.id().with("caption-close"), Sense::click());
        if t.appearance == Appearance::Windows98 {
            bevel(ui, close, t.chrome, !response.is_pointer_button_down_on(), t);
        } else {
            p.rect_filled(close, 0.0, t.chrome);
            p.rect_stroke(close, 0.0, Stroke::new(1.0, t.text), StrokeKind::Inside);
        }
        if t.appearance == Appearance::Windows98 {
            let x = close.shrink(4.0);
            p.line_segment([x.left_top(), x.right_bottom()], Stroke::new(1.0, t.text));
            p.line_segment([x.right_top(), x.left_bottom()], Stroke::new(1.0, t.text));
        }
        if response.on_hover_text(tl!("Close")).clicked() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    });
    egui::Panel::top("historical-menu")
        .exact_size(theme::LayoutTokens::get(ui.ctx()).menu_bar_height)
        .frame(egui::Frame::NONE.fill(if t.appearance == Appearance::Windows98 { t.chrome } else { t.field }).inner_margin(egui::Margin::symmetric(5, 0)))
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                crate::menus::menu_bar(app, ui);
            });
        });
    true
}

/// App-drawn modal caption; the shell keeps its existing dragging semantics.
pub fn dialog_caption(ui: &mut Ui, title: &str) -> Option<Response> {
    caption(ui, title, false).map(|(drag, _)| drag)
}

/// Floating app window caption. The caller owns movement and close/cancel behavior.
pub fn window_caption(ui: &mut Ui, title: &str) -> Option<(Response, bool)> {
    caption(ui, title, true)
}

// Real font metrics matter for installed faces and unusually large user type sizes.
fn caption_height(ui: &Ui) -> f32 {
    let layout = theme::LayoutTokens::get(ui.ctx());
    let font = crate::theme_fonts::control(ui.ctx(), layout.caption_font_size, true);
    let text_height = ui.fonts_mut(|fonts| fonts.row_height(&font));
    layout.title_bar_height.max(text_height + 2.0)
}

fn caption(ui: &mut Ui, title: &str, closable: bool) -> Option<(Response, bool)> {
    let t = Tokens::get(ui.ctx());
    if t.appearance == Appearance::Modern {
        return None;
    }
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), caption_height(ui)), Sense::drag());
    if t.appearance == Appearance::Windows98 {
        let mut mesh = egui::Mesh::default();
        mesh.colored_vertex(rect.left_top(), t.accent);
        mesh.colored_vertex(rect.right_top(), t.accent_soft);
        mesh.colored_vertex(rect.right_bottom(), t.accent_soft);
        mesh.colored_vertex(rect.left_bottom(), t.accent);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        ui.painter().add(mesh);
    } else {
        stripes(ui, rect, t);
    }
    let color = if t.appearance == Appearance::Windows98 { t.accent_text } else { t.text };
    let reserved = if closable { 42.0 } else { 12.0 };
    let galley = crate::tab_strip::elided(
        ui,
        title,
        crate::theme_fonts::control(ui.ctx(), theme::LayoutTokens::get(ui.ctx()).caption_font_size, true),
        color,
        (rect.width() - reserved).max(1.0),
    );
    let at = if t.appearance == Appearance::Windows98 {
        pos2(rect.left() + 4.0, rect.center().y - galley.size().y / 2.0)
    } else {
        rect.center() - galley.size() / 2.0
    };
    if t.appearance != Appearance::Windows98 {
        ui.painter().rect_filled(Rect::from_min_size(at - vec2(4.0, 1.0), galley.size() + vec2(8.0, 2.0)), 0.0, t.chrome);
    }
    ui.painter().galley(at, galley, color);
    let closed = if closable {
        let close = Rect::from_center_size(
            pos2(if t.appearance == Appearance::Windows98 { rect.right() - 11.0 } else { rect.left() + 11.0 }, rect.center().y),
            vec2(13.0, 13.0),
        );
        let close_response = ui.interact(close, ui.id().with("window-caption-close"), Sense::click());
        close_response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), tl!("Close")));
        if t.appearance == Appearance::Windows98 {
            bevel(ui, close, t.chrome, !close_response.is_pointer_button_down_on(), t);
            let x = close.shrink(4.0);
            ui.painter().line_segment([x.left_top(), x.right_bottom()], Stroke::new(1.0, t.text));
            ui.painter().line_segment([x.right_top(), x.left_bottom()], Stroke::new(1.0, t.text));
        } else {
            ui.painter().rect_filled(close, 0.0, t.chrome);
            ui.painter().rect_stroke(close, 0.0, Stroke::new(1.0, t.text), StrokeKind::Inside);
            if close_response.is_pointer_button_down_on() {
                ui.painter().rect_filled(close.shrink(3.0), 0.0, t.text);
            }
        }
        close_response.on_hover_text(tl!("Close")).clicked()
    } else {
        false
    };
    Some((response, closed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Services;
    use photocraft_engine::Session;

    #[test]
    fn all_historical_presets_are_valid_distinct_reserved_and_agent_selectable() {
        let ctx = egui::Context::default();
        crate::PhotocraftApp::setup_context(&ctx, theme::ThemeKind::Classic);
        let mut app = PhotocraftApp::new(Session::new(), Services::default());
        let mut profiles = Vec::new();
        for id in IDS {
            let pack = preset(id).unwrap();
            assert_eq!(pack.id, id);
            crate::themes::select(&mut app, &ctx, id).unwrap();
            assert_eq!(app.session.prefs().interface.theme_id, id);
            assert_eq!(Tokens::get(&ctx).appearance, pack.appearance);
            assert!(!Tokens::get(&ctx).pro);
            assert!(app.theme_rt.catalog.insert(pack.clone(), true).is_err());
            assert!(crate::themes::command(&mut app, &ctx, "remove", &serde_json::json!({"id":id})).is_err());
            let exported = crate::themes::command(&mut app, &ctx, "export", &serde_json::json!({"id":id})).unwrap();
            assert_eq!(ThemePack::parse(exported["json"].as_str().unwrap()).unwrap(), pack);
            profiles.push(Tokens::get(&ctx).appearance);
        }
        assert_ne!(profiles[0], profiles[1]);
        assert_ne!(profiles[1], profiles[2]);
        assert!(ThemePack::parse(r#"{"version":1,"id":"x","name":"X","base":"classic","appearance":"unknown"}"#).is_err());
        assert!(ThemePack::parse(r#"{"version":1,"id":"x","name":"X","base":"classic","typography":{"family":"unknown"}}"#).is_err());
    }

    #[test]
    fn captions_contain_actual_font_rows_at_all_scales_and_sizes() {
        use egui_kittest::Harness;
        for scale in [1.0, 1.5, 2.0] {
            let mut initialized = false;
            let mut h = Harness::builder().with_size(vec2(900.0, 300.0)).with_pixels_per_point(scale).build_ui_state(
                |ui, measured| {
                    if !initialized {
                        crate::PhotocraftApp::setup_context(ui.ctx(), theme::ThemeKind::Classic);
                        initialized = true;
                        return;
                    }
                    if let Some((response, _)) = window_caption(ui, "PhotoCraft caption") {
                        let font = crate::theme_fonts::control(ui.ctx(), theme::LayoutTokens::get(ui.ctx()).caption_font_size, true);
                        *measured = (response.rect.height(), ui.fonts_mut(|fonts| fonts.row_height(&font)));
                    }
                },
                (0.0_f32, 0.0_f32),
            );
            for id in IDS {
                for size in [12.0, 16.0, 24.0, 64.0] {
                    let mut pack = preset(id).unwrap();
                    pack.typography.caption = Some(size);
                    pack.resolve().unwrap().apply(&h.ctx);
                    h.run_steps(4);
                    let (height, row) = *h.state();
                    assert!(height >= row + 2.0, "{id} {scale} {size}: {height} vs {row}");
                }
            }
        }
    }

    #[test]
    fn historical_controls_remain_clickable_and_fonts_reset_when_switching_back() {
        use egui_kittest::{Harness, kittest::Queryable};
        let mut initialized = false;
        let mut h = Harness::builder().with_size(vec2(600.0, 300.0)).build_ui_state(
            |ui, on| {
                if !initialized {
                    crate::PhotocraftApp::setup_context(ui.ctx(), theme::ThemeKind::Classic);
                    initialized = true;
                    return;
                }
                crate::widgets::checkbox(ui, on, "Enabled");
                crate::widgets::primary_button(ui, "Apply", 80.0);
                crate::widgets::secondary_button(ui, "Cancel", 80.0);
            },
            false,
        );
        for id in IDS {
            preset(id).unwrap().resolve().unwrap().apply(&h.ctx);
            h.run_steps(3);
            let before = *h.state();
            h.get_by_label("Enabled").click();
            h.run_steps(3);
            assert_ne!(*h.state(), before);
            assert!(h.get_by_label("Apply").rect().bottom() < h.get_by_label("Cancel").rect().top());
            assert!(crate::theme_fonts::status(&h.ctx).is_some());
        }
        theme::apply(&h.ctx, theme::ThemeKind::Classic);
        h.run_steps(3);
        assert!(crate::theme_fonts::status(&h.ctx).is_none());
    }
    #[test]
    fn historical_radio_rows_select_by_label_and_preserve_disabled_state_at_all_scales() {
        use egui_kittest::{Harness, kittest::Queryable};
        for scale in [1.0, 1.5, 2.0] {
            for id in IDS {
                let mut initialized = false;
                let mut h = Harness::builder().with_size(vec2(400.0, 250.0)).with_pixels_per_point(scale).build_ui_state(
                    |ui, value| {
                        if !initialized {
                            PhotocraftApp::setup_context(ui.ctx(), theme::ThemeKind::Classic);
                            initialized = true;
                            return;
                        }
                        crate::widgets::radio_value(ui, value, 0, "First choice");
                        crate::widgets::radio_value(ui, value, 1, "Second choice");
                        ui.add_enabled_ui(false, |ui| {
                            crate::widgets::radio_value(ui, value, 2, "Disabled choice");
                        });
                    },
                    0,
                );
                preset(id).unwrap().resolve().unwrap().apply(&h.ctx);
                h.run_steps(3);
                let first = h.get_by_label("First choice").rect();
                let second = h.get_by_label("Second choice").rect();
                assert_eq!(first.height(), 18.0);
                assert!(first.bottom() <= second.top());
                h.get_by_label("Second choice").click();
                h.run_steps(2);
                assert_eq!(*h.state(), 1);
                // A real pointer click on the disabled row must never change the value.
                let at = h.get_by_label("Disabled choice").rect().center();
                h.event(egui::Event::PointerMoved(at));
                h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
                h.run_steps(1);
                h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
                h.run_steps(2);
                assert_eq!(*h.state(), 1);
            }
        }
    }
    #[test]
    fn windows98_keyboard_focus_has_visible_ink() {
        use egui_kittest::{Harness, kittest::Queryable};
        let mut initialized = false;
        let mut h = Harness::builder().with_size(vec2(300.0, 140.0)).wgpu().build_ui(|ui| {
            if !initialized {
                PhotocraftApp::setup_context(ui.ctx(), theme::ThemeKind::Classic);
                initialized = true;
                return;
            }
            crate::widgets::secondary_button(ui, "Focusable action", 140.0);
        });
        preset(IDS[0]).unwrap().resolve().unwrap().apply(&h.ctx);
        h.run_steps(4);
        let before = h.render().unwrap().into_raw();
        h.get_by_label("Focusable action").focus();
        h.run_steps(2);
        let after = h.render().unwrap().into_raw();
        assert!(before != after, "the focused control must show its dotted focus rectangle");
    }
}
