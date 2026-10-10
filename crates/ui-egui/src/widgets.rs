//! Custom widgets for the Photocraft look: cards with pill tabs, thin sliders with round knobs,
//! monospace value fields with dimmed units, toggle switches, primary/secondary buttons.

use egui::{Color32, CornerRadius, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

use crate::theme::{self, Tokens};

/// An accent insertion line on one edge of `r` while a drag hovers it (vertical: on its left or,
/// `after`, right edge; else on its top or bottom).
pub fn drop_line(ui: &Ui, r: Rect, after: bool, vertical: bool, t: &Tokens) {
    let s = Stroke::new(2.0, t.accent);
    if vertical {
        let x = if after { r.right() } else { r.left() };
        ui.painter().line_segment([pos2(x, r.top() + 2.0), pos2(x, r.bottom() - 2.0)], s);
    } else {
        let y = if after { r.bottom() } else { r.top() };
        ui.painter().line_segment([pos2(r.left() + 4.0, y), pos2(r.right() - 4.0, y)], s);
    }
}

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
    // Measure the selected face, including compatibility calibration and custom
    // sizes. Nominal font size alone does not describe its actual line box.
    let number_galley = ui.painter().layout_no_wrap(
        if range.end() - range.start() <= 10.0 { fmt_num2(f64::from(*value)) } else { fmt_num(f64::from(*value)) },
        font.clone(),
        t.text,
    );
    let suffix_font = crate::theme_fonts::number(ui.ctx(), (metrics.field_font_size - 1.0).max(6.0));
    let suffix_galley = ui.painter().layout_no_wrap(suffix.to_owned(), suffix_font, t.text_faint);
    let height = metrics.field_height.max(number_galley.size().y.max(suffix_galley.size().y) + 2.0 * vertical_padding);
    let suffix_w = if suffix.is_empty() { 0.0 } else { suffix_galley.size().x + 8.0 };
    let width = width.max(number_galley.size().x + suffix_w + 8.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    surface(ui, rect, t.field, false);
    if !t.bevel {
        ui.painter().rect_stroke(rect, t.radius_sm, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
    }
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
            ui.spacing_mut().button_padding = egui::Vec2::ZERO;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let layout = egui::Layout::centered_and_justified(ui.layout().main_dir());
                ui.allocate_ui_with_layout(field.size(), layout, |ui| number_edit(ui, value, range, fine)).inner
            })
            .inner
        }
    };
    if !suffix.is_empty() {
        ui.painter().galley(pos2(rect.right() - 6.0 - suffix_galley.size().x, rect.center().y - suffix_galley.size().y / 2.0), suffix_galley, t.text_faint);
    }
    resp
}

/// The number in a [`value_field`]. A typed number applies as it's typed; arithmetic waits for
/// Enter, Tab or click-away, because per keystroke `5/2` would land first and a caller that
/// rounds it would cut `5/2*2` short. `changed()` means a new value, not just a keystroke.
fn number_edit(ui: &mut Ui, value: &mut f32, range: std::ops::RangeInclusive<f32>, fine: bool) -> Response {
    let (id, ctx) = (ui.next_auto_id(), ui.ctx().clone());
    let held = id.with("arithmetic");
    let math = ui.memory(|m| m.has_focus(id)) && ui.data(|d| d.get_temp(held)).unwrap_or(false);
    ui.data_mut(|d| d.insert_temp(held, math));
    let before = *value;
    let mut resp = ui.add(
        egui::DragValue::new(value)
            .range(range)
            .speed(if fine { 0.01 } else { 0.5 })
            .custom_formatter(move |v, _| if fine { fmt_num2(v) } else { fmt_num(v) })
            .update_while_editing(!math)
            // Focus is read when parsing, not above: Tab hands it on inside `ui.add`.
            .custom_parser(move |s| {
                let v = parse_num(s);
                if ctx.memory(|m| m.has_focus(id)) && plain(s).is_none() {
                    // Still typing arithmetic: hold it, and stop applying keystrokes once it parses.
                    ctx.data_mut(|d| d.insert_temp(held, v.is_some()));
                    return None;
                }
                v
            }),
    );
    resp.flags.set(egui::response::Flags::CHANGED, *value != before);
    resp
}

/// Thin-track slider with a round knob. `gradient` paints the track (e.g. hue spectrum).
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

/// What a dialog button does, which decides where the platform's button order puts it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonRole {
    /// The default answer (OK, Save, Yes), drawn as the primary button.
    Default,
    /// Another answer that closes the dialog (Don't Save, No).
    Alternate,
    /// Closes the dialog without acting.
    Cancel,
    /// Acts but keeps the dialog open (Apply).
    Apply,
}

impl ButtonRole {
    /// Position from the left in the platform's order. Windows and Linux put the default action
    /// first (OK Cancel Apply, Yes No Cancel); macOS puts it last, in the corner, with Cancel beside
    /// it and the other answers further left (Don't Save, Cancel, Save).
    fn slot(self, mac: bool) -> u8 {
        match (self, mac) {
            (Self::Default, false) | (Self::Alternate, true) => 0,
            (Self::Alternate, false) | (Self::Cancel, true) => 1,
            (Self::Cancel, false) | (Self::Apply, true) => 2,
            (Self::Apply, false) | (Self::Default, true) => 3,
        }
    }
}

/// One button of a [`dialog_buttons`] row.
#[derive(Clone, Copy)]
pub struct DialogButton<'a> {
    pub role: ButtonRole,
    pub label: &'a str,
    pub min_width: f32,
    pub enabled: bool,
}

impl<'a> DialogButton<'a> {
    pub fn new(role: ButtonRole, label: &'a str, min_width: f32) -> Self {
        Self { role, label, min_width, enabled: true }
    }

    pub fn enabled(self, enabled: bool) -> Self {
        Self { enabled, ..self }
    }
}

/// A dialog's button row at the cursor, in the platform's order (see [`ButtonRole`]): every modal
/// draws its buttons through this, so they all agree. Inside a right-to-left row it sits at the
/// right edge. The buttons are laid out left to right, so Tab walks them in reading order.
/// Returns the role of the button clicked this frame.
pub fn dialog_buttons(ui: &mut Ui, buttons: &[DialogButton]) -> Option<ButtonRole> {
    let mac = ui.ctx().os() == egui::os::OperatingSystem::Mac;
    let gap = ui.spacing().item_spacing.x;
    let size = buttons.iter().fold(Vec2::ZERO, |acc, b| {
        let s = button_size(ui, b.label, b.min_width);
        vec2(acc.x + s.x, acc.y.max(s.y))
    }) + vec2(gap * buttons.len().saturating_sub(1) as f32, 0.0);
    ui.allocate_ui_with_layout(size, egui::Layout::left_to_right(egui::Align::Center), |ui| {
        let mut hit = None;
        for slot in 0..4 {
            for b in buttons.iter().filter(|b| b.role.slot(mac) == slot) {
                let r = ui
                    .add_enabled_ui(b.enabled, |ui| {
                        if b.role == ButtonRole::Default { primary_button(ui, b.label, b.min_width) } else { secondary_button(ui, b.label, b.min_width) }
                    })
                    .inner;
                if r.clicked() {
                    hit = Some(b.role);
                }
            }
        }
        hit
    })
    .inner
}

/// The size of a primary or secondary button: its label plus padding, at least `min_width` wide.
fn button_size(ui: &Ui, label: &str, min_width: f32) -> Vec2 {
    button_layout(ui, label, min_width, Tokens::get(ui.ctx()).text).1
}

fn button_layout(ui: &Ui, label: &str, min_width: f32, fg: Color32) -> (std::sync::Arc<egui::Galley>, Vec2) {
    let layout = theme::LayoutTokens::get(ui.ctx());
    let galley = crate::tab_strip::elided(ui, tl!(label), crate::theme_fonts::control(ui.ctx(), layout.button_font_size, false), fg, f32::INFINITY);
    let h = layout.button_height.max(galley.size().y + 4.0);
    let size = vec2((galley.size().x + layout.button_padding_x * 2.0).max(min_width), h);
    (galley, size)
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
    if resp.has_focus() {
        let r = if t.pro { h / 2.0 } else { t.radius_sm } + 2.0;
        ui.painter().rect_stroke(rect.expand(2.0), r, Stroke::new(2.0, t.accent), StrokeKind::Outside);
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

/// Cached RGB data; drawing work is bounded by the 256 display bins, never the image size.
pub fn rgb_histogram(ui: &mut Ui, histogram: &photocraft_algo::histogram::RgbHistogram, height: f32) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width().max(1.0), height.max(1.0)), egui::Sense::hover());
    ui.painter().rect_filled(rect, t.radius_sm, t.histogram_background());
    let plot = rect.shrink(4.0);
    crate::rgb_histogram::paint(ui.painter(), plot, histogram, &t);
    ui.painter().rect_stroke(rect, t.radius_sm, egui::Stroke::new(1.0, t.field_border), egui::StrokeKind::Inside);
    response
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
    dropdown_hovered(ui, id, current, options, width).0
}

/// [`dropdown`], also returning the option under the pointer in its open list (live previews).
pub fn dropdown_hovered<T: PartialEq + Clone>(ui: &mut Ui, id: &str, current: &mut T, options: &[(T, &str)], width: f32) -> (bool, Option<T>) {
    let t = Tokens::get(ui.ctx());
    let label = options.iter().find(|(v, _)| v == current).map(|(_, l)| tl!(l)).unwrap_or("—");
    let (mut changed, mut hovered) = (false, None);
    let historical = t.appearance != theme::Appearance::Modern;
    let width =
        if historical { if width.is_finite() { width.clamp(0.0, ui.available_width().max(0.0)) } else { ui.available_width().max(0.0) } } else { width };
    let mut draw = |ui: &mut Ui| {
        let combo = egui::ComboBox::from_id_salt(id).selected_text(label).width(width).height(420.0).icon(chevron_icon);
        let combo = if historical { combo.truncate() } else { combo };
        combo
            .show_ui(ui, |ui| {
                for (v, l) in options {
                    let selected = v == current;
                    let text = egui::RichText::new(tl!(l));
                    let text = if selected && (t.bevel || historical) { text.color(t.accent_text) } else { text };
                    let item = ui.selectable_label(selected, text);
                    if item.hovered() {
                        hovered = Some(v.clone());
                    }
                    if item.clicked() {
                        *current = v.clone();
                        changed = true;
                    }
                }
            })
            .response
            .on_hover_text(label);
    };
    if historical {
        ui.scope(|ui| {
            ui.set_width(width);
            draw(ui);
        });
    } else {
        draw(ui);
    }
    (changed, hovered)
}

/// The body of a right-click menu: as tall as its items up to the part of the window that can be
/// seen (clear of a taskbar a too-tall window runs under, #315), scrolling past that, so long
/// context menus (a layer's, the canvas tools') stay reachable on small windows instead of running
/// off the bottom. egui moves a popup up to keep it in the window, so a menu opened low on the
/// screen first shifts up and only scrolls when it is taller than the window.
pub fn menu_scroll<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    // The popup frame's margin and stroke, and a small gap to the window's edges.
    let frame = ui.spacing().menu_margin.sum().y + 2.0 + 2.0 * MENU_EDGE;
    let room = (crate::work_area::visible_rect(ui.ctx()).height() - frame).max(MENU_MIN_HEIGHT);
    // A popup's Ui is only as tall as the popup was last frame (400 pt on the first), so ask for
    // the whole room: the area still shrinks to its rows when they need less.
    egui::ScrollArea::vertical().max_height(room).min_scrolled_height(room).show(ui, add_contents).inner
}

/// Gap kept between a context menu and the window's edges, and the shortest it gets.
const MENU_EDGE: f32 = 4.0;
const MENU_MIN_HEIGHT: f32 = 120.0;

/// The colour picker popup of a colour swatch: a click on `swatch` toggles it, a click outside
/// closes it. While open, its left edge stays where it first showed (at the swatch, or further
/// left when the window edge needs it), below the swatch or above it as room allows. The picker's
/// width follows its value readouts, and placing it anew every frame moved it under the pointer,
/// flipping it from side to side near the right edge of the window (#534).
pub fn swatch_popup(swatch: &Response) -> egui::Popup<'static> {
    // Room for the readouts to widen after the picker opened.
    const SLACK: f32 = 32.0;
    let popup = egui::Popup::from_toggle_button_response(swatch).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
    let ctx = &swatch.ctx;
    let key = popup.get_id().with("left");
    if !popup.is_open() {
        ctx.data_mut(|d| d.remove::<f32>(key));
        return popup;
    }
    // Its width is known from the frame after it opened (egui sizes it unseen first).
    let left = ctx.data(|d| d.get_temp::<f32>(key)).or_else(|| {
        let width = popup.get_expected_size()?.x;
        let screen = ctx.content_rect();
        let left = swatch.rect.left().min(screen.right() - width - SLACK).max(screen.left());
        ctx.data_mut(|d| d.insert_temp(key, left));
        Some(left)
    });
    let Some(left) = left else { return popup };
    popup
        .anchor(Rect::from_x_y_ranges(left..=left, swatch.rect.y_range()))
        .align(egui::RectAlign::BOTTOM_START)
        .align_alternatives(&[egui::RectAlign::TOP_START])
}

pub fn dropdown_with_tooltips<T: PartialEq + Clone>(ui: &mut Ui, id: &str, current: &mut T, options: &[(T, &str, &str)], width: f32) -> bool {
    let label = options.iter().find(|(v, _, _)| v == current).map(|(_, l, _)| tl!(l)).unwrap_or("—");
    let mut changed = false;
    let response = egui::ComboBox::from_id_salt(id).selected_text(label).width(width).height(420.0).icon(chevron_icon).show_ui(ui, |ui| {
        for (v, l, tip) in options {
            if ui.selectable_label(v == current, tl!(l)).on_hover_text(tl!(tip)).clicked() {
                *current = v.clone();
                changed = true;
            }
        }
    });
    if let Some((_, _, tip)) = options.iter().find(|(v, _, _)| v == current) {
        let _ = response.response.on_hover_text(tl!(tip));
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

/// Parses a typed number or bounded arithmetic, including parentheses and powers.
/// Preserves egui-compatible whitespace and Unicode minus handling.
pub fn parse_num(text: &str) -> Option<f64> {
    if text.len() > 1024 {
        return None;
    }
    crate::numeric_expression::parse(&clean(text))
}

/// A plain typed number, no arithmetic.
fn plain(text: &str) -> Option<f64> {
    clean(text).parse().ok()
}

fn clean(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).map(|c| if c == '−' { '-' } else { c }).collect()
}

#[cfg(test)]
mod tests {
    /// A right-aligned OK / Cancel / Apply row as `os` draws it: labels left to right, and the
    /// row's right edge with the window's.
    fn button_row(os: egui::os::OperatingSystem) -> (Vec<String>, f32, f32) {
        use super::{ButtonRole, DialogButton};
        use egui_kittest::{Harness, kittest::Queryable};
        // Drawn from the second frame, once the theme's fonts are bound.
        let mut h = Harness::builder().with_size(egui::vec2(500.0, 80.0)).build_ui_state(
            |ui, ready: &mut bool| {
                if *ready {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                        let row = [
                            DialogButton::new(ButtonRole::Default, "OK", 84.0),
                            DialogButton::new(ButtonRole::Cancel, "Cancel", 84.0),
                            DialogButton::new(ButtonRole::Apply, "Apply", 84.0),
                        ];
                        super::dialog_buttons(ui, &row);
                    });
                }
            },
            false,
        );
        crate::PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
        h.ctx.set_os(os);
        *h.state_mut() = true;
        h.run();
        let mut drawn: Vec<(f32, f32, String)> =
            ["OK", "Cancel", "Apply"].iter().map(|l| (h.get_by_label(l).rect().left(), h.get_by_label(l).rect().right(), l.to_string())).collect();
        drawn.sort_by(|a, b| a.0.total_cmp(&b.0));
        let right = drawn.last().map_or(0.0, |d| d.1);
        (drawn.into_iter().map(|d| d.2).collect(), right, h.ctx.content_rect().right())
    }

    #[test]
    fn dialog_buttons_follow_the_platform_order() {
        use egui::os::OperatingSystem as Os;
        for (os, want) in [(Os::Windows, ["OK", "Cancel", "Apply"]), (Os::Nix, ["OK", "Cancel", "Apply"]), (Os::Mac, ["Cancel", "Apply", "OK"])] {
            let (order, right, edge) = button_row(os);
            assert_eq!(order, want, "{os:?}");
            assert!(edge - right < 20.0, "{os:?}: the row hugs the right edge ({right} of {edge})");
        }
    }

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
    fn custom_numeric_fonts_and_unit_labels_stay_inside_painted_fields() {
        for source in [
            include_str!("../../../assets/themes/windows-98-se.json"),
            include_str!("../../../assets/themes/mac-os-9-1.json"),
            include_str!("../../../assets/themes/system-6.json"),
        ] {
            for size in [6.0, 24.0, 64.0] {
                for scale in [1.0, 1.5, 2.0] {
                    let mut pack = crate::theme_pack::ThemePack::parse(source).unwrap();
                    pack.typography.monospace = Some(size);
                    let id = pack.id.clone();
                    let pack = pack.resolve().unwrap();
                    let mut initialized = false;
                    let mut value = 100.0;
                    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(640.0, 300.0)).with_pixels_per_point(scale).build_ui_state(
                        |ui, painted| {
                            if !initialized {
                                crate::PhotocraftApp::setup_context(ui.ctx(), crate::theme::ThemeKind::Classic);
                                initialized = true;
                                return;
                            }
                            pack.apply(ui.ctx());
                            let field = ui.scope(|ui| super::value_field(ui, &mut value, 0.0..=999.0, "pixels", 78.0));
                            *painted = field.response.rect;
                        },
                        egui::Rect::NOTHING,
                    );
                    h.run_steps(4);
                    let painted = *h.state();
                    let texts: Vec<_> = h
                        .output()
                        .shapes
                        .iter()
                        .filter_map(|shape| match &shape.shape {
                            egui::Shape::Text(text) if matches!(text.galley.job.text.as_str(), "100" | "pixels") => Some(text),
                            _ => None,
                        })
                        .collect();
                    assert_eq!(texts.len(), 2);
                    for text in &texts {
                        let bounds = egui::Rect::from_min_size(text.pos, text.galley.size());
                        assert!(painted.expand(0.5).contains_rect(bounds), "{id} {size} {scale}: {bounds:?} outside {painted:?}");
                    }
                    let a = egui::Rect::from_min_size(texts[0].pos, texts[0].galley.size());
                    let b = egui::Rect::from_min_size(texts[1].pos, texts[1].galley.size());
                    assert!(!a.intersects(b), "numeric value and suffix overlap: {a:?} {b:?}");
                    if let Some(dir) = std::env::var_os("PHOTOCRAFT_FIELD_FONT_EVIDENCE") {
                        let dir = std::path::PathBuf::from(dir);
                        std::fs::create_dir_all(&dir).unwrap();
                        h.render().unwrap().save(dir.join(format!("{id}-{size}-{scale}x.png"))).unwrap();
                    }
                }
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
    fn typed_arithmetic_evaluates() {
        use super::parse_num;
        for (text, want) in [
            ("1280*2", 2560.0),
            ("658 * 1.5", 987.0),
            ("48/3", 16.0),
            ("20*2+5-2", 43.0),
            ("2+3*4", 14.0),
            ("10/4*2", 5.0),
            ("-5+3", -2.0),
            ("5--3", 8.0),
            ("2*-3+1", -5.0),
            ("−4", -4.0),
            ("1 234", 1234.0),
            ("1e3/2", 500.0),
            ("(2+3)*4", 20.0),
            ("2^3", 8.0),
            ("10%3", 1.0),
        ] {
            assert_eq!(parse_num(text), Some(want), "{text}");
        }
        for text in ["", "abc", "5+", "*2", "4/0", "0/0", "1+*2", "(2+3", "1e400"] {
            assert_eq!(parse_num(text), None, "{text}");
        }
    }

    /// Types `text` into a value field holding `start`, then presses `key`. Returns the value an OK
    /// button that also fires on Enter saw, as dialogs read it, else the field's value. `round`
    /// makes the caller round the value every frame, as pixel fields do.
    fn type_and_press(start: f32, text: &str, key: egui::Key, round: bool) -> f32 {
        use egui_kittest::{Harness, kittest::Queryable};
        let mut h = Harness::builder().with_size(egui::vec2(300.0, 100.0)).build_ui_state(
            move |ui, (v, ok): &mut (f32, Option<f32>)| {
                super::value_field(ui, v, 0.0..=300000.0, "px", 90.0);
                if round {
                    *v = v.round();
                }
                if ui.button("OK").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    *ok = Some(*v);
                }
            },
            (start, None),
        );
        h.get_by_role(egui::accesskit::Role::SpinButton).click();
        h.run();
        for c in text.chars() {
            h.event(egui::Event::Text(c.to_string()));
            h.run();
        }
        h.key_press(key);
        h.run();
        let (v, ok) = *h.state();
        ok.unwrap_or(v)
    }

    /// Plain digits apply as they're typed; arithmetic doesn't report a change until it's committed.
    #[test]
    fn value_field_holds_arithmetic_until_committed() {
        use egui_kittest::{Harness, kittest::Queryable};
        let mut h = Harness::builder().with_size(egui::vec2(300.0, 100.0)).build_ui_state(
            |ui, (v, changes): &mut (f32, u32)| {
                if super::value_field(ui, v, 0.0..=100.0, "%", 90.0).changed() {
                    *changes += 1;
                }
            },
            (100.0, 0),
        );
        h.get_by_role(egui::accesskit::Role::SpinButton).click();
        h.run();
        for (c, want) in [('5', (5.0, 1)), ('0', (50.0, 2)), ('/', (50.0, 2)), ('2', (50.0, 2))] {
            h.event(egui::Event::Text(c.to_string()));
            h.run();
            assert_eq!(*h.state(), want, "after {c}");
        }
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(*h.state(), (25.0, 3));
    }

    #[test]
    fn value_field_applies_typed_arithmetic() {
        use egui::Key::{Enter, Tab};
        assert_eq!(type_and_press(500.0, "1280*2", Tab, false), 2560.0);
        assert_eq!(type_and_press(500.0, "1280*2", Enter, false), 2560.0);
        // `5/2` alone would round to 3 under the caller; the whole expression still applies.
        assert_eq!(type_and_press(1.0, "5/2*2", Tab, true), 5.0);
        assert_eq!(type_and_press(1.0, "1280/3*2", Enter, true), 853.0);
    }

    #[test]
    fn numbers_drop_trailing_zero() {
        assert_eq!(super::fmt_num(100.0), "100");
        assert_eq!(super::fmt_num(12.46), "12.5");
        assert_eq!(super::fmt_num(-3.0), "-3");
    }
}
