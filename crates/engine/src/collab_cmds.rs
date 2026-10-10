//! Typed collaborative room commands; tool and selection state stay private.
use crate::{EngineError, Result, Session, commands::CommandSpec};
use photocraft_collab::*;
use serde_json::{Value, json};
fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: "collab".into(), msg: msg.into() }
}
fn string<'a>(p: &'a Value, key: &str) -> Result<&'a str> {
    p.get(key).and_then(Value::as_str).filter(|s| !s.is_empty() && s.len() <= 128).ok_or_else(|| bad(format!("missing or invalid {key}")))
}
fn enabled(_: &Session) -> std::result::Result<(), String> {
    Ok(())
}
fn room(s: &mut Session, p: &Value, create: bool) -> Result<Value> {
    if create && s.active().is_none() {
        return Err(EngineError::NoDocument);
    }
    if p.get("name").and_then(Value::as_str).is_some_and(|name| name.is_empty() || name.len() > 64) {
        return Err(bad("name must be1..64 bytes"));
    }
    let token = room_token();
    let peer = p.get("peer").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("artist-{token}"));
    let code = if create {
        p.get("code").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string).unwrap_or(token)
    } else {
        string(p, "code")?.to_string()
    }
    .to_ascii_uppercase();
    if peer.is_empty() || peer.len() > 128 || code.len() < 4 || code.len() > 12 || !code.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(bad("invalid peer or room code"));
    }
    if create && let Some(why) = s.job_conflict("collab.room.create", true) {
        return Err(EngineError::Other(why));
    }
    let password = p.get("password").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
    if password.as_ref().is_some_and(|p| p.len() > 256) {
        return Err(bad("password too long"));
    }
    let server = p.get("signalingUrl").and_then(Value::as_str).unwrap_or("http://127.0.0.1:5548").to_string();
    s.collaboration = crate::collab::Collaboration::default();
    s.collaboration.document_id = s.active().map(|st| st.doc.id);
    s.collaboration.room = Some(RoomState::new(code.clone(), if create { peer.clone() } else { "host".into() }, peer.clone()));
    s.collaboration.network_intents.push(if create {
        crate::collab::NetworkIntent::Create { code: code.clone(), password, server }
    } else {
        crate::collab::NetworkIntent::Join { code: code.clone(), password, server }
    });
    let profile = Profile { name: p.get("name").and_then(Value::as_str).unwrap_or("Artist").into(), ..Default::default() };
    let presence = Presence { profile, position: None, layer: s.active().and_then(|d| d.active_layer), tool: "brush".into(), speaking: false };
    s.collaboration_submit(Operation::Presence { presence })?;
    Ok(json!({"code":code,"peer":peer}))
}
fn room_token() -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let counter = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    #[cfg(not(target_arch = "wasm32"))]
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(u128::from(counter), |d| d.as_nanos());
    #[cfg(target_arch = "wasm32")]
    let stamp = u128::from(counter);
    blake3::hash(format!("{stamp}-{counter}").as_bytes()).to_hex().chars().take(8).collect::<String>().to_ascii_uppercase()
}
fn create(s: &mut Session, p: &Value) -> Result<Value> {
    room(s, p, true)
}
fn join(s: &mut Session, p: &Value) -> Result<Value> {
    room(s, p, false)
}
fn leave(s: &mut Session, _: &Value) -> Result<Value> {
    s.collaboration = Default::default();
    s.collaboration.network_intents.push(crate::collab::NetworkIntent::Leave);
    Ok(Value::Null)
}
fn submit(s: &mut Session, p: &Value) -> Result<Value> {
    let op: Operation = serde_json::from_value(p.clone()).map_err(|e| bad(e.to_string()))?;
    s.collaboration_submit(op)?;
    Ok(Value::Null)
}
fn receive(s: &mut Session, p: &Value) -> Result<Value> {
    let event: HostMessage = serde_json::from_value(p.clone()).map_err(|e| bad(e.to_string()))?;
    s.collaboration_receive(&event)?;
    Ok(Value::Null)
}
fn chat(s: &mut Session, p: &Value) -> Result<Value> {
    let text = p.get("text").and_then(Value::as_str).ok_or_else(|| bad("missing text"))?.to_string();
    s.collaboration_submit(Operation::Chat { text })?;
    Ok(Value::Null)
}
fn profile(s: &mut Session, p: &Value) -> Result<Value> {
    let room = s.collaboration.room.as_ref().ok_or_else(|| bad("not in a room"))?;
    let old = room.members.get(&room.local_peer).cloned().unwrap_or(Presence {
        profile: Default::default(),
        position: None,
        layer: None,
        tool: "brush".into(),
        speaking: false,
    });
    let mut value = serde_json::to_value(&old.profile).map_err(|e| bad(e.to_string()))?;
    if let (Some(dst), Some(src)) = (value.as_object_mut(), p.as_object()) {
        for (k, v) in src {
            dst.insert(k.clone(), v.clone());
        }
    }
    let profile = serde_json::from_value(value).map_err(|e| bad(e.to_string()))?;
    s.collaboration_submit(Operation::Presence { presence: Presence { profile, ..old } })?;
    Ok(Value::Null)
}
fn cursor(s: &mut Session, p: &Value) -> Result<Value> {
    let room = s.collaboration.room.as_ref().ok_or_else(|| bad("not in a room"))?;
    let mut presence = room.members.get(&room.local_peer).cloned().unwrap_or(Presence {
        profile: Default::default(),
        position: None,
        layer: None,
        tool: "brush".into(),
        speaking: false,
    });
    presence.position = Some(serde_json::from_value(p.get("position").cloned().ok_or_else(|| bad("missing position"))?).map_err(|e| bad(e.to_string()))?);
    if let Some(tool) = p.get("tool").and_then(Value::as_str) {
        presence.tool = tool.into();
    }
    if let Some(layer) = p.get("layer") {
        presence.layer = serde_json::from_value(layer.clone()).map_err(|e| bad(e.to_string()))?;
    }
    if let Some(speaking) = p.get("speaking").and_then(Value::as_bool) {
        presence.speaking = speaking;
    }
    s.collaboration_submit(Operation::Presence { presence })?;
    Ok(Value::Null)
}
fn note(s: &mut Session, p: &Value) -> Result<Value> {
    let author = s.collaboration.room.as_ref().ok_or_else(|| bad("not in a room"))?.local_peer.clone();
    let id =
        p.get("id").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("{author}-note-{}", s.collaboration.next_nonce.saturating_add(1)));
    let author = if let Some(existing) = s.collaboration.room.as_ref().and_then(|r| r.notes.get(&id)) {
        let room = s.collaboration.room.as_ref().ok_or_else(|| bad("not in a room"))?;
        if existing.author != room.local_peer && room.host != room.local_peer {
            return Err(bad("only note author or host can edit note"));
        }
        existing.author.clone()
    } else {
        author
    };
    let position = serde_json::from_value(p.get("position").cloned().ok_or_else(|| bad("missing position"))?).map_err(|e| bad(e.to_string()))?;
    s.collaboration_submit(Operation::Note {
        note: StickyNote {
            id,
            author,
            position,
            text: p.get("text").and_then(Value::as_str).ok_or_else(|| bad("missing text"))?.into(),
            collapsed: p.get("collapsed").and_then(Value::as_bool).unwrap_or(false),
        },
    })?;
    Ok(Value::Null)
}
fn remove_note(s: &mut Session, p: &Value) -> Result<Value> {
    s.collaboration_submit(Operation::RemoveNote { id: string(p, "id")?.into() })?;
    Ok(Value::Null)
}
fn visibility(s: &mut Session, p: &Value) -> Result<Value> {
    let mode = serde_json::from_value(p.get("mode").cloned().ok_or_else(|| bad("missing mode"))?).map_err(|e| bad(e.to_string()))?;
    let mut layers: std::collections::BTreeMap<photocraft_doc::LayerId, bool> =
        serde_json::from_value(p.get("layers").cloned().unwrap_or(json!({}))).map_err(|e| bad(e.to_string()))?;
    if mode == VisibilityMode::Shared
        && layers.is_empty()
        && let Some(state) = s.active()
    {
        layers = state.doc.walk().into_iter().map(|(_, _, l)| (l.id, l.visible)).collect();
    }
    s.collaboration_submit(Operation::Visibility { mode, layers })?;
    Ok(Value::Null)
}
fn undo(s: &mut Session, _: &Value) -> Result<Value> {
    s.collaboration_submit(Operation::Undo)?;
    Ok(Value::Null)
}
fn redo(s: &mut Session, _: &Value) -> Result<Value> {
    s.collaboration_submit(Operation::Redo)?;
    Ok(Value::Null)
}
fn stroke_begin(s: &mut Session, p: &Value) -> Result<Value> {
    let id = string(p, "id")?.to_string();
    let command = p.get("command").and_then(Value::as_str).unwrap_or("paint.stroke");
    let params = p.get("params").cloned().unwrap_or(json!({}));
    if s.active().map(|st| st.doc.id) != s.collaboration.document_id {
        return Err(bad("activate the room document before drawing"));
    }
    let brush = crate::brush_cmds::collaboration_brush(s, &params, command)?;
    let state = s.active().ok_or(EngineError::NoDocument)?;
    let layer = params.get("layer").and_then(Value::as_u64).map(photocraft_doc::LayerId).or(state.active_layer).ok_or_else(|| bad("no target layer"))?;
    let lock_transparency = state.doc.layer(layer).ok_or(EngineError::NoLayer(layer))?.locks.transparency;
    let mut selection = Vec::new();
    if let Some(mask) = state.doc.selection.as_ref() {
        let bounds = mask.content_bounds();
        if i64::from(bounds.width()).saturating_mul(i64::from(bounds.height())) > 1_048_576 {
            return Err(bad("selection too large for collaboration; simplify selection"));
        }
        for y in bounds.y0..bounds.y1 {
            let mut coverage = Vec::new();
            for x in bounds.x0..bounds.x1 {
                coverage.push(mask.pixel(x, y).first().copied().unwrap_or(0.0));
            }
            selection.push(SelectionRun { x: bounds.x0, y, coverage });
        }
    }
    let seed = brush.seed;
    s.collaboration_submit(Operation::StrokeBegin {
        stroke: Box::new(StrokeStart {
            id,
            layer,
            brush,
            selection,
            lock_transparency,
            zoom: params.get("zoom").and_then(Value::as_f64).unwrap_or(1.0) as f32,
        }),
    })?;
    Ok(json!({"seed":seed}))
}
pub fn specs() -> Vec<CommandSpec> {
    type Run = fn(&mut Session, &Value) -> Result<Value>;
    let commands: [(&'static str, &'static str, Run); 14] = [
        ("collab.room.create", "Create Collaboration Room", create),
        ("collab.room.join", "Join Collaboration Room", join),
        ("collab.room.leave", "Leave Collaboration Room", leave),
        ("collab.submit", "Submit Collaboration Input", submit),
        ("collab.stroke.begin", "Begin Collaborative Stroke", stroke_begin),
        ("collab.receive", "Receive Host Operation", receive),
        ("collab.profile", "Collaboration Profile", profile),
        ("collab.cursor", "Collaboration Cursor", cursor),
        ("collab.chat", "Room Chat", chat),
        ("collab.note", "Place Sticky Note", note),
        ("collab.note.remove", "Remove Sticky Note", remove_note),
        ("collab.visibility", "Layer Visibility Mode", visibility),
        ("collab.undo", "Undo My Edit", undo),
        ("collab.redo", "Redo My Edit", redo),
    ];
    commands
        .into_iter()
        .map(|(id, label, run)| CommandSpec {
            id,
            label,
            menu: &[],
            shortcut: None,
            params: "Typed collaborative room parameters; see collaboration protocol",
            enabled,
            run,
            journal: false,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guest_can_join_from_home_without_local_document() {
        let mut s = Session::new();
        assert!(s.execute("collab.room.join", json!({"code":"TEST01","name":"Guest"})).is_ok());
        assert!(s.collaboration.room.is_some());
        assert!(s.collaboration.document_id.is_none());
        assert!(s.collaboration.queued_presence.is_some());
    }
}
