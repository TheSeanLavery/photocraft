//! Historical scrollbar adapter; egui retains content layout, virtualization and wheel input.
use crate::theme::{Appearance, Tokens};
use egui::containers::scroll_area::{ScrollAreaOutput, ScrollSource};
use egui::{Id, Rect, Response, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

#[derive(Clone, Copy)]
struct Hold {
    next: f64,
}
#[derive(Clone, Copy)]
struct ThumbDrag {
    offset: f32,
    at: f32,
}

/// Physical geometry is theme data, in logical points.
pub fn width(ui: &Ui) -> f32 {
    crate::theme::LayoutTokens::get(ui.ctx()).scroll_bar_width
}

/// Historical viewport excludes the reserved bars, including their corner square.
pub fn content_rect(ui: &Ui, outer: Rect) -> Rect {
    let w = width(ui).min(outer.width().max(0.0)).min(outer.height().max(0.0));
    Rect::from_min_max(outer.min, pos2(outer.right() - w, outer.bottom() - w))
}

#[derive(Clone, Copy, Debug)]
pub struct BarLayout {
    pub leading: Rect,
    pub trailing: Rect,
    pub track: Rect,
    pub thumb: Option<Rect>,
}

/// Fixed classic Mac scroll boxes; proportional Windows 98 indicators.
pub fn bar_layout(bar: Rect, axis: usize, offset: f32, visible: f32, total: f32, appearance: Appearance) -> BarLayout {
    let horizontal = axis == 0;
    let length = if horizontal { bar.width() } else { bar.height() };
    let cross = if horizontal { bar.height() } else { bar.width() };
    let arrow = cross.max(0.0).min(length.max(0.0) / 2.0);
    let section = |start: f32, len: f32| {
        if horizontal {
            Rect::from_min_size(bar.min + vec2(start, 0.0), vec2(len, cross))
        } else {
            Rect::from_min_size(bar.min + vec2(0.0, start), vec2(cross, len))
        }
    };
    let track_len = (length - arrow * 2.0).max(0.0);
    let track = section(arrow, track_len);
    let thumb = if total.is_finite() && visible.is_finite() && offset.is_finite() && total > visible && visible > 0.0 && track_len > 0.0 {
        let size =
            if appearance == Appearance::Windows98 { (visible / total * track_len).clamp(cross.min(track_len), track_len) } else { cross.min(track_len) };
        let at = offset.clamp(0.0, total - visible) / (total - visible) * (track_len - size);
        Some(section(arrow + at, size))
    } else {
        None
    };
    BarLayout { leading: section(0.0, arrow), trailing: section(length - arrow, arrow), track, thumb }
}

fn repeat(ui: &Ui, response: &Response, allowed: bool) -> bool {
    let id = response.id.with("historical-repeat");
    let old = ui.ctx().data(|d| d.get_temp::<Hold>(id));
    let time = ui.input(|i| i.time);
    if response.is_pointer_button_down_on() {
        let fire = allowed && old.is_none_or(|h| time >= h.next) && response.hovered();
        if fire {
            ui.ctx().data_mut(|d| d.insert_temp(id, Hold { next: time + if old.is_none() { 0.35 } else { 0.06 } }));
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
        fire
    } else {
        ui.ctx().data_mut(|d| d.remove::<Hold>(id));
        allowed && old.is_none() && response.clicked()
    }
}

fn track(ui: &Ui, rect: Rect, t: Tokens, enabled: bool) {
    ui.painter().rect_filled(rect, 0.0, t.chrome);
    if !enabled {
        return;
    }
    if t.appearance == Appearance::Platinum {
        crate::historical::surface(ui, rect, t.chrome, false);
        return;
    }
    let key = Id::new("historical-scroll-pattern");
    let colors = if t.appearance == Appearance::System6 { (t.field, t.text) } else { (t.bevel_highlight, t.chrome) };
    let cached = ui.ctx().data(|d| d.get_temp::<(egui::Color32, egui::Color32, egui::TextureHandle)>(key));
    let texture = match cached {
        Some((a, b, texture)) if (a, b) == colors => texture,
        _ => {
            let pixels = (0..1024).map(|i| if (i / 32 + i % 32) % 2 == 0 { colors.0 } else { colors.1 }).collect();
            let options = egui::TextureOptions { wrap_mode: egui::TextureWrapMode::Repeat, ..egui::TextureOptions::NEAREST };
            let texture = ui.ctx().load_texture("historical-scroll-pattern", egui::ColorImage::new([32, 32], pixels), options);
            ui.ctx().data_mut(|d| d.insert_temp(key, (colors.0, colors.1, texture.clone())));
            texture
        }
    };
    // Explicit tiles also work with egui's predictable offscreen filtering, which
    // clamps texture loads and ignores sampler repeat. Batch the whole track in one mesh.
    let rect = rect.intersect(ui.clip_rect());
    if !rect.is_positive() {
        return;
    }
    let mut mesh = egui::Mesh::with_texture(texture.id());
    let nx = (rect.width() / 32.0).ceil().clamp(0.0, 512.0) as usize;
    let ny = (rect.height() / 32.0).ceil().clamp(0.0, 512.0) as usize;
    for y in 0..ny {
        for x in 0..nx {
            let min = rect.min + vec2(x as f32 * 32.0, y as f32 * 32.0);
            let r = Rect::from_min_max(min, (min + vec2(32.0, 32.0)).min(rect.max));
            mesh.add_rect_with_uv(r, Rect::from_min_max(pos2(0.0, 0.0), pos2(r.width() / 32.0, r.height() / 32.0)), egui::Color32::WHITE);
        }
    }
    ui.painter().add(mesh);
}

fn arrow(ui: &Ui, r: Rect, axis: usize, forward: bool, enabled: bool, response: &Response, t: Tokens) {
    let down = enabled && response.is_pointer_button_down_on();
    let fill = if down && t.appearance == Appearance::System6 { t.text } else { t.chrome };
    crate::historical::surface(ui, r, fill, !down);
    let dir = if forward { 1.0 } else { -1.0 };
    let c = r.center();
    let points = if axis == 0 {
        vec![c + vec2(3.0 * dir, 0.0), c + vec2(-2.0 * dir, -3.0), c + vec2(-2.0 * dir, 3.0)]
    } else {
        vec![c + vec2(0.0, 3.0 * dir), c + vec2(-3.0, -2.0 * dir), c + vec2(3.0, -2.0 * dir)]
    };
    let hollow = t.appearance == Appearance::System6 || !enabled;
    let ink = if down && t.appearance == Appearance::System6 {
        t.field
    } else if enabled || t.appearance == Appearance::System6 {
        t.text
    } else {
        t.text_faint
    };
    ui.painter().add(egui::Shape::convex_polygon(points, if hollow { fill } else { ink }, Stroke::new(1.0, ink)));
}

/// Shared interaction used by canvas bars and all app scroll areas. Returns offset and hover state.
#[derive(Clone, Copy)]
pub struct ScrollRange {
    pub offset: f32,
    pub visible: f32,
    pub total: f32,
}

pub fn bar(ui: &Ui, rect: Rect, axis: usize, range: ScrollRange, id: Id, interactive: bool) -> (f32, bool) {
    let ScrollRange { offset, visible, total } = range;
    let t = Tokens::get(ui.ctx());
    let layout = bar_layout(rect, axis, offset, visible, total, t.appearance);
    let max = (total - visible).max(0.0);
    let enabled = interactive && ui.is_enabled() && max.is_finite() && max > 0.0;
    let mut next = if offset.is_finite() { offset.clamp(0.0, max) } else { 0.0 };
    let track_response = ui.interact(layout.track, id.with("track"), Sense::click());
    track(ui, layout.track, t, enabled);
    let coordinate = |p: egui::Pos2| if axis == 0 { p.x } else { p.y };
    if let Some(thumb) = layout.thumb {
        let clicked = track_response.interact_pointer_pos();
        let backward = clicked.is_some_and(|p| coordinate(p) < coordinate(thumb.min));
        let forward = clicked.is_some_and(|p| coordinate(p) > coordinate(thumb.max));
        if repeat(ui, &track_response, enabled && (backward || forward)) {
            next = (next + if backward { -visible } else { visible }).clamp(0.0, max);
        }
        let response = ui.interact(thumb, id.with("thumb"), Sense::click_and_drag());
        response.widget_info(|| egui::WidgetInfo::new(egui::WidgetType::ScrollBar));
        crate::historical::surface(ui, thumb, if t.appearance == Appearance::Platinum { t.accent } else { t.chrome }, true);
        let drag_id = response.id.with("drag-origin");
        if response.drag_started()
            && let Some(p) = ui.input(|i| i.pointer.press_origin()).or(response.interact_pointer_pos())
        {
            ui.ctx().data_mut(|d| d.insert_temp(drag_id, ThumbDrag { offset: next, at: coordinate(p) }));
        }
        if enabled
            && response.dragged()
            && let (Some(start), Some(p)) = (ui.ctx().data(|d| d.get_temp::<ThumbDrag>(drag_id)), response.interact_pointer_pos())
        {
            let travel = if axis == 0 { layout.track.width() - thumb.width() } else { layout.track.height() - thumb.height() };
            if travel > 0.0 {
                next = (start.offset + (coordinate(p) - start.at) * max / travel).clamp(0.0, max);
            }
        }
        if response.drag_stopped() || !ui.input(|i| i.pointer.primary_down()) {
            ui.ctx().data_mut(|d| d.remove::<ThumbDrag>(drag_id));
        }
    }
    for (forward, r, label) in [
        (false, layout.leading, if axis == 0 { tl!("Scroll left") } else { tl!("Scroll up") }),
        (true, layout.trailing, if axis == 0 { tl!("Scroll right") } else { tl!("Scroll down") }),
    ] {
        let response = ui.interact(r, id.with(if forward { "trailing" } else { "leading" }), Sense::click());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
        if repeat(ui, &response, enabled) {
            next = (next + if forward { 16.0 } else { -16.0 }).clamp(0.0, max);
        }
        arrow(ui, r, axis, forward, enabled, &response, t);
    }
    ui.painter().rect_stroke(rect, 0.0, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
    let over = ui.input(|i| i.pointer.hover_pos().is_some_and(|p| rect.contains(p)))
        || ui.ctx().data(|d| d.get_temp::<ThumbDrag>(id.with("thumb").with("drag-origin"))).is_some();
    if next != offset {
        ui.ctx().request_repaint();
    }
    (next, over)
}

/// Builder-compatible adapter: existing modern themes take egui's original path unchanged.
pub struct ScrollArea {
    inner: egui::ScrollArea,
    source: ScrollSource,
    direction: [bool; 2],
}
impl ScrollArea {
    pub fn vertical() -> Self {
        Self { inner: egui::ScrollArea::vertical(), source: ScrollSource::default(), direction: [false, true] }
    }
    pub fn both() -> Self {
        Self { inner: egui::ScrollArea::both(), source: ScrollSource::default(), direction: [true, true] }
    }
    pub fn id_salt(mut self, id: impl egui::AsIdSalt) -> Self {
        self.inner = self.inner.id_salt(id);
        self
    }
    pub fn max_height(mut self, v: f32) -> Self {
        self.inner = self.inner.max_height(v);
        self
    }
    pub fn max_width(mut self, v: f32) -> Self {
        self.inner = self.inner.max_width(v);
        self
    }
    pub fn min_scrolled_height(mut self, v: f32) -> Self {
        self.inner = self.inner.min_scrolled_height(v);
        self
    }
    pub fn auto_shrink(mut self, v: [bool; 2]) -> Self {
        self.inner = self.inner.auto_shrink(v);
        self
    }
    pub fn scroll_source(mut self, v: ScrollSource) -> Self {
        self.source = v;
        self.inner = self.inner.scroll_source(v);
        self
    }
    fn prepare(mut self, ui: &Ui) -> Self {
        if Tokens::get(ui.ctx()).appearance != Appearance::Modern {
            let mut source = self.source;
            source.scroll_bar = false;
            self.inner = self.inner.scroll_source(source).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded).animated(false);
        }
        self
    }
    fn finish<R>(ui: &Ui, mut out: ScrollAreaOutput<R>, direction: [bool; 2], interactive: bool) -> ScrollAreaOutput<R> {
        if Tokens::get(ui.ctx()).appearance == Appearance::Modern {
            return out;
        }
        let w = width(ui);
        for (axis, shown) in direction.into_iter().enumerate() {
            if !shown || out.content_size[axis] <= out.inner_rect.size()[axis] + 0.5 {
                continue;
            }
            let r = if axis == 0 {
                Rect::from_min_size(out.inner_rect.left_bottom(), vec2(out.inner_rect.width(), w))
            } else {
                Rect::from_min_size(out.inner_rect.right_top(), vec2(w, out.inner_rect.height()))
            };
            let (offset, _) = bar(
                ui,
                r,
                axis,
                ScrollRange { offset: out.state.offset[axis], visible: out.inner_rect.size()[axis], total: out.content_size[axis] },
                out.id.with(("historical-bar", axis)),
                interactive,
            );
            out.state.offset[axis] = offset;
        }
        if direction == [true, true] && out.content_size.x > out.inner_rect.width() + 0.5 && out.content_size.y > out.inner_rect.height() + 0.5 {
            ui.painter().rect_filled(Rect::from_min_size(out.inner_rect.right_bottom(), vec2(w, w)), 0.0, Tokens::get(ui.ctx()).chrome);
        }
        out.state.store(ui.ctx(), out.id);
        out
    }
    pub fn show<R>(self, ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> ScrollAreaOutput<R> {
        let direction = self.direction;
        let interactive = self.source.scroll_bar;
        let out = self.prepare(ui).inner.show(ui, add);
        Self::finish(ui, out, direction, interactive)
    }
    pub fn show_rows<R>(self, ui: &mut Ui, height: f32, rows: usize, add: impl FnOnce(&mut Ui, std::ops::Range<usize>) -> R) -> ScrollAreaOutput<R> {
        let direction = self.direction;
        let interactive = self.source.scroll_bar;
        let out = self.prepare(ui).inner.show_rows(ui, height, rows, add);
        Self::finish(ui, out, direction, interactive)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{Harness, kittest::Queryable};

    #[test]
    fn era_thumb_geometry_reaches_ends_and_small_tracks_are_safe() {
        for appearance in [Appearance::Windows98, Appearance::Platinum, Appearance::System6] {
            for axis in [0, 1] {
                let r =
                    if axis == 0 { Rect::from_min_size(egui::Pos2::ZERO, vec2(300.0, 16.0)) } else { Rect::from_min_size(egui::Pos2::ZERO, vec2(16.0, 300.0)) };
                let a = bar_layout(r, axis, 0.0, 100.0, 1000.0, appearance);
                let b = bar_layout(r, axis, 900.0, 100.0, 1000.0, appearance);
                assert_eq!(a.thumb.unwrap().min[axis], a.track.min[axis]);
                assert_eq!(b.thumb.unwrap().max[axis], b.track.max[axis]);
                if appearance != Appearance::Windows98 {
                    assert_eq!(a.thumb.unwrap().size()[axis], 16.0);
                }
                assert!(bar_layout(r, axis, 0.0, 1000.0, 100.0, appearance).thumb.is_none());
                for v in [0.0, 1.0, f32::NAN, f32::INFINITY] {
                    assert!(bar_layout(r, axis, v, v, v, appearance).thumb.is_none());
                }
                let tiny = Rect::from_min_size(egui::Pos2::ZERO, vec2(8.0, 8.0));
                let tiny = bar_layout(tiny, axis, 0.0, 100.0, 1000.0, appearance);
                assert!(tiny.thumb.is_none());
                assert!(tiny.leading.max[axis] <= tiny.trailing.min[axis]);
            }
        }
    }

    struct ListState {
        offset: f32,
        inner: Rect,
        size: egui::Vec2,
    }
    impl Default for ListState {
        fn default() -> Self {
            Self { offset: 0.0, inner: Rect::NOTHING, size: egui::Vec2::ZERO }
        }
    }
    fn list(id: &str, scale: f32) -> Harness<'static, ListState> {
        let mut initialized = false;
        let mut h = Harness::builder().with_size(vec2(400.0, 220.0)).with_pixels_per_point(scale).build_ui_state(
            move |ui, state: &mut ListState| {
                if !initialized {
                    crate::PhotocraftApp::setup_context(ui.ctx(), crate::theme::ThemeKind::Classic);
                    initialized = true;
                    return;
                }
                let out = ScrollArea::vertical().id_salt("scroll-test").max_height(120.0).auto_shrink([false, false]).show(ui, |ui| {
                    for i in 0..60 {
                        ui.label(format!("Row {i}"));
                    }
                });
                state.offset = out.state.offset.y;
                state.inner = out.inner_rect;
                state.size = out.content_size;
            },
            ListState::default(),
        );
        crate::historical::preset(id).unwrap().resolve().unwrap().apply(&h.ctx);
        h.run_steps(6);
        assert_eq!(h.ctx.global_style().spacing.scroll.fade.strength, 0.0, "historical lists clip instead of fading their edges");
        h
    }
    fn click(h: &mut Harness<'static, ListState>, p: egui::Pos2) {
        h.event(egui::Event::PointerMoved(p));
        h.run_steps(1);
        h.event(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
        h.run_steps(1);
        h.event(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
        h.run_steps(2);
    }

    #[test]
    fn arrow_click_page_click_wheel_and_thumb_drag_work_at_all_scales() {
        for id in crate::historical::IDS {
            for scale in [1.0, 1.5, 2.0] {
                let mut h = list(id, scale);
                h.get_by_label("Scroll down").click();
                h.run_steps(3);
                assert_eq!(h.state().offset, 16.0, "one line, no duplicate release step");
                let r = h.state().inner;
                click(&mut h, pos2(r.right() + 8.0, r.bottom() - 20.0));
                assert!((h.state().offset - 16.0 - r.height()).abs() < 0.1, "one page");
                let before = h.state().offset;
                h.event(egui::Event::PointerMoved(r.center()));
                h.event(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    phase: egui::TouchPhase::Move,
                    delta: vec2(0.0, -40.0),
                    modifiers: egui::Modifiers::NONE,
                });
                h.run_steps(12);
                assert!(h.state().offset > before, "wheel scrolling is preserved");
                let r = Rect::from_min_size(h.state().inner.right_top(), vec2(16.0, h.state().inner.height()));
                let layout = bar_layout(r, 1, h.state().offset, h.state().inner.height(), h.state().size.y, crate::theme::Tokens::get(&h.ctx).appearance);
                let thumb = layout.thumb.unwrap();
                let start = thumb.center();
                let end = start + vec2(0.0, layout.track.bottom() - thumb.bottom());
                h.event(egui::Event::PointerMoved(start));
                h.run_steps(1);
                h.event(egui::Event::PointerButton { pos: start, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
                h.run_steps(1);
                h.event(egui::Event::PointerMoved(end));
                h.run_steps(2);
                h.event(egui::Event::PointerButton { pos: end, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
                h.run_steps(2);
                assert!((h.state().offset - (h.state().size.y - h.state().inner.height())).abs() < 1.0, "thumb reaches the end: {}", h.state().offset);
            }
        }
    }

    #[test]
    fn holding_an_arrow_repeats_and_release_stops() {
        let mut h = list("builtin.system-6", 1.0);
        let at = h.get_by_label("Scroll down").rect().center();
        h.event(egui::Event::PointerMoved(at));
        h.run_steps(1);
        h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
        h.run_steps(1);
        assert_eq!(h.state().offset, 16.0);
        h.run_steps(60);
        assert!(h.state().offset > 16.0);
        h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
        h.run_steps(2);
        let stopped = h.state().offset;
        h.run_steps(12);
        assert_eq!(h.state().offset, stopped);
    }

    #[test]
    fn modern_adapter_matches_egui_and_virtualization_is_retained() {
        fn capture(adapter: bool) -> (Rect, egui::Vec2) {
            let mut h = Harness::builder().with_size(vec2(400.0, 220.0)).build_ui_state(
                |ui, state: &mut (Rect, egui::Vec2)| {
                    let add = |ui: &mut Ui| {
                        for i in 0..30 {
                            ui.label(format!("Row {i}"));
                        }
                    };
                    let out = if adapter {
                        ScrollArea::vertical().id_salt("same-id").max_height(120.0).show(ui, add)
                    } else {
                        egui::ScrollArea::vertical().id_salt("same-id").max_height(120.0).show(ui, add)
                    };
                    *state = (out.inner_rect, out.content_size);
                },
                (Rect::NOTHING, egui::Vec2::ZERO),
            );
            h.run_steps(6);
            *h.state()
        }
        assert_eq!(capture(true), capture(false));
        let mut count = 0;
        let mut h = Harness::builder().with_size(vec2(400.0, 220.0)).build_ui(|ui| {
            count = 0;
            ScrollArea::vertical().max_height(120.0).show_rows(ui, 20.0, 10_000, |ui, range| {
                count += range.len();
                for _ in range {
                    ui.label("Virtual row");
                }
            });
        });
        crate::PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::Classic);
        crate::historical::preset("builtin.system-6").unwrap().resolve().unwrap().apply(&h.ctx);
        h.run_steps(6);
        drop(h);
        assert!(count < 20, "virtual rows remain bounded: {count}");
    }
    #[test]
    fn system6_track_pattern_has_black_and_white_pixels_offscreen() {
        let mut initialized = false;
        let mut h = Harness::builder().with_size(vec2(300.0, 120.0)).wgpu().build_ui_state(
            |ui, rect: &mut Rect| {
                if !initialized {
                    crate::PhotocraftApp::setup_context(ui.ctx(), crate::theme::ThemeKind::Classic);
                    initialized = true;
                    return;
                }
                let (r, _) = ui.allocate_exact_size(vec2(160.0, 16.0), Sense::hover());
                *rect = r;
                bar(ui, r, 0, ScrollRange { offset: 0.0, visible: 100.0, total: 1000.0 }, Id::new("pattern-test"), true);
            },
            Rect::NOTHING,
        );
        crate::historical::preset("builtin.system-6").unwrap().resolve().unwrap().apply(&h.ctx);
        h.run_steps(6);
        let image = h.render().unwrap();
        let r = *h.state();
        let a = image.get_pixel((r.left() + 48.0) as u32, (r.top() + 4.0) as u32);
        let b = image.get_pixel((r.left() + 49.0) as u32, (r.top() + 4.0) as u32);
        assert_ne!(a, b, "adjacent track cells must alternate");
        assert!((a[0] == 0 && b[0] == 255) || (a[0] == 255 && b[0] == 0), "the monochrome checker is opaque black and white");
    }
}
