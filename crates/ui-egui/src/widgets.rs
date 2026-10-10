//! Custom widgets for the Photocraft look: cards with pill tabs, thin sliders with round knobs,
//! monospace value fields with dimmed units, toggle switches, primary/secondary buttons.

use egui::{Align2, Color32, CornerRadius, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

use crate::theme::{self, Tokens};

/// Draw a bevelled box (Classic theme) or a flat rounded box.
pub fn surface(ui: &Ui, rect: Rect, fill: Color32, raised: bool) {
    let t = Tokens::get(ui.ctx());
    let p = ui.painter();
    if crate::historical::surface(ui, rect, fill, raised) {
        return;
    }
    p.rect_filled(rect, t.radius_sm, fill);
    if t.bevel {
        let edge = |rect: Rect, hi, lo| {
            p.line_segment([rect.left_bottom(), rect.left_top()], Stroke::new(1.0, hi));
            p.line_segment([rect.left_top(), rect.right_top()], Stroke::new(1.0, hi));
            p.line_segment([rect.right_top(), rect.right_bottom()], Stroke::new(1.0, lo));
            p.line_segment([rect.right_bottom(), rect.left_bottom()], Stroke::new(1.0, lo));
        };
        if t.bevel_width >= 2.0 {
            let (outer_hi, outer_lo, inner_hi, inner_lo) = if raised {
                (t.bevel_light, t.bevel_dark, t.bevel_highlight, t.bevel_shadow)
            } else {
                (t.bevel_shadow, t.bevel_highlight, t.bevel_dark, t.bevel_light)
            };
            edge(rect, outer_hi, outer_lo);
            if rect.width() >= 4.0 && rect.height() >= 4.0 {
                edge(rect.shrink(1.0), inner_hi, inner_lo);
            }
        } else {
            let (hi, lo) = if raised { (t.bevel_highlight, t.bevel_shadow) } else { (t.bevel_shadow, t.bevel_highlight) };
            edge(rect, hi, lo);
        }
    }
}

/// A dock card: rounded container with a header of pill tabs and optional trailing actions.
/// Returns the index of the selected tab.
pub fn card(ui: &mut Ui, id: &str, tabs: &[&str], selected: &mut usize, body: impl FnOnce(&mut Ui, usize)) {
    let _ = card_ex(ui, id, tabs, selected, false, body);
}

/// What happened on a card's tab strip this frame (see [`card_ex`]).
pub struct CardResponse {
    /// The tab strip background: drag to move the group, double-click to collapse it.
    pub strip: Response,
    /// The panel menu button (hamburger in Pro, ellipsis in Studio).
    pub menu: Response,
    /// A tab was double-clicked (Photoshop collapses the group).
    pub tab_double_clicked: bool,
    /// Rects of the tabs on the strip, `(tab index, rect)`; tabs that don't fit are in the
    /// chevron menu instead (#151).
    pub tabs: Vec<(usize, Rect)>,
    /// The » overflow button, when some tabs didn't fit.
    pub chevron: Option<Rect>,
}

/// [`card`] that can be collapsed to its tab strip and reports strip and menu interactions.
pub fn card_ex(ui: &mut Ui, id: &str, tabs: &[&str], selected: &mut usize, collapsed: bool, body: impl FnOnce(&mut Ui, usize)) -> CardResponse {
    let t = Tokens::get(ui.ctx());
    if t.pro {
        return pro_panel(ui, id, tabs, selected, collapsed, body);
    }
    let layout = theme::LayoutTokens::get(ui.ctx());
    let padding = layout.panel_padding.round() as i8;
    let frame =
        egui::Frame::NONE.fill(t.card).stroke(Stroke::new(1.0, t.card_border)).corner_radius(CornerRadius::same(t.radius as u8)).inner_margin(egui::Margin {
            left: padding,
            right: padding,
            top: (padding - 2).max(0),
            bottom: if collapsed { (padding - 2).max(0) } else { (padding + 2).min(34) },
        });
    let out = frame
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            // Registered before the tabs so they keep their clicks; drags fall through to it.
            let strip_rect = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), (layout.tab_height - 2.0).max(14.0)));
            let strip = ui.interact(strip_rect, ui.id().with((id, "strip")), Sense::click_and_drag());
            // Pill tabs left of the menu button; they elide or overflow into a chevron (#151).
            let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), layout.tab_height), Sense::hover());
            let tip = crate::i18n::fmt(tl!("{name} options"), &[("name", tl!(tabs.get(*selected).copied().unwrap_or(id)))]);
            let menu_size = (layout.tab_height - 2.0).clamp(14.0, 22.0);
            let menu_rect = Rect::from_min_max(pos2(row.right() - menu_size, row.top() + 1.0), pos2(row.right(), row.bottom() - 1.0));
            let menu = crate::icons::button(&mut ui.new_child(egui::UiBuilder::new().max_rect(menu_rect)), "ellipsis", menu_size, false, &tip);
            let area = Rect::from_min_max(row.min, pos2((menu_rect.left() - 4.0).max(row.left()), row.bottom()));
            let tabs_out = crate::tab_strip::pill_tabs(ui, ui.id().with((id, "tabs")), area, tabs, selected);
            if !collapsed {
                ui.add_space(layout.panel_gap);
                body(ui, *selected);
            }
            CardResponse { strip, menu, tab_double_clicked: tabs_out.double_clicked, tabs: tabs_out.tabs, chevron: tabs_out.chevron }
        })
        .inner;
    ui.add_space(layout.panel_gap);
    out
}

/// Photoshop-grammar panel group: dark tab strip with flat tabs, flat body, hamburger menu.
fn pro_panel(ui: &mut Ui, id: &str, tabs: &[&str], selected: &mut usize, collapsed: bool, body: impl FnOnce(&mut Ui, usize)) -> CardResponse {
    let t = Tokens::get(ui.ctx());
    let width = ui.available_width();
    let layout = theme::LayoutTokens::get(ui.ctx());
    // Tab strip. Its background senses drags (move the group) and double-clicks (collapse);
    // the tabs, registered after it, keep their clicks.
    let (strip, _) = ui.allocate_exact_size(vec2(width, layout.tab_height), Sense::hover());
    let strip_resp = ui.interact(strip, ui.id().with((id, "strip")), Sense::click_and_drag());
    let rounding = if collapsed { CornerRadius::same(3) } else { CornerRadius { nw: 3, ne: 3, sw: 0, se: 0 } };
    ui.painter().rect_filled(strip, rounding, t.tab_strip);
    // Panel menu (hamburger); the tabs stay left of it, eliding or overflowing (#151).
    let menu = Rect::from_center_size(pos2(strip.right() - 14.0, strip.center().y), vec2(20.0, 18.0));
    let tabs_out = crate::tab_strip::pro_tabs(ui, ui.id().with((id, "tabs")), strip, menu.left(), tabs, selected, collapsed);
    let mresp = ui.interact(menu, ui.id().with((id, "menu")), Sense::click());
    let c = if mresp.hovered() { t.text } else { t.text_faint };
    for k in 0..3 {
        let y = menu.center().y - 3.5 + k as f32 * 3.5;
        ui.painter().line_segment([pos2(menu.center().x - 5.0, y), pos2(menu.center().x + 5.0, y)], Stroke::new(1.0, c));
    }
    // Body.
    if !collapsed {
        egui::Frame::NONE
            .fill(t.card)
            .corner_radius(CornerRadius { nw: 0, ne: 0, sw: 3, se: 3 })
            .inner_margin(egui::Margin::same(layout.panel_padding.round() as i8))
            .show(ui, |ui| {
                ui.set_width((width - layout.panel_padding.round() * 2.0).max(0.0));
                body(ui, *selected);
            });
    }
    ui.add_space(layout.panel_gap);
    CardResponse { strip: strip_resp, menu: mresp, tab_double_clicked: tabs_out.double_clicked, tabs: tabs_out.tabs, chevron: tabs_out.chevron }
}

pub fn pill_tab(ui: &mut Ui, label: &str, selected: bool) -> Response {
    let t = Tokens::get(ui.ctx());
    let font = crate::theme_fonts::control(ui.ctx(), theme::LayoutTokens::get(ui.ctx()).tab_font_size, false);
    let galley = ui.painter().layout_no_wrap(tl!(label).to_owned(), font, t.text);
    let size = vec2(galley.size().x + 20.0, theme::LayoutTokens::get(ui.ctx()).tab_height);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if selected {
        surface(ui, rect, t.hover, true);
        if !t.bevel {
            ui.painter().rect_stroke(rect, t.radius_sm, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
        }
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, t.radius_sm, t.hover.gamma_multiply(0.6));
    }
    let color = if selected { t.text } else { t.text_dim };
    ui.painter().galley_with_override_text_color(rect.center() - galley.size() / 2.0, galley, color);
    resp
}

/// Monospace numeric field with a dimmed unit suffix, Photoshop style. Drag to scrub.
pub fn value_field(ui: &mut Ui, value: &mut f32, range: std::ops::RangeInclusive<f32>, suffix: &str, width: f32) -> Response {
    let t = Tokens::get(ui.ctx());
    let metrics = theme::LayoutTokens::get(ui.ctx());
    let font = crate::theme_fonts::number(ui.ctx(), metrics.field_font_size);
    let calibrated = crate::theme_fonts::chrome_line_height(ui.ctx(), &font).is_some();
    let vertical_padding = if calibrated { 1.0 } else { 2.0 };
    let height = if calibrated {
        metrics.field_height.max(ui.painter().layout_no_wrap("0".into(), font.clone(), t.text).size().y + 2.0 * vertical_padding)
    } else {
        metrics.field_height
    };
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    surface(ui, rect, t.field, false);
    if !t.bevel {
        ui.painter().rect_stroke(rect, t.radius_sm, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
    }
    let suffix_w = if suffix.is_empty() { 0.0 } else { 16.0 };
    let field = Rect::from_min_max(rect.min + vec2(4.0, vertical_padding), rect.max - vec2(4.0 + suffix_w, vertical_padding));
    // Small ranges (gamma 0.01–9.99, 0–1 centres) need two decimals and a finer drag, like Photoshop.
    let fine = range.end() - range.start() <= 10.0;
    // new_child (not scope_builder): a scope would move the parent cursor back to the child rect.
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(field));
    let resp = {
        let ui = &mut child;
        {
            ui.style_mut().visuals.widgets.inactive.bg_fill = Color32::TRANSPARENT;
            ui.style_mut().visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
            ui.style_mut().visuals.widgets.inactive.bg_stroke = Stroke::NONE;
            ui.style_mut().visuals.widgets.hovered.bg_stroke = Stroke::NONE;
            ui.style_mut().visuals.widgets.hovered.weak_bg_fill = Color32::TRANSPARENT;
            ui.style_mut().override_font_id = Some(font);
            // DragValue is a button internally: its stock minimum/padding must fit our painted field.
            ui.spacing_mut().interact_size.y = field.height();
            ui.spacing_mut().button_padding.y = 0.0;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_sized(
                    field.size(),
                    egui::DragValue::new(value)
                        .range(range)
                        .speed(if fine { 0.01 } else { 0.5 })
                        .custom_formatter(|v, _| if fine { fmt_num2(v) } else { fmt_num(v) }),
                )
            })
            .inner
        }
    };
    if !suffix.is_empty() {
        ui.painter().text(
            pos2(rect.right() - 6.0, rect.center().y),
            Align2::RIGHT_CENTER,
            suffix,
            crate::theme_fonts::number(ui.ctx(), (theme::LayoutTokens::get(ui.ctx()).field_font_size - 1.0).max(6.0)),
            t.text_faint,
        );
    }
    resp
}

/// Setting slider with theme-specific chrome. `gradient` paints the edited color range.
pub fn slider(ui: &mut Ui, value: &mut f32, range: std::ops::RangeInclusive<f32>, gradient: Option<&[Color32]>) -> Response {
    let t = Tokens::get(ui.ctx());
    let width = ui.available_width().max(60.0);
    let historical = t.appearance != theme::Appearance::Modern;
    let metrics = theme::LayoutTokens::get(ui.ctx());
    let [thumb_width, thumb_height] = metrics.slider_thumb_size;
    let height = if historical { (thumb_height.max(metrics.slider_track_height) + 4.0).max(18.0) } else { 18.0 };
    let (rect, mut resp) = ui.allocate_exact_size(vec2(width, height), Sense::click_and_drag());
    let (lo, hi) = (*range.start(), *range.end());
    let span = hi - lo;
    let adjustable = lo.is_finite() && hi.is_finite() && span.is_finite() && span > 0.0;
    let inset = if historical { thumb_width + 4.0 } else { 14.0 };
    let track_height = if historical {
        metrics.slider_track_height.max(if gradient.is_some() { 5.0 } else { 2.0 })
    } else if gradient.is_some() {
        5.0
    } else {
        3.0
    };
    let track = Rect::from_center_size(rect.center(), vec2(rect.width() - inset, track_height));
    if let Some(p) = resp.interact_pointer_pos()
        && adjustable
        && (resp.dragged() || resp.clicked())
    {
        let f = ((p.x - track.left()) / track.width()).clamp(0.0, 1.0);
        let nv = lo + f * (hi - lo);
        if !value.is_finite() || (nv - *value).abs() > f32::EPSILON {
            *value = nv;
            resp.mark_changed();
        }
    }
    if resp.clicked() && adjustable {
        resp.request_focus();
    }
    if adjustable && resp.has_focus() && ui.is_enabled() {
        let base = if value.is_finite() { value.clamp(lo, hi) } else { lo };
        let keyboard = ui.input_mut(|i| {
            let modifiers = if i.modifiers.shift { egui::Modifiers::SHIFT } else { egui::Modifiers::NONE };
            if i.consume_key(modifiers, egui::Key::Home) {
                Some(lo)
            } else if i.consume_key(modifiers, egui::Key::End) {
                Some(hi)
            } else {
                let increase = i.consume_key(modifiers, egui::Key::ArrowRight) || i.consume_key(modifiers, egui::Key::ArrowUp);
                let decrease = i.consume_key(modifiers, egui::Key::ArrowLeft) || i.consume_key(modifiers, egui::Key::ArrowDown);
                (increase || decrease)
                    .then(|| (base + (i32::from(increase) - i32::from(decrease)) as f32 * span / if i.modifiers.shift { 10.0 } else { 100.0 }).clamp(lo, hi))
            }
        });
        if let Some(next) = keyboard
            && (!value.is_finite() || next != *value)
        {
            *value = next;
            resp.mark_changed();
        }
    }
    resp.widget_info(|| egui::WidgetInfo::slider(ui.is_enabled() && adjustable, if value.is_finite() { f64::from(*value) } else { 0.0 }, "Slider"));
    let f = if adjustable && value.is_finite() { ((*value - lo) / span).clamp(0.0, 1.0) } else { 0.0 };
    let knob_x = track.left() + f * track.width();
    let painter = ui.painter();
    match gradient {
        Some(colors) if colors.len() >= 2 => {
            let n = colors.len() - 1;
            for (i, w) in colors.windows(2).enumerate() {
                let x0 = track.left() + track.width() * i as f32 / n as f32;
                let x1 = track.left() + track.width() * (i + 1) as f32 / n as f32;
                let mut mesh = egui::Mesh::default();
                let r = Rect::from_min_max(pos2(x0, track.top()), pos2(x1, track.bottom()));
                mesh.colored_vertex(r.left_top(), w[0]);
                mesh.colored_vertex(r.right_top(), w[1]);
                mesh.colored_vertex(r.right_bottom(), w[1]);
                mesh.colored_vertex(r.left_bottom(), w[0]);
                mesh.add_triangle(0, 1, 2);
                mesh.add_triangle(0, 2, 3);
                painter.add(mesh);
            }
        }
        _ => {
            if !crate::historical::slider_track(ui, track) {
                painter.rect_filled(track, 2.0, t.field_border);
                let filled = Rect::from_min_max(track.min, pos2(knob_x, track.max.y));
                painter.rect_filled(filled, 2.0, if t.bevel { t.accent } else { t.text_dim });
            }
        }
    }
    let knob = pos2(knob_x, rect.center().y);
    if !crate::historical::slider_indicator(ui, rect, knob, &resp) {
        if t.bevel {
            let kr = Rect::from_center_size(knob, vec2(10.0, 16.0));
            surface(ui, kr, t.card, true);
        } else {
            let kr = if t.pro { 6.0 } else { 7.0 };
            painter.circle_filled(knob + vec2(0.0, 1.0), kr + 0.5, Color32::from_black_alpha(90));
            painter.circle_filled(knob, kr, if t.pro { Color32::from_gray(236) } else { Color32::WHITE });
            if t.pro {
                painter.circle_stroke(knob, kr, Stroke::new(1.0, Color32::from_gray(40)));
            }
            if resp.hovered() || resp.dragged() {
                painter.circle_stroke(knob, 9.5, Stroke::new(2.0, t.accent_soft));
            }
        }
    }
    resp
}

/// Labelled slider row: `Label ........ [value field]` above a full-width thin slider.
pub fn slider_row(ui: &mut Ui, label: &str, value: &mut f32, range: std::ops::RangeInclusive<f32>, suffix: &str, gradient: Option<&[Color32]>) -> Response {
    let t = Tokens::get(ui.ctx());
    let mut changed_resp = None;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(tl!(label)).color(t.text_dim));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            changed_resp = Some(value_field(ui, value, range.clone(), suffix, 74.0));
        });
    });
    let s = slider(ui, value, range, gradient);
    let mut r = s.clone();
    if let Some(v) = changed_resp
        && v.changed()
    {
        r.mark_changed();
    }
    ui.add_space(4.0);
    r
}

/// iOS-style toggle switch.
pub fn toggle(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    if t.pro || t.appearance != theme::Appearance::Modern {
        return checkbox(ui, on, label);
    }
    let mut resp = ui
        .horizontal(|ui| {
            let (rect, resp) = ui.allocate_exact_size(vec2(30.0, 17.0), Sense::click());
            let how_on = ui.ctx().animate_bool(resp.id, *on);
            let bg = if *on { t.accent } else { t.field_border };
            if t.bevel {
                surface(ui, rect, if *on { t.accent } else { t.field }, false);
            } else {
                ui.painter().rect_filled(rect, 8.5, bg);
            }
            let x = egui::lerp((rect.left() + 8.5)..=(rect.right() - 8.5), how_on);
            ui.painter().circle_filled(pos2(x, rect.center().y), 6.5, Color32::WHITE);
            ui.label(egui::RichText::new(tl!(label)).color(if *on { t.text } else { t.text_dim }));
            resp
        })
        .inner;
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    resp
}

/// Big white primary button.
pub fn primary_button(ui: &mut Ui, label: &str, min_width: f32) -> Response {
    let t = Tokens::get(ui.ctx());
    button_impl(ui, label, min_width, t.primary_bg, t.primary_text, true, None)
}

pub fn secondary_button(ui: &mut Ui, label: &str, min_width: f32) -> Response {
    let t = Tokens::get(ui.ctx());
    button_impl(ui, label, min_width, t.field, t.text, false, None)
}

/// A compact chrome action that fits a bar without changing dialog button metrics.
pub fn chrome_button(ui: &mut Ui, label: &str, height: f32) -> Response {
    let t = Tokens::get(ui.ctx());
    button_impl(ui, label, 0.0, t.field, t.text, false, Some(height))
}

fn button_impl(ui: &mut Ui, label: &str, min_width: f32, bg: Color32, fg: Color32, primary: bool, height: Option<f32>) -> Response {
    let t = Tokens::get(ui.ctx());
    let layout = theme::LayoutTokens::get(ui.ctx());
    let galley = crate::tab_strip::elided(ui, tl!(label), crate::theme_fonts::control(ui.ctx(), layout.button_font_size, false), fg, f32::INFINITY);
    let h = height.filter(|h| h.is_finite()).unwrap_or(layout.button_height).max(galley.size().y + 4.0);
    let size = vec2((galley.size().x + layout.button_padding_x * 2.0).max(min_width), h);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    // Painted text: name the button for accessibility (and so tests and agents can find it).
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label));
    if crate::historical::button(ui, rect, &resp, primary, galley.clone()) {
        return resp;
    }
    if t.pro {
        // Spectrum buttons: fully rounded; primary = filled accent, secondary = outline.
        let down = resp.is_pointer_button_down_on();
        let r = h / 2.0;
        if primary {
            let fill = if down {
                bg.gamma_multiply(0.8)
            } else if resp.hovered() {
                bg.gamma_multiply(0.9)
            } else {
                bg
            };
            ui.painter().rect_filled(rect, r, fill);
        } else {
            if resp.hovered() || down {
                ui.painter().rect_filled(rect, r, t.hover);
            }
            ui.painter().rect_stroke(rect, r, Stroke::new(1.5, if resp.hovered() { t.text } else { t.text_dim }), StrokeKind::Inside);
        }
        ui.painter().galley(rect.center() - galley.size() / 2.0, galley, fg);
        return resp;
    }
    let fill = if resp.is_pointer_button_down_on() {
        bg.gamma_multiply(0.85)
    } else if resp.hovered() {
        if primary { bg.gamma_multiply(0.93) } else { t.hover }
    } else {
        bg
    };
    surface(ui, rect, fill, !resp.is_pointer_button_down_on());
    if !t.bevel && !primary {
        ui.painter().rect_stroke(rect, t.radius_sm, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
    }
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, fg);
    resp
}

/// Small caps section label.
pub fn section_label(ui: &mut Ui, text: &str) {
    let t = Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(tl!(text)).font(theme::medium(11.5)).color(t.text_faint));
}

/// Hairline separator.
pub fn hairline(ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().line_segment([r.left_center(), r.right_center()], Stroke::new(1.0, t.separator));
}

/// Vertical hairline for horizontal layouts.
pub fn vline(ui: &mut Ui, height: f32) {
    let t = Tokens::get(ui.ctx());
    let (r, _) = ui.allocate_exact_size(vec2(9.0, height), Sense::hover());
    ui.painter().line_segment([r.center_top(), r.center_bottom()], Stroke::new(1.0, t.separator));
}

/// Hue spectrum stops for colour sliders.
pub fn hue_stops() -> Vec<Color32> {
    (0..=12)
        .map(|i| {
            let h = i as f32 / 12.0;
            let rgb = egui::ecolor::Hsva::new(h, 0.85, 1.0, 1.0).to_srgb();
            Color32::from_rgb(rgb[0], rgb[1], rgb[2])
        })
        .collect()
}

/// A compact labelled dropdown in the studio style.
pub fn dropdown<T: PartialEq + Clone>(ui: &mut Ui, id: &str, current: &mut T, options: &[(T, &str)], width: f32) -> bool {
    let t = Tokens::get(ui.ctx());
    let label = options.iter().find(|(v, _)| v == current).map(|(_, l)| tl!(l)).unwrap_or("—");
    let mut changed = false;
    let historical = t.appearance != theme::Appearance::Modern;
    let width = if historical {
        let available = ui.available_width().max(0.0);
        if width.is_finite() { width.clamp(0.0, available) } else { available }
    } else {
        width
    };
    let mut draw = |ui: &mut Ui| {
        let combo = egui::ComboBox::from_id_salt(id).selected_text(label).width(width).height(420.0).icon(chevron_icon);
        let combo = if historical { combo.truncate() } else { combo };
        combo
            .show_ui(ui, |ui| {
                for (v, l) in options {
                    let selected = v == current;
                    let text = egui::RichText::new(tl!(l));
                    let text = if selected && (t.bevel || historical) { text.color(t.accent_text) } else { text };
                    if ui.selectable_label(selected, text).clicked() {
                        *current = v.clone();
                        changed = true;
                    }
                }
            })
            .response
            .on_hover_text(label);
    };
    if historical {
        // ComboBox width is a minimum. Constrain its text layout separately so a
        // long preset/translated label cannot enlarge a compact Properties card.
        ui.scope(|ui| {
            ui.set_width(width);
            draw(ui);
        });
    } else {
        draw(ui);
    }
    changed
}

/// A radio control with historical painting and the same semantics as egui's radio.
pub fn radio(ui: &mut Ui, on: bool, label: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    if t.appearance == theme::Appearance::Modern {
        return ui.radio(on, tl!(label));
    }
    let galley = crate::tab_strip::elided(
        ui,
        tl!(label),
        crate::theme_fonts::control(ui.ctx(), theme::LayoutTokens::get(ui.ctx()).button_font_size, false),
        t.text,
        f32::INFINITY,
    );
    let mark_size = if t.appearance == theme::Appearance::Windows98 { 13.0 } else { 12.0 };
    let (rect, response) = ui.allocate_exact_size(vec2(mark_size + 5.0 + galley.size().x, 18.0_f32.max(galley.size().y)), Sense::click());
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::RadioButton, ui.is_enabled(), on, tl!(label)));
    let mark = Rect::from_center_size(pos2(rect.left() + mark_size / 2.0, rect.center().y), vec2(mark_size, mark_size));
    crate::historical::radio(ui, mark, on, &response);
    let at = pos2(rect.left() + mark_size + 5.0, rect.center().y - galley.size().y / 2.0);
    let text_rect = Rect::from_min_size(at, galley.size());
    ui.painter().galley_with_override_text_color(at, galley, t.text);
    crate::historical::disabled_stipple(ui, text_rect, t.chrome);
    response
}

pub fn radio_value<T: PartialEq>(ui: &mut Ui, current: &mut T, value: T, label: &str) -> Response {
    let mut response = radio(ui, *current == value, label);
    if response.clicked() && *current != value {
        *current = value;
        response.mark_changed();
    }
    response
}

/// Paint a small checkerboard (transparency) in `rect`.
pub fn checker(painter: &egui::Painter, rect: Rect, cell: f32) {
    painter.rect_filled(rect, 0.0, Color32::from_gray(250));
    let nx = (rect.width() / cell).ceil() as i32;
    let ny = (rect.height() / cell).ceil() as i32;
    for j in 0..ny {
        for i in 0..nx {
            if (i + j) % 2 == 1 {
                let r = Rect::from_min_size(Pos2::new(rect.left() + i as f32 * cell, rect.top() + j as f32 * cell), Vec2::splat(cell)).intersect(rect);
                painter.rect_filled(r, 0.0, Color32::from_gray(214));
            }
        }
    }
}

/// Spectrum-style checkbox (blue when checked).
pub fn checkbox(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let mut resp = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = if t.appearance == theme::Appearance::Modern { 6.0 } else { 5.0 };
            let (width, height) = match t.appearance {
                theme::Appearance::Modern => (14.0, 14.0),
                theme::Appearance::Windows98 => (13.0, 18.0),
                theme::Appearance::Platinum | theme::Appearance::System6 => (12.0, 18.0),
            };
            let (rect, resp) = ui.allocate_exact_size(vec2(width, height), Sense::click());
            let p = ui.painter();
            if crate::historical::checkbox(ui, rect, *on, &resp) {
                // The historical renderer paints the box and checked state.
            } else if *on {
                p.rect_filled(rect, 2.0, t.accent);
                let a = rect.left_center() + vec2(3.0, 0.5);
                let b = rect.center_bottom() + vec2(-1.0, -3.5);
                let c = rect.right_top() + vec2(-3.0, 3.5);
                p.line_segment([a, b], Stroke::new(1.8, Color32::WHITE));
                p.line_segment([b, c], Stroke::new(1.8, Color32::WHITE));
            } else {
                p.rect_filled(rect, 2.0, t.field);
                p.rect_stroke(rect, 2.0, Stroke::new(1.5, if resp.hovered() { t.text_dim } else { t.text_faint }), StrokeKind::Inside);
            }
            let l = ui.add(egui::Label::new(egui::RichText::new(tl!(label)).color(t.text_dim)).sense(Sense::click()));
            crate::historical::disabled_stipple(ui, l.rect, t.chrome);
            resp.union(l)
        })
        .inner;
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    resp
}

/// Small chevron for dropdowns (replaces egui's filled triangle).
pub fn chevron_icon(ui: &Ui, rect: Rect, visuals: &egui::style::WidgetVisuals, _open: bool) {
    if crate::historical::dropdown_arrow(ui, rect) {
        return;
    }
    let c = rect.center();
    let s = 3.2;
    let stroke = Stroke::new(1.3, visuals.fg_stroke.color.gamma_multiply(0.8));
    ui.painter().line_segment([c + vec2(-s, -s * 0.5), c + vec2(0.0, s * 0.5)], stroke);
    ui.painter().line_segment([c + vec2(0.0, s * 0.5), c + vec2(s, -s * 0.5)], stroke);
}

/// Photoshop-style numbers: "100", "12.5" (never "100.0").
pub fn fmt_num(v: f64) -> String {
    let r = (v * 10.0).round() / 10.0;
    if (r - r.round()).abs() < 1e-9 { format!("{}", r.round() as i64) } else { format!("{r:.1}") }
}

/// Two-decimal variant of [`fmt_num`] for small ranges: `1.05`, `0.78`, `2`.
pub fn fmt_num2(v: f64) -> String {
    let r = (v * 100.0).round() / 100.0;
    format!("{r:.2}").trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn historical_numeric_text_fits_its_painted_field_at_all_scales() {
        for source in [
            include_str!("../../../assets/themes/windows-98-se.json"),
            include_str!("../../../assets/themes/mac-os-9-1.json"),
            include_str!("../../../assets/themes/system-6.json"),
        ] {
            for scale in [1.0, 1.5, 2.0] {
                let pack = crate::theme_pack::ThemePack::parse(source).unwrap().resolve().unwrap();
                let mut initialized = false;
                let mut value = 100.0;
                let mut h = egui_kittest::Harness::builder().with_pixels_per_point(scale).build_ui(|ui| {
                    if !initialized {
                        crate::PhotocraftApp::setup_context(ui.ctx(), crate::theme::ThemeKind::Classic);
                        initialized = true;
                        return;
                    }
                    pack.apply(ui.ctx());
                    let field = ui.scope(|ui| super::value_field(ui, &mut value, 0.0..=999.0, "%", 78.0));
                    assert!(field.response.rect.contains_rect(field.inner.rect), "{scale}: {:?} outside {:?}", field.inner.rect, field.response.rect);
                });
                h.run_steps(3);
            }
        }
    }

    #[test]
    fn historical_dropdown_long_labels_fit_requested_width_and_keep_selection() {
        use egui_kittest::kittest::Queryable;
        const LONG: &str = "An unusually long color lookup preset name with additional translated description";
        for source in [
            include_str!("../../../assets/themes/windows-98-se.json"),
            include_str!("../../../assets/themes/mac-os-9-1.json"),
            include_str!("../../../assets/themes/system-6.json"),
        ] {
            for scale in [1.0, 1.5, 2.0] {
                let resolved = crate::theme_pack::ThemePack::parse(source).unwrap().resolve().unwrap();
                let mut initialized = false;
                let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(320.0, 240.0)).with_pixels_per_point(scale).build_ui(|ui| {
                    if !initialized {
                        crate::PhotocraftApp::setup_context(ui.ctx(), crate::theme::ThemeKind::Classic);
                        initialized = true;
                        return;
                    }
                    resolved.apply(ui.ctx());
                    let mut value = ui.data(|d| d.get_temp::<i32>(egui::Id::new("chosen"))).unwrap_or(0);
                    ui.horizontal(|ui| {
                        let row = ui.scope(|ui| {
                            super::dropdown(ui, "long-dropdown", &mut value, &[(0, LONG), (1, "Other Entry")], 120.0);
                        });
                        assert!(row.response.rect.width() <= 120.5, "{scale}: {:?}", row.response.rect);
                        ui.label("Following control");
                    });
                    ui.data_mut(|d| d.insert_temp(egui::Id::new("chosen"), value));
                });
                h.run_steps(3);
                h.get_by_value(LONG).click();
                h.run_steps(3);
                h.get_by_label("Other Entry").click();
                h.run_steps(3);
                assert_eq!(h.ctx.data(|d| d.get_temp::<i32>(egui::Id::new("chosen"))), Some(1));
            }
        }
    }

    #[test]
    fn classic_dropdown_selected_text_uses_the_runtime_selection_color() {
        use egui_kittest::kittest::Queryable;
        let resolved = crate::theme_pack::ThemePack::parse(r##"{"version":1,"id":"test","name":"Test","base":"classic","colors":{"accent_text":"#E9F0FA"}}"##)
            .unwrap()
            .resolve()
            .unwrap();
        let mut current = 0;
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(320.0, 240.0)).build_ui(move |ui| {
            resolved.apply(ui.ctx());
            super::dropdown(ui, "selection-color", &mut current, &[(0, "Selected Entry"), (1, "Other Entry")], 200.0);
        });
        h.run_steps(2);
        h.get_by_value("Selected Entry").click();
        h.run_steps(2);
        assert!(
            h.output().shapes.iter().any(|clipped| match &clipped.shape {
                egui::epaint::Shape::Text(text) =>
                    text.galley.job.text == "Selected Entry"
                        && text.galley.job.sections.iter().any(|section| section.format.color == egui::Color32::from_rgb(233, 240, 250)),
                _ => false,
            }),
            "selected popup glyphs must use the configured foreground rather than black"
        );
    }

    #[test]
    fn two_decimal_numbers_trim_zeros() {
        assert_eq!(super::fmt_num2(1.05), "1.05");
        assert_eq!(super::fmt_num2(0.78), "0.78");
        assert_eq!(super::fmt_num2(0.5), "0.5");
        assert_eq!(super::fmt_num2(2.0), "2");
    }

    #[test]
    fn numbers_drop_trailing_zero() {
        assert_eq!(super::fmt_num(100.0), "100");
        assert_eq!(super::fmt_num(12.46), "12.5");
        assert_eq!(super::fmt_num(-3.0), "-3");
    }
}
