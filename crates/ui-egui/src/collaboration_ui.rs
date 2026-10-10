//! Native room controls. Shared changes are engine commands; display preferences stay local.
use crate::PhotocraftApp;
use egui::RichText;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CollaborationUi {
    pub open: bool,
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
            transport_pending: false,
        }
    }
}

pub fn window(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if !app.ui.collaboration.open {
        app.ui.collaboration.push_to_talk = false;
        return;
    }
    let room = app.session.collaboration.room.as_ref().and_then(|room| serde_json::to_value(room).ok());
    let center = app.session.active().map(|s| [f64::from(s.doc.size.width) / 2.0, f64::from(s.doc.size.height) / 2.0]).unwrap_or([0.0, 0.0]);
    let layers = app
        .session
        .active()
        .map(|s| {
            let mut pending: Vec<_> = s.doc.layers.iter().collect();
            let mut values = serde_json::Map::new();
            while let Some(layer) = pending.pop() {
                values.insert(layer.id.0.to_string(), Value::Bool(layer.visible));
                if let Some(children) = layer.children() {
                    pending.extend(children);
                }
            }
            values
        })
        .unwrap_or_default();
    let mut open = true;
    let mut action: Option<(&str, Value)> = None;
    let state = &mut app.ui.collaboration;
    egui::Window::new("Collaboration").open(&mut open).default_width(360.0).show(ctx, |ui| {
        let t = crate::theme::Tokens::get(ctx);
        ui.label(RichText::new(&state.transport_status).color(t.text));
        if !state.error.is_empty() {
            ui.label(RichText::new(&state.error).color(t.accent));
        }
        ui.horizontal(|ui| {
            ui.label("Your name");
            ui.text_edit_singleline(&mut state.name);
        });
        ui.horizontal(|ui| {
            ui.label("Icon");
            ui.text_edit_singleline(&mut state.icon);
            ui.color_edit_button_srgb(&mut state.color);
        });
        ui.horizontal(|ui| { ui.checkbox(&mut state.cursor_visible, "Show my cursor"); ui.checkbox(&mut state.name_visible, "Show my name"); });
        if ui.button("Update profile").clicked() {
            action = Some(("collab.profile", json!({"name": state.name, "icon": if state.icon.is_empty() { None } else { Some(state.icon.clone()) }, "visible":state.cursor_visible, "showName":state.name_visible, "color": [f32::from(state.color[0])/255.0, f32::from(state.color[1])/255.0, f32::from(state.color[2])/255.0, 1.0]})));
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Signaling server");
            ui.text_edit_singleline(&mut state.signaling_url);
        });
        ui.horizontal(|ui| {
            ui.label("Room code");
            ui.text_edit_singleline(&mut state.code);
        });
        ui.horizontal(|ui| {
            ui.label("Password (optional)");
            ui.add(egui::TextEdit::singleline(&mut state.password).password(true));
        });
        ui.horizontal(|ui| {
            if ui.button("Create room").clicked() {
                action = Some(("collab.room.create", json!({"name": state.name, "password": state.password, "signalingUrl": state.signaling_url})));
            }
            if ui.button("Join room").clicked() {
                action = Some((
                    "collab.room.join",
                    json!({"code": state.code, "name": state.name, "password": state.password, "signalingUrl": state.signaling_url}),
                ));
            }
            if ui.button("Leave").clicked() {
                action = Some(("collab.room.leave", json!({})));
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            ui.checkbox(&mut state.show_cursors, "Cursors");
            ui.checkbox(&mut state.show_names, "Names");
            ui.checkbox(&mut state.show_notes, "Sticky notes");
        });
        ui.label("Tools and selections belong to each artist. The host orders shared edits.");
        if let Some(room) = &room {
            if let Some(code) = room.get("code").and_then(Value::as_str) {
                ui.label(format!("Room: {code}"));
            }
            ui.horizontal(|ui| {
                if ui.selectable_label(room.get("visibility_mode").and_then(Value::as_str) == Some("shared"), "Shared layer visibility").clicked() {
                    action = Some(("collab.visibility", json!({"mode":"shared", "layers":layers})));
                }
                if ui.selectable_label(room.get("visibility_mode").and_then(Value::as_str) == Some("personal"), "Personal layer visibility").clicked() {
                    action = Some(("collab.visibility", json!({"mode":"personal", "layers":layers})));
                }
            });
            ui.collapsing("Participants", |ui| {
                if let Some(members) = room.get("members").and_then(Value::as_object) {
                    for (id, member) in members {
                        ui.label(member.get("profile").unwrap_or(member).get("name").and_then(Value::as_str).unwrap_or(id));
                    }
                }
            });
            if let Some(messages) = room.get("chat").and_then(Value::as_array) {
                egui::ScrollArea::vertical().max_height(130.0).show(ui, |ui| {
                    for message in messages {
                        if let Some(parts) = message.as_array() {
                            let author = parts.first().and_then(Value::as_str).unwrap_or("Artist");
                            let text = parts.get(1).and_then(Value::as_str).unwrap_or("");
                            let name = room.get("members").and_then(|m|m.get(author)).and_then(|m|m.get("profile")).and_then(|p|p.get("name")).and_then(Value::as_str).unwrap_or(author);
                            ui.label(format!("{name}: {text}"));
                        }
                    }
                });
            }
            if state.show_notes {
                ui.collapsing("Sticky notes", |ui| {
                    if let Some(notes) = room.get("notes").and_then(Value::as_object) {
                        for (id, note) in notes {
                            let text = note.get("text").and_then(Value::as_str).unwrap_or("");
                            let can_edit = note.get("author") == room.get("local_peer") || room.get("host") == room.get("local_peer");
                            ui.collapsing(id, |ui| {
                                ui.label(text);
                                let mut collapsed = note.get("collapsed").and_then(Value::as_bool).unwrap_or(false);
                                if ui.add_enabled(can_edit, egui::Checkbox::new(&mut collapsed, "Collapse on canvas")).changed() {
                                    action = Some(("collab.note", json!({"id":id,"text":text,"position":note.get("position"),"collapsed":collapsed})));
                                }
                                if ui.add_enabled(can_edit, egui::Button::new("Remove")).clicked() {
                                    action = Some(("collab.note.remove", json!({"id":id})));
                                }
                            });
                        }
                    }
                });
            }
        }
        ui.separator();
        ui.checkbox(&mut state.voice_enabled, "Enable voice");
        let talk = ui.button("Hold to talk");
        state.push_to_talk = state.voice_enabled && talk.is_pointer_button_down_on();
        ui.label(&state.voice_status);
        ui.separator();
        ui.label("Room chat");
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut state.chat_draft);
            if ui.button("Send").clicked() && !state.chat_draft.trim().is_empty() {
                action = Some(("collab.chat", json!({"text": state.chat_draft})));
            }
        });
        ui.label("Sticky note at canvas center");
        ui.text_edit_multiline(&mut state.note_draft);
        if ui.button("Place note").clicked() && !state.note_draft.trim().is_empty() {
            action = Some(("collab.note", json!({"text": state.note_draft, "position": center})));
        }
    });
    app.ui.collaboration.open = open;
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
                if matches!(id, "collab.room.create" | "collab.room.join") && app.ui.collaboration.transport_status != "Disconnected" {
                    app.ui.collaboration.transport_pending = true;
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
