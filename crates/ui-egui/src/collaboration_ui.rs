//! Native room controls. Shared changes are engine commands; display preferences stay local.
use crate::PhotocraftApp;
use egui::RichText;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CollaborationUi {
    pub open: bool,
    pub panel_mode: PanelMode,
    pub notes_tab: bool,
    pub name: String,
    pub icon: String,
    pub color: [u8; 3],
    pub code: String,
    #[serde(skip)]
    pub password: String,
    pub signaling_url: String,
    pub cursor_visible: bool,
    pub name_visible: bool,
    pub show_cursors: bool,
    pub show_names: bool,
    pub show_notes: bool,
    pub chat_draft: String,
    pub note_draft: String,
    pub error: String,
    pub transport_status: String,
    /// Read-only diagnostics for bounded network work; no presence or tool payloads.
    pub transport_metrics: Value,
    #[serde(skip)]
    pub transport_pending: bool,
    #[serde(skip)]
    pub last_cursor_sent: f64,
    #[serde(skip)]
    pub last_cursor_payload: Option<(String, Value)>,
    pub voice_enabled: bool,
    #[serde(skip)]
    pub push_to_talk: bool,
    pub voice_status: String,
}
impl Default for CollaborationUi {
    fn default() -> Self {
        Self {
            open: false,
            panel_mode: PanelMode::Full,
            notes_tab: false,
            name: "Artist".into(),
            icon: String::new(),
            color: [80, 170, 240],
            code: String::new(),
            password: String::new(),
            signaling_url: photocraft_engine::collab::DEFAULT_SIGNALING_URL.into(),
            cursor_visible: true,
            name_visible: true,
            show_cursors: true,
            show_names: true,
            show_notes: true,
            chat_draft: String::new(),
            note_draft: String::new(),
            error: String::new(),
            last_cursor_sent: 0.0,
            last_cursor_payload: None,
            voice_enabled: false,
            push_to_talk: false,
            voice_status: "Voice off".into(),
            transport_status: "Disconnected".into(),
            transport_metrics: Value::Null,
            transport_pending: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PanelMode {
    #[default]
    Full,
    Minimal,
    Collapsed,
}

/// A discoverable people button, drawn with native primitives in every theme.
pub fn entry_button(app: &mut PhotocraftApp, ui: &mut egui::Ui, side: f32) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(egui::Vec2::splat(side), egui::Sense::click());
    let selected = app.ui.collaboration.open;
    if selected || response.hovered() {
        ui.painter().rect_filled(rect, t.radius_sm, if selected { t.accent_soft } else { t.hover });
    }
    let color = if selected { t.accent } else { t.text_dim };
    let c = rect.center();
    for dx in [-4.0, 4.0] {
        ui.painter().circle_stroke(c + egui::vec2(dx, -3.0), 2.5, egui::Stroke::new(1.3, color));
        ui.painter().line_segment([c + egui::vec2(dx - 3.0, 7.0), c + egui::vec2(dx - 3.0, 3.0)], egui::Stroke::new(1.3, color));
        ui.painter().line_segment([c + egui::vec2(dx - 3.0, 3.0), c + egui::vec2(dx + 3.0, 3.0)], egui::Stroke::new(1.3, color));
        ui.painter().line_segment([c + egui::vec2(dx + 3.0, 3.0), c + egui::vec2(dx + 3.0, 7.0)], egui::Stroke::new(1.3, color));
    }
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Room"));
    if response.on_hover_text("Room — invite people and draw together").clicked() {
        let state = &mut app.ui.collaboration;
        if state.open && state.panel_mode != PanelMode::Collapsed {
            state.open = false;
            state.push_to_talk = false;
        } else {
            state.open = true;
            if state.panel_mode == PanelMode::Collapsed {
                state.panel_mode = PanelMode::Full;
            }
        }
    }
}

/// Open an invitation without closing or replacing an existing drawing.
pub fn open_invite(app: &mut PhotocraftApp, input: &str) -> Result<(), String> {
    let invite = photocraft_engine::collab::invite::parse_with_server(input, &app.ui.collaboration.signaling_url)?;
    app.ui.collaboration.open = true;
    app.ui.collaboration.panel_mode = PanelMode::Full;
    if let Some(room) = app.session.collaboration.room.as_ref() {
        if room.code != invite.code {
            return Err("Leave the current room before joining another room. Your drawing is preserved.".into());
        }
        if app.ui.collaboration.transport_pending
            || app.ui.collaboration.transport_status.starts_with("Connected")
            || app.ui.collaboration.transport_status.starts_with("Hosting")
            || !app.session.collaboration.network_intents.is_empty()
        {
            return Ok(());
        }
    }
    app.ui.collaboration.code = invite.code.clone();
    app.ui.collaboration.signaling_url = invite.signaling_url.clone();
    app.run(
        "collab.room.join",
        json!({"code":invite.code,"name":app.ui.collaboration.name,"password":app.ui.collaboration.password,"signalingUrl":invite.signaling_url}),
    )?;
    Ok(())
}

fn color_of(profile: &photocraft_engine::collab::Profile) -> egui::Color32 {
    egui::Color32::from_rgb(
        (profile.color[0].clamp(0.0, 1.0) * 255.0) as u8,
        (profile.color[1].clamp(0.0, 1.0) * 255.0) as u8,
        (profile.color[2].clamp(0.0, 1.0) * 255.0) as u8,
    )
}

fn people(ui: &mut egui::Ui, room: &photocraft_engine::collab::RoomState, minimal: bool) {
    let t = crate::theme::Tokens::get(ui.ctx());
    ui.label(RichText::new("People").strong());
    egui::ScrollArea::vertical().id_salt("room-people").max_height(if minimal { 150.0 } else { 220.0 }).show(ui, |ui| {
        for (id, member) in &room.members {
            let color = color_of(&member.profile);
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(if minimal { 20.0 } else { 28.0 }), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), rect.width() / 2.0, color);
                let initials = member
                    .profile
                    .icon
                    .clone()
                    .filter(|v| !v.is_empty())
                    .unwrap_or_else(|| member.profile.name.chars().take(2).collect::<String>().to_uppercase());
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, initials, crate::theme::medium(10.0), t.canvas);
                ui.label(if *id == room.local_peer { "You" } else { &member.profile.name });
                if *id == room.host {
                    ui.label(RichText::new("Host").small().color(t.text_dim));
                }
                if member.speaking {
                    ui.label(RichText::new("Speaking").small().color(color));
                }
            });
        }
    });
}

/// Persistent right dock. Called before the central canvas so no drawing is covered.
pub fn dock(app: &mut PhotocraftApp, root: &mut egui::Ui) {
    if !app.ui.collaboration.open {
        app.ui.collaboration.push_to_talk = false;
        return;
    }
    let mode = app.ui.collaboration.panel_mode;
    let t = crate::theme::Tokens::get(root.ctx());
    if mode == PanelMode::Collapsed {
        app.ui.collaboration.push_to_talk = false;
        return;
    }
    let center = app.session.active().map(|s| [f64::from(s.doc.size.width) / 2.0, f64::from(s.doc.size.height) / 2.0]).unwrap_or([0.0, 0.0]);
    let mut layers = serde_json::Map::new();
    if let Some(doc) = app.session.documents().iter().find(|st| Some(st.doc.id) == app.session.collaboration.document_id) {
        let mut pending: Vec<_> = doc.doc.layers.iter().collect();
        while let Some(layer) = pending.pop() {
            layers.insert(layer.id.0.to_string(), json!(layer.visible));
            if let Some(children) = layer.children() {
                pending.extend(children);
            }
        }
    }
    let mut action: Option<(&str, Value)> = None;
    let mut desktop_url = None;
    let room = app.session.collaboration.room.as_ref();
    let state = &mut app.ui.collaboration;
    state.push_to_talk = false;
    let width = if mode == PanelMode::Minimal { 184.0 } else { 300.0 };
    egui::Panel::right("room-dock").resizable(mode == PanelMode::Full).default_size(width).min_size(if mode == PanelMode::Minimal { 184.0 } else { 240.0 }).max_size(if mode == PanelMode::Minimal { 184.0 } else { 360.0 }).frame(egui::Frame::NONE.fill(t.dock).inner_margin(12)).show(root, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Room").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::icons::button(ui, "chevrons-right", 22.0, false, "Collapse room panel").clicked() { state.panel_mode = PanelMode::Collapsed; }
                if crate::icons::button(ui, "maximize-2", 22.0, false, if mode == PanelMode::Minimal { "Full room panel" } else { "Minimal room panel" }).clicked() {
                    state.panel_mode = if mode == PanelMode::Full { PanelMode::Minimal } else { PanelMode::Full };
                }
            });
        });
        ui.separator();
        let ready = room.is_some() && !state.transport_pending && (state.transport_status.starts_with("Hosting") || state.transport_status.starts_with("Connected"));
        ui.label(RichText::new(if ready { format!("Connected · {} people", room.map_or(0, |r| r.members.len())) } else { state.transport_status.clone() }).small().color(t.text_dim));
        if !state.error.is_empty() { ui.label(RichText::new(&state.error).color(t.danger)); }
        egui::ScrollArea::vertical().id_salt("room-content").show(ui, |ui| {
            if let Some(room) = room {
                if ui.add_enabled(ready, egui::Button::image_and_text(crate::icons::image("link", 14.0, t.accent_text), "Copy invite").fill(t.accent).min_size(egui::vec2(ui.available_width(), 30.0))).clicked() {
                    match photocraft_engine::collab::invite::share_url_with_server(&room.code, &state.signaling_url) {
                        Ok(link) => ui.ctx().copy_text(link),
                        Err(error) => state.error = error,
                    }
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Code").small().color(t.text_dim));
                    ui.monospace(&room.code);
                    if crate::icons::button(ui, "copy", 20.0, false, "Copy room code").clicked() { ui.ctx().copy_text(room.code.clone()); }
                });
                ui.separator();
                people(ui, room, mode == PanelMode::Minimal);
            }
            if mode == PanelMode::Full && !ready && !state.transport_pending {
                ui.add_space(8.0);
                ui.label("Draw together");
                ui.add(egui::TextEdit::singleline(&mut state.code).hint_text("Invite link or room code").desired_width(f32::INFINITY));
                if crate::widgets::primary_button(ui, "Join room", ui.available_width()).clicked() {
                    match photocraft_engine::collab::invite::parse_with_server(&state.code, &state.signaling_url) {
                        Ok(invite) => action = Some(("collab.room.join", json!({"code":invite.code,"name":state.name,"password":state.password,"signalingUrl":invite.signaling_url}))),
                        Err(error) => state.error = error,
                    }
                }
                if ui.button("Create a room").clicked() { action = Some(("collab.room.create", json!({"name":state.name,"password":state.password,"signalingUrl":state.signaling_url}))); }
            }
            ui.separator();
            if cfg!(target_arch = "wasm32") {
                state.voice_enabled = false;
                ui.label(RichText::new("Voice available in desktop").small().color(t.text_dim));
                if let Some(room) = room && ui.button("Open in PhotoCraft").clicked() { desktop_url = Some(format!("photocraft://join/{}", room.code)); }
            } else {
                ui.add_enabled_ui(ready, |ui| {
                    ui.checkbox(&mut state.voice_enabled, "Voice");
                    let talk = ui.add_sized([ui.available_width(), 30.0], egui::Button::new("Hold to talk"));
                    state.push_to_talk = ready && state.voice_enabled && talk.is_pointer_button_down_on();
                    if mode == PanelMode::Full { ui.label(RichText::new(&state.voice_status).small().color(t.text_dim)); }
                });
            }
            if mode == PanelMode::Full && let Some(room) = room {
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut state.notes_tab, false, "Chat");
                        ui.selectable_value(&mut state.notes_tab, true, format!("Notes  {}", room.notes.len()));
                    });
                    if state.notes_tab {
                        ui.checkbox(&mut state.show_notes, "Show notes on canvas");
                        if state.show_notes {
                            for (id, note) in &room.notes {
                                let can_edit = note.author == room.local_peer || room.host == room.local_peer;
                                ui.collapsing(format!("{}: {}", room.members.get(&note.author).map_or("Artist", |p| p.profile.name.as_str()), note.text.chars().take(24).collect::<String>()), |ui| {
                                    ui.label(&note.text);
                                    let mut collapsed = note.collapsed;
                                    if ui.add_enabled(can_edit, egui::Checkbox::new(&mut collapsed, "Collapse on canvas")).changed() { action = Some(("collab.note", json!({"id":id,"text":note.text,"position":note.position,"collapsed":collapsed}))); }
                                    if ui.add_enabled(can_edit, egui::Button::new("Remove")).clicked() { action = Some(("collab.note.remove", json!({"id":id}))); }
                                });
                            }
                        }
                        ui.add(egui::TextEdit::multiline(&mut state.note_draft).hint_text("Comment on the drawing…").desired_rows(2));
                        if ui.button("Place note at canvas center").clicked() && !state.note_draft.trim().is_empty() { action = Some(("collab.note", json!({"text":state.note_draft,"position":center}))); }
                    } else {
                        egui::ScrollArea::vertical().id_salt("room-chat").max_height(160.0).stick_to_bottom(true).show(ui, |ui| {
                            for (author, text) in &room.chat {
                                let profile = room.members.get(author).map(|p| &p.profile);
                                ui.label(RichText::new(profile.map_or(author.as_str(), |p| p.name.as_str())).small().color(profile.map_or(t.accent, color_of)));
                                ui.label(text);
                                ui.add_space(4.0);
                            }
                        });
                        ui.add(egui::TextEdit::singleline(&mut state.chat_draft).hint_text("Message the room…").desired_width(f32::INFINITY));
                        if ui.button("Send").clicked() && !state.chat_draft.trim().is_empty() { action = Some(("collab.chat", json!({"text":state.chat_draft}))); }
                    }
            }
            ui.separator();
            if mode == PanelMode::Full {
            ui.collapsing("Your profile", |ui| {
                ui.label("Name"); ui.text_edit_singleline(&mut state.name);
                ui.label("Optional icon"); ui.text_edit_singleline(&mut state.icon);
                ui.color_edit_button_srgb(&mut state.color);
                ui.checkbox(&mut state.cursor_visible, "Show my cursor"); ui.checkbox(&mut state.name_visible, "Show my name");
                if ui.button("Update profile").clicked() { action = Some(("collab.profile", json!({"name":state.name,"icon":if state.icon.is_empty(){None}else{Some(state.icon.clone())},"visible":state.cursor_visible,"showName":state.name_visible,"color":[f32::from(state.color[0])/255.0,f32::from(state.color[1])/255.0,f32::from(state.color[2])/255.0,1.0]}))); }
            });
            egui::CollapsingHeader::new("Advanced").open(if state.error.to_lowercase().contains("password") { Some(true) } else { None }).show(ui, |ui| {
                ui.label("Signaling service"); ui.add_enabled(room.is_none() && !state.transport_pending, egui::TextEdit::singleline(&mut state.signaling_url));
                ui.label("Password (optional)"); ui.add(egui::TextEdit::singleline(&mut state.password).password(true));
                ui.checkbox(&mut state.show_cursors, "Show cursors"); ui.checkbox(&mut state.show_names, "Show names"); ui.checkbox(&mut state.show_notes, "Show sticky notes");
                if let Some(room) = room {
                    if ui.selectable_label(room.visibility_mode == photocraft_engine::collab::VisibilityMode::Shared, "Shared layer visibility").clicked() { action = Some(("collab.visibility", json!({"mode":"shared","layers":layers}))); }
                    if ui.selectable_label(room.visibility_mode == photocraft_engine::collab::VisibilityMode::Personal, "Personal layer visibility").clicked() { action = Some(("collab.visibility", json!({"mode":"personal","layers":layers}))); }
                }
                ui.label(RichText::new("Tools and selections stay personal. The host orders shared edits.").small().color(t.text_dim));
                ui.collapsing("Connection details", |ui| { ui.label(&state.transport_status); ui.label(state.transport_metrics.to_string()); });
            });
            }
            if room.is_some() && ui.button("Leave room").clicked() { action = Some(("collab.room.leave", json!({}))); }
        });
    });
    if !app.ui.collaboration.open || app.ui.collaboration.panel_mode == PanelMode::Collapsed {
        app.ui.collaboration.push_to_talk = false;
    }
    if let Some(url) = desktop_url {
        crate::links::open(app, root.ctx(), &url);
    }
    if let Some((id, params)) = action {
        match app.run(id, params) {
            Ok(value) => {
                app.ui.collaboration.error.clear();
                if id == "collab.chat" {
                    app.ui.collaboration.chat_draft.clear();
                }
                if id == "collab.note" {
                    app.ui.collaboration.note_draft.clear();
                }
                if let Some(code) = value.get("code").and_then(Value::as_str) {
                    app.ui.collaboration.code = code.into();
                }
            }
            Err(error) => app.ui.collaboration.error = error,
        }
    }
}

/// Draw remote presence in document coordinates, with the same zoom and mirror as the canvas.
pub fn draw_overlay(app: &PhotocraftApp, painter: &egui::Painter, xf: &crate::canvas::ViewXform) {
    let Some(room) = app.session.collaboration.room.as_ref() else { return };
    let Ok(room) = serde_json::to_value(room) else { return };
    let t = crate::theme::Tokens::get(painter.ctx());
    if app.ui.collaboration.show_cursors
        && let Some(members) = room.get("members").and_then(Value::as_object)
    {
        for (id, member) in members {
            let profile = member.get("profile").unwrap_or(member);
            if room.get("local_peer").and_then(Value::as_str) == Some(id.as_str()) || profile.get("visible").and_then(Value::as_bool) == Some(false) {
                continue;
            }
            let position = member.get("position").or_else(|| member.get("cursor")).and_then(Value::as_array);
            let Some(position) = position else { continue };
            let (Some(x), Some(y)) = (position.first().and_then(Value::as_f64), position.get(1).and_then(Value::as_f64)) else { continue };
            if !x.is_finite() || !y.is_finite() {
                continue;
            }
            let pos = xf.to_screen(x as f32, y as f32);
            let color = profile
                .get("color")
                .and_then(Value::as_array)
                .and_then(|c| {
                    Some(egui::Color32::from_rgb(
                        (c.first()?.as_f64()?.clamp(0.0, 1.0) * 255.0) as u8,
                        (c.get(1)?.as_f64()?.clamp(0.0, 1.0) * 255.0) as u8,
                        (c.get(2)?.as_f64()?.clamp(0.0, 1.0) * 255.0) as u8,
                    ))
                })
                .unwrap_or(t.accent);
            painter.circle_stroke(pos, 5.0, egui::Stroke::new(2.0, color));
            if app.ui.collaboration.show_names && profile.get("showName").and_then(Value::as_bool) != Some(false) {
                let name = profile.get("name").and_then(Value::as_str).unwrap_or(id);
                let icon = profile.get("icon").and_then(Value::as_str).unwrap_or("");
                painter.text(pos + egui::vec2(9.0, -9.0), egui::Align2::LEFT_BOTTOM, format!("{icon} {name}"), egui::FontId::proportional(12.0), color);
            }
        }
    }

    if app.ui.collaboration.show_notes
        && let Some(notes) = room.get("notes").and_then(Value::as_object)
    {
        for note in notes.values() {
            let Some(position) = note.get("position").and_then(Value::as_array) else { continue };
            let (Some(x), Some(y)) = (position.first().and_then(Value::as_f64), position.get(1).and_then(Value::as_f64)) else { continue };
            if !x.is_finite() || !y.is_finite() {
                continue;
            }
            let pos = xf.to_screen(x as f32, y as f32);
            painter.text(pos, egui::Align2::LEFT_TOP, "▣", egui::FontId::proportional(18.0), t.accent);
            if note.get("collapsed").and_then(Value::as_bool) != Some(true) {
                let text = note.get("text").and_then(Value::as_str).unwrap_or("");
                let origin = pos + egui::vec2(21.0, 0.0);
                let galley = painter.layout_no_wrap(text.into(), egui::FontId::proportional(12.0), t.text);
                let rect = egui::Rect::from_min_size(origin, galley.size() + egui::vec2(12.0, 8.0));
                painter.rect_filled(rect, t.radius_sm, t.card);
                painter.rect_stroke(rect, t.radius_sm, egui::Stroke::new(1.0, t.card_border), egui::StrokeKind::Inside);
                painter.galley(origin + egui::vec2(6.0, 4.0), galley, t.text);
            }
        }
    }
}

/// Presence is sampled at 20 Hz independently of brush traffic.
pub fn pointer(app: &mut PhotocraftApp, ctx: &egui::Context, position: [f64; 2]) {
    let now = ctx.input(|i| i.time);
    if now - app.ui.collaboration.last_cursor_sent < 0.05 {
        return;
    }
    let Some(peer) = app.session.collaboration.room.as_ref().map(|room| room.local_peer.clone()) else { return };
    let layer = app.session.active().and_then(|s| s.active_layer);
    let tool = app.ui.tool.label();
    let payload = json!({"position": position, "tool": tool, "layer": layer, "speaking": app.ui.collaboration.push_to_talk});
    app.ui.collaboration.last_cursor_sent = now;
    if app.ui.collaboration.last_cursor_payload.as_ref().is_some_and(|(previous_peer, previous)| previous_peer == &peer && previous == &payload) {
        return;
    }
    match app.run("collab.cursor", payload.clone()) {
        Ok(_) => app.ui.collaboration.last_cursor_payload = Some((peer, payload)),
        Err(error) => app.ui.collaboration.error = error,
    }
}

/// Automation-facing local room preferences, including the signaling server before admission.
pub fn settings(app: &mut PhotocraftApp, params: &Value) -> Result<Value, String> {
    let mut next = app.ui.collaboration.clone();
    let state = &mut next;
    for (key, slot) in [("name", &mut state.name), ("icon", &mut state.icon), ("signalingUrl", &mut state.signaling_url)] {
        if let Some(value) = params.get(key) {
            let value = value.as_str().ok_or_else(|| format!("{key} must be text"))?;
            if value.len() > 1024 {
                return Err(format!("{key} is too long"));
            }
            if key == "signalingUrl" && value != slot.as_str() && (app.session.collaboration.room.is_some() || state.transport_pending) {
                return Err("Leave the room before changing its signaling service".into());
            }
            *slot = value.into();
        }
    }
    for (key, slot) in [
        ("showCursors", &mut state.show_cursors),
        ("showNames", &mut state.show_names),
        ("showNotes", &mut state.show_notes),
        ("voiceEnabled", &mut state.voice_enabled),
        ("open", &mut state.open),
        ("cursorVisible", &mut state.cursor_visible),
        ("nameVisible", &mut state.name_visible),
    ] {
        if let Some(value) = params.get(key) {
            *slot = value.as_bool().ok_or_else(|| format!("{key} must be boolean"))?;
        }
    }
    if let Some(mode) = params.get("panelMode").or_else(|| params.get("panel_mode")) {
        state.panel_mode = serde_json::from_value(mode.clone()).map_err(|_| "panelMode must be full, minimal, or collapsed".to_string())?;
    }
    if !state.open || state.panel_mode == PanelMode::Collapsed || !state.voice_enabled {
        state.push_to_talk = false;
    }
    if let Some(color) = params.get("color") {
        state.color = serde_json::from_value(color.clone()).map_err(|e| e.to_string())?;
    }
    let value = serde_json::to_value(&next).map_err(|e| e.to_string())?;
    app.ui.collaboration = next;
    Ok(value)
}

/// Room History shows only this participant's completed edits, including their undone entries.
pub fn history(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let entries = app.session.collaboration.own_history();
    ui.label("My room history");
    let mut command = None;
    ui.horizontal(|ui| {
        if ui.button("Undo my edit").clicked() {
            command = Some("collab.undo");
        }
        if ui.button("Redo my edit").clicked() {
            command = Some("collab.redo");
        }
    });
    egui::ScrollArea::vertical().id_salt("own-room-history").max_height(ui.available_height().max(40.0)).show(ui, |ui| {
        if entries.is_empty() {
            ui.label(RichText::new("No completed edits yet").color(t.text_dim));
        }
        for (index, (id, visible)) in entries.iter().enumerate() {
            let label = format!("Edit {}{}", index + 1, if *visible { "" } else { " — undone" });
            ui.label(RichText::new(label).color(if *visible { t.text } else { t.text_faint })).on_hover_text(id);
        }
    });
    if let Some(command) = command {
        let _ = app.run(command, json!({}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dock_modes_reserve_space_and_collapsing_stops_talk() {
        let mut app = PhotocraftApp::new(Default::default(), Default::default());
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let mut widths = Vec::new();
        for mode in [PanelMode::Full, PanelMode::Minimal, PanelMode::Collapsed] {
            app.ui.collaboration.open = true;
            app.ui.collaboration.panel_mode = mode;
            app.ui.collaboration.voice_enabled = true;
            app.ui.collaboration.push_to_talk = true;
            ctx.run_ui(
                egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 700.0))), ..Default::default() },
                |ui| {
                    let before = ui.available_width();
                    dock(&mut app, ui);
                    widths.push(before - ui.available_width());
                },
            )
            .textures_delta
            .clear();
            assert!(!app.ui.collaboration.push_to_talk);
        }
        assert!(widths[0] > widths[1] && widths[1] > 150.0, "{widths:?}");
        assert_eq!(widths[2], 0.0);
        app.ui.collaboration.push_to_talk = true;
        settings(&mut app, &json!({"open":false})).unwrap();
        assert!(!app.ui.collaboration.push_to_talk);
        app.ui.collaboration.open = true;
        crate::menus::invoke(&mut app, &ctx, "window.collaboration", json!({})).unwrap();
        assert!(app.ui.collaboration.open);
        assert_eq!(app.ui.collaboration.panel_mode, PanelMode::Full);
    }

    #[test]
    fn join_os_event_preserves_documents_and_same_room_is_idempotent() {
        for saved in [false, true] {
            let mut app = PhotocraftApp::new(Default::default(), Default::default());
            app.run("file.new", json!({"width":16,"height":16,"name":"Keep my drawing"})).unwrap();
            if saved {
                let state = app.session.active_mut().unwrap();
                state.saved_revision = state.revision;
            }
            let before = app.session.active().unwrap().doc.clone();
            let dirty = app.session.active().unwrap().is_dirty();
            let mut events = vec![crate::OsEvent::JoinRoom("photocraft://join/ROOM01".into())];
            app.services.os_events = Some(Box::new(move || std::mem::take(&mut events)));
            app.drain_os_events(&egui::Context::default());
            assert_eq!(app.session.documents().len(), 1);
            assert_eq!(app.session.active().unwrap().doc.id, before.id);
            assert_eq!(app.session.active().unwrap().doc.name, before.name);
            assert_eq!(app.session.active().unwrap().is_dirty(), dirty);
            assert_eq!(app.ui.collaboration.code, "ROOM01");
            let intents = app.session.collaboration.network_intents.len();
            open_invite(&mut app, "room01").unwrap();
            assert_eq!(app.session.collaboration.network_intents.len(), intents);
            app.session.collaboration.network_intents.clear();
            app.ui.collaboration.transport_status = "Connected to host".into();
            open_invite(&mut app, "ROOM01").unwrap();
            assert!(app.session.collaboration.network_intents.is_empty());
            assert!(open_invite(&mut app, "OTHER1").is_err());
            assert_eq!(app.ui.collaboration.code, "ROOM01");
            assert_eq!(app.session.active().unwrap().doc.id, before.id);
            app.ui.collaboration.transport_status = "Disconnected".into();
            open_invite(&mut app, "ROOM01").unwrap();
            assert!(!app.session.collaboration.network_intents.is_empty(), "failed admission can retry");
        }
    }

    #[test]
    fn panel_mode_settings_are_serde_compatible_atomic_and_stop_voice() {
        let mut app = PhotocraftApp::new(Default::default(), Default::default());
        let old: CollaborationUi = serde_json::from_value(json!({})).unwrap();
        assert_eq!(old.panel_mode, PanelMode::Full);
        app.ui.collaboration.push_to_talk = true;
        settings(&mut app, &json!({"panelMode":"collapsed"})).unwrap();
        assert_eq!(app.ui.collaboration.panel_mode, PanelMode::Collapsed);
        assert!(!app.ui.collaboration.push_to_talk);
        assert!(settings(&mut app, &json!({"name":"Changed","panelMode":"huge"})).is_err());
        assert_eq!(app.ui.collaboration.name, "Artist");
        settings(&mut app, &json!({"panel_mode":"minimal"})).unwrap();
        assert_eq!(app.ui.collaboration.panel_mode, PanelMode::Minimal);
    }

    #[test]
    fn stationary_cursor_does_not_emit_idle_presence_traffic() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.run("file.new", json!({"width":8,"height":8})).unwrap();
        app.run("collab.room.create", json!({"code":"CURSOR","peer":"alice"})).unwrap();
        app.session.collaboration.queued_presence = None;
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput { time: Some(1.0), ..Default::default() }, |ui| pointer(&mut app, ui.ctx(), [2.0, 2.0])).textures_delta.clear();
        assert!(app.session.collaboration.queued_presence.take().is_some());
        ctx.run_ui(egui::RawInput { time: Some(1.1), ..Default::default() }, |ui| pointer(&mut app, ui.ctx(), [2.0, 2.0])).textures_delta.clear();
        assert!(app.session.collaboration.queued_presence.is_none());
        ctx.run_ui(egui::RawInput { time: Some(1.2), ..Default::default() }, |ui| pointer(&mut app, ui.ctx(), [3.0, 2.0])).textures_delta.clear();
        assert!(app.session.collaboration.queued_presence.is_some());
        assert!(app.session.collaboration.canonical_outbox.is_empty());
    }

    #[test]
    fn settings_cannot_forge_transport_readiness_and_invalid_patch_is_atomic() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        settings(&mut app, &json!({"name":"Sam", "transport_pending":false, "transport_status":"Connected"})).unwrap();
        assert!(crate::menus::is_enabled(&app, "window.collaboration"));
        assert!(crate::menus::is_enabled(&app, "window.collaboration.settings"));
        assert_eq!(app.ui.collaboration.name, "Sam");
        assert_eq!(app.ui.collaboration.transport_status, "Disconnected");
        assert!(settings(&mut app, &json!({"name":"Changed", "color":[999,0,0]})).is_err());
        assert_eq!(app.ui.collaboration.name, "Sam");
    }
    #[test]
    fn room_password_is_not_persisted() {
        let state = CollaborationUi { password: "private".into(), ..Default::default() };
        let value = serde_json::to_value(state).expect("serialize UI state");
        assert!(value.get("password").is_none());
        let restored: CollaborationUi = serde_json::from_value(json!({})).expect("old UI state");
        assert!(restored.show_cursors && restored.show_names && restored.show_notes);
    }
}
