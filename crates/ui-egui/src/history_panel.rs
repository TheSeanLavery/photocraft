//! Narrow vertical history scrubber. Painting reads metadata and cached previews only.
use crate::{PhotocraftApp, theme::Tokens, widgets};
use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use photocraft_doc::DocId;
use photocraft_ops::{HistoryEntry, HistoryState};
use std::collections::{HashMap, VecDeque};

const ROW_HEIGHT: f32 = 62.0;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key(DocId, u64, u64);

enum Preview {
    Ready(egui::TextureHandle),
    Failed(String),
}
type Stamp = (DocId, (u64, u64, u64, usize, usize, usize));

#[derive(Clone, Copy)]
struct ScrollPosition {
    document: DocId,
    rows: usize,
    offset: f32,
    max: f32,
    visible: f32,
}

#[derive(Default)]
pub(crate) struct HistoryPreviews {
    textures: HashMap<Key, Preview>,
    order: VecDeque<Key>,
    pending: Option<(Key, std::sync::mpsc::Receiver<Result<egui::ColorImage, String>>)>,
    stamp: Option<Stamp>,
    rows: Vec<HistoryEntry>,
    indices: HashMap<u64, usize>,
    scrub: Option<u64>,
    scrubbing: bool,
    scroll: Option<ScrollPosition>,
}
impl HistoryPreviews {
    fn poll(&mut self, ctx: &egui::Context) {
        let Some((key, rx)) = &self.pending else { return };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
                return;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err("History thumbnail worker stopped".into()),
        };
        let key = *key;
        self.pending = None;
        self.insert(key, result, ctx);
    }
    fn insert(&mut self, key: Key, image: Result<egui::ColorImage, String>, ctx: &egui::Context) {
        let preview = match image {
            Ok(image) => Preview::Ready(ctx.load_texture(format!("history-{}-{}-{}", key.0.0, key.1, key.2), image, egui::TextureOptions::LINEAR)),
            Err(error) => Preview::Failed(error),
        };
        self.textures.insert(key, preview);
        self.order.push_back(key);
        // Bound GPU/UI preview memory independently of retained document history.
        while self.order.len() > 256 {
            if let Some(old) = self.order.pop_front() {
                self.textures.remove(&old);
            }
        }
    }
    fn request(&mut self, key: Key, state: HistoryState, ctx: &egui::Context) {
        if self.pending.is_some() || self.textures.contains_key(&key) {
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            let repaint = ctx.clone();
            match std::thread::Builder::new().name("history-preview".into()).spawn(move || {
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| preview_image(state))).unwrap_or_else(|_| Err("History preview failed".into()));
                let _ = tx.send(result);
                repaint.request_repaint();
            }) {
                Ok(_) => self.pending = Some((key, rx)),
                Err(error) => self.insert(key, Err(error.to_string()), ctx),
            }
        }
        #[cfg(target_arch = "wasm32")]
        self.insert(key, preview_image(state), ctx);
    }
}
fn preview_image(state: HistoryState) -> Result<egui::ColorImage, String> {
    let doc = state.load_document()?;
    let image = photocraft_compose::thumbnail(&doc, 72);
    Ok(egui::ColorImage::from_rgba_unmultiplied([image.width as usize, image.height as usize], &image.pixels))
}

/// Deterministic branch identity, supplemented with text so color is never the only cue.
pub fn branch_color(accent: Color32, branch: u64) -> Color32 {
    let mut color = egui::ecolor::Hsva::from(accent);
    color.h = (color.h + (branch % 24) as f32 * 0.618_034) % 1.0;
    color.s = color.s.clamp(0.4, 0.65);
    color.v = color.v.max(0.8);
    Color32::from(color)
}

pub fn show(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let Some(active) = app.session.active() else {
        ui.label(tl!("No document"));
        return;
    };
    let document = active.doc.clone();
    let snapshots = active.snapshots.clone();
    let snapshot_count = snapshots.len();
    let live_snapshots: Vec<_> = snapshots.iter().map(|snapshot| snapshot.id).collect();
    let snapshot_names: std::collections::HashSet<_> = active.snapshots.iter().map(|snapshot| snapshot.name.clone()).collect();
    let mut snapshot_command = None;
    let stamp = (document.id, active.history.timeline_stamp());
    app.history_previews.poll(ui.ctx());
    if app.history_previews.stamp != Some(stamp) {
        app.history_previews.rows = active.history.timeline();
        app.history_previews.indices = app.history_previews.rows.iter().enumerate().map(|(index, entry)| (entry.identity.id, index)).collect();
        app.history_previews.stamp = Some(stamp);
    }
    let root = app.history_previews.rows.iter().find(|row| row.identity.parent.is_none()).map(|row| row.identity.id);
    let current = active.history.current_id();
    let enabled = app.ui.transform.is_none() && app.session.active_job().is_none();
    let mut destination = None;
    if ui.add_enabled(enabled && root != Some(current), egui::Button::new(tl!("Return to Original")).min_size(vec2(ui.available_width(), 22.0))).clicked() {
        destination = root;
    }
    if let Some(error) = app.session.history_storage_error() {
        ui.colored_label(t.text_dim, error);
        if ui.button(tl!("Retry storage")).clicked() {
            let _ = app.run("history.retryStorage", serde_json::json!({}));
        }
    }
    // Virtualize rows: an unlimited document must not paint thousands of widgets each frame.
    let count = snapshot_count + app.history_previews.rows.len();
    let max_height = (ui.available_height() - if t.pro { widgets::footer_height(ui) + ui.spacing().item_spacing.y } else { 0.0 }).max(0.0);
    let mut area = crate::scroll_area::ScrollArea::vertical()
        .id_salt(("history-rows", document.id.0))
        .max_height(max_height)
        .min_scrolled_height(0.0)
        .auto_shrink([false, false]);
    // Follow appended actions only if the previous viewport was at the end. Use
    // the adapter's returned offset so historical scrollbar drags count too.
    if let Some(previous) = app.history_previews.scroll
        && previous.document == document.id
        && app.history_previews.rows.len() > previous.rows
        && previous.max - previous.offset <= 1.0
    {
        let spacing = ui.spacing().item_spacing.y;
        let total = count as f32 * (ROW_HEIGHT + spacing) - spacing;
        area = area.vertical_scroll_offset((total - previous.visible).max(0.0));
    }
    let scroll = area.show_rows(ui, ROW_HEIGHT, count, |ui, range| {
        for row_index in range {
            if row_index < snapshot_count {
                snapshot_command = crate::panels::snapshot_row(app, ui, document.id, &snapshots, &live_snapshots, row_index).or(snapshot_command.take());
                ui.add_space(ROW_HEIGHT - 38.0);
                continue;
            }
            let index = row_index - snapshot_count;
            let Some(entry) = app.history_previews.rows.get(index).cloned() else { continue };
            let key = Key(document.id, entry.identity.id, entry.identity.revision);
            if !app.history_previews.textures.contains_key(&key)
                && let Some(active) = app.session.active()
                && let Some(state) = active.history.state_by_id(active.doc.clone(), entry.identity.id)
            {
                app.history_previews.request(key, state, ui.ctx());
            }
            let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click_and_drag());
            let accessible_name = if snapshot_names.contains(&entry.label) { format!("{}: {}", tl!("History"), entry.label) } else { entry.label.clone() };
            response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, enabled, entry.current, &accessible_name));
            let color = branch_color(t.accent, entry.identity.branch);
            let rail_x = rect.left() + 9.0 + if entry.identity.branch == 0 { 0.0 } else { 5.0 };
            if let Some(parent_index) = entry.identity.parent.and_then(|id| app.history_previews.indices.get(&id)).copied()
                && let Some(parent) = app.history_previews.rows.get(parent_index)
                && parent.identity.branch != entry.identity.branch
            {
                let parent_y = rect.center().y - index.saturating_sub(parent_index) as f32 * (ROW_HEIGHT + ui.spacing().item_spacing.y);
                let parent_x = rect.left() + 9.0 + if parent.identity.branch == 0 { 0.0 } else { 5.0 };
                ui.painter().add(egui::epaint::PathShape::line(
                    vec![pos2(parent_x, parent_y), pos2(rail_x, parent_y + 12.0), pos2(rail_x, rect.center().y)],
                    Stroke::new(2.0, color),
                ));
            }
            if entry.current {
                ui.painter().rect_filled(rect, t.radius_sm, t.row_selected);
            } else if app.history_previews.scrub == Some(entry.identity.id) {
                ui.painter().rect_filled(rect, t.radius_sm, t.hover);
            } else if response.hovered() {
                ui.painter().rect_filled(rect, t.radius_sm, t.hover.gamma_multiply(0.5));
            }
            ui.painter().line_segment([pos2(rail_x, rect.top()), pos2(rail_x, rect.bottom())], Stroke::new(2.0, color));
            ui.painter().circle_filled(pos2(rail_x, rect.center().y), 4.0, color);
            let thumb = Rect::from_min_size(pos2(rect.left() + 25.0, rect.top() + 8.0), vec2(64.0, 46.0));
            widgets::checker(ui.painter(), thumb, 5.0);
            match app.history_previews.textures.get(&key) {
                Some(Preview::Ready(texture)) => {
                    let size = texture.size_vec2();
                    let fitted = size * (thumb.width() / size.x).min(thumb.height() / size.y);
                    let image = Rect::from_center_size(thumb.center(), fitted);
                    ui.painter().image(texture.id(), image, Rect::from_min_max(egui::Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
                }
                Some(Preview::Failed(error)) => {
                    response.clone().on_hover_text(error);
                }
                None => {
                    ui.painter().text(thumb.center(), Align2::CENTER_CENTER, "…", egui::FontId::proportional(13.0), t.text_dim);
                }
            }
            ui.painter().rect_stroke(thumb, t.radius_sm, Stroke::new(1.0, t.separator), StrokeKind::Inside);
            let name = if entry.identity.parent.is_none() { tl!("Original").to_string() } else { tl!(&entry.label).to_string() };
            let text_color = if entry.current { t.row_selected_text } else { t.text };
            if let Some(galley) = crate::layer_row_ui::truncated(
                ui.painter(),
                &name,
                egui::FontId::proportional(13.0),
                text_color,
                false,
                (rect.right() - thumb.right() - 15.0).max(0.0),
            ) {
                ui.painter().galley(pos2(thumb.right() + 8.0, rect.top() + 12.0), galley, text_color);
            }
            let branch = format!("{} {}{}", tl!("Branch"), entry.identity.branch, if entry.current { format!(" · {}", tl!("Current")) } else { String::new() });
            ui.painter().text(pos2(thumb.right() + 8.0, rect.top() + 38.0), Align2::LEFT_TOP, branch, egui::FontId::proportional(11.0), color);
            response.clone().on_hover_text(&entry.label);
            if enabled && response.drag_started() {
                app.history_previews.scrubbing = true;
            }
            if enabled
                && app.history_previews.scrubbing
                && ui.input(|input| input.pointer.interact_pos()).is_some_and(|position| rect.intersect(ui.clip_rect()).contains(position))
            {
                app.history_previews.scrub = Some(entry.identity.id);
            }
            if enabled && !entry.current && response.clicked() {
                destination = Some(entry.identity.id);
            }
        }
    });
    app.history_previews.scroll = Some(ScrollPosition {
        document: document.id,
        rows: app.history_previews.rows.len(),
        offset: scroll.state.offset.y,
        max: (scroll.content_size.y - scroll.inner_rect.height()).max(0.0),
        visible: scroll.inner_rect.height(),
    });
    if ui.input(|input| input.pointer.any_released()) {
        app.history_previews.scrubbing = false;
        if enabled
            && let Some(id) = app.history_previews.scrub.take()
            && id != current
        {
            destination = Some(id);
        }
    }
    if let Some(id) = destination {
        let _ = app.run("history.jump", serde_json::json!({"state":id}));
    }
    if let Some((command, params)) = snapshot_command.filter(|(command, _)| *command != "history.selectSnapshot" || enabled) {
        match app.run(command, params) {
            Ok(_) => {
                // Restoring a snapshot appends a state, but the clicked row must stay in reach.
                // Clear only the follow hint; the scroll area's own offset remains unchanged.
                if command == "history.selectSnapshot" {
                    app.history_previews.scroll = None;
                }
            }
            Err(error) => {
                app.ui.status = error;
                app.ui.status_error = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{Harness, kittest::Queryable};
    #[test]
    fn dragging_previews_rows_and_commits_only_the_release_destination() {
        let mut session = photocraft_engine::Session::new();
        session.add_document(
            photocraft_doc::Document::new("Original", photocraft_doc::Size::new(2, 2), photocraft_doc::ColorMode::Rgb, photocraft_doc::SampleType::U8),
            None,
        );
        for name in ["Brush paint", "Erase", "New layer"] {
            session
                .edit(name, |doc, _| {
                    doc.name = name.into();
                    Ok(())
                })
                .unwrap();
        }
        let app = PhotocraftApp::new(session, crate::Services::default());
        let mut h = Harness::builder().with_size(vec2(310.0, 620.0)).build_ui_state(|ui, app| show(app, ui), app);
        h.run_steps(4);
        let start = h.get_by_label("New layer").rect().center();
        let destination = h.get_by_label("Brush paint").rect().center();
        h.event(egui::Event::PointerMoved(start));
        h.event(egui::Event::PointerButton { pos: start, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
        h.step();
        h.event(egui::Event::PointerMoved(destination));
        h.run_steps(2);
        assert_eq!(h.state().session.active().unwrap().history.current_id(), 3, "drag must not restore intermediate documents");
        h.event(egui::Event::PointerButton { pos: destination, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
        h.run_steps(3);
        assert_eq!(h.state().session.active().unwrap().history.current_id(), 1);
        assert_eq!(h.state().session.active().unwrap().doc.name, "Brush paint");
    }
    #[test]
    fn follows_new_actions_only_while_already_at_the_bottom() {
        for preset in ["builtin.pro", "builtin.system-6"] {
            let mut session = photocraft_engine::Session::new();
            session.add_document(
                photocraft_doc::Document::new("Scroll demo", photocraft_doc::Size::new(2, 2), photocraft_doc::ColorMode::Rgb, photocraft_doc::SampleType::U8),
                None,
            );
            // A user may name a snapshot after an ordinary action. Its name must not disable
            // following later actions with the same label.
            session.execute("history.newSnapshot", serde_json::json!({"name": "Brush paint 15"})).unwrap();
            let app = PhotocraftApp::new(session, crate::Services::default());
            let mut h = Harness::builder().with_size(vec2(310.0, 400.0)).build_ui_state(|ui, app| show(app, ui), app);
            PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::Pro);
            if preset == "builtin.system-6" {
                crate::historical::preset(preset).unwrap().resolve().unwrap().apply(&h.ctx);
            }
            h.run_steps(3);
            for i in 0..16 {
                h.state_mut()
                    .session
                    .edit(&format!("Brush paint {i}"), |doc, _| {
                        doc.name = format!("State {i}");
                        Ok(())
                    })
                    .unwrap();
                h.run_steps(3);
                let position = h.state().history_previews.scroll.unwrap();
                assert!((position.max - position.offset).abs() <= 1.0, "{preset}: new actions follow the bottom");
            }
            if let Ok(dir) = std::env::var("PHOTOCRAFT_HISTORY_SCROLL_SNAPSHOT") {
                h.render().unwrap().save(format!("{dir}/{}-following.png", preset.replace('.', "-"))).unwrap();
            }
            h.event(egui::Event::PointerMoved(pos2(180.0, 200.0)));
            h.event(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                phase: egui::TouchPhase::Move,
                delta: vec2(0.0, 250.0),
                modifiers: egui::Modifiers::NONE,
            });
            h.run_steps(30);
            let before = h.state().history_previews.scroll.unwrap();
            assert!(before.max - before.offset > 100.0, "{preset}: wheel must leave the bottom");
            h.state_mut().session.edit("New Layer", |_, _| Ok(())).unwrap();
            h.run_steps(4);
            let after = h.state().history_previews.scroll.unwrap();
            assert!((before.offset - after.offset).abs() <= 1.0, "{preset}: browsing older actions must not jump");
            if let Ok(dir) = std::env::var("PHOTOCRAFT_HISTORY_SCROLL_SNAPSHOT") {
                h.render().unwrap().save(format!("{dir}/{}-browsing.png", preset.replace('.', "-"))).unwrap();
            }
            h.event(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                phase: egui::TouchPhase::Move,
                delta: vec2(0.0, -10_000.0),
                modifiers: egui::Modifiers::NONE,
            });
            h.run_steps(30);
            let returned = h.state().history_previews.scroll.unwrap();
            assert!((returned.max - returned.offset).abs() <= 1.0, "{preset}: scrolling back to bottom resumes following");
            h.state_mut().session.try_undo().unwrap();
            h.state_mut().session.edit("Branch paint", |_, _| Ok(())).unwrap();
            h.run_steps(4);
            let branch = h.state().history_previews.scroll.unwrap();
            assert!((branch.max - branch.offset).abs() <= 1.0, "{preset}: a new retained branch follows too");
        }
    }
    #[test]
    fn branch_colors_are_stable_and_derived_from_the_theme_accent() {
        let accent = Color32::from_rgb(50, 120, 210);
        assert_eq!(branch_color(accent, 3), branch_color(accent, 3));
        assert_ne!(branch_color(accent, 0), branch_color(accent, 3));
        assert_ne!(branch_color(accent, 3), branch_color(Color32::RED, 3));
    }
}
