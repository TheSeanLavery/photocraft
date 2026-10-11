//! Direct, fallible history navigation. Only the selected state is hydrated.
use crate::commands::int;
use crate::image_cmds::reidentify_copied_layers;
use crate::{CommandSpec, EngineError, Result, Session};
use serde_json::{Value, json};

fn enabled(s: &Session) -> std::result::Result<(), String> {
    let state = s.active().ok_or("no document open")?;
    if s.collaboration.room.is_some() && s.collaboration.document_id == Some(state.doc.id) {
        return Err("Shared documents use personal collaboration undo".into());
    }
    Ok(())
}

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

/// The `state` param as an index into the active document's history entries, defaulting to the
/// current state.
fn state_index(s: &Session, cmd: &str, p: &Value) -> Result<(usize, usize)> {
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let current = st.history.past_len();
    let i = match p.get("state") {
        None | Some(Value::Null) => current,
        Some(_) => {
            let v = int(p, "state").ok_or_else(|| bad(cmd, "`state` must be a number"))?;
            usize::try_from(v).ok().filter(|i| *i <= current).ok_or_else(|| bad(cmd, format!("`state` = {v} is not a state (0..={current})")))?
        }
    };
    Ok((i, current))
}

/// History panel › Delete: drop the state and every state after it, as Photoshop does in linear
/// history. The document returns to the state before it, and the deleted steps can't be redone.
/// The oldest state (the document as opened) can't be deleted.
fn delete_state(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "history.deleteState";
    let (i, _current) = state_index(s, CMD, p)?;
    if i == 0 {
        return Err(bad(CMD, "the oldest state can't be deleted"));
    }
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let checkpoint = st.history.checkpoint();
    let deleted = checkpoint.undo.get(i).map(|state| state.identity.id).unwrap_or_else(|| st.history.current_id());
    let destination = checkpoint.undo.get(i - 1).cloned().ok_or_else(|| bad(CMD, "the previous state is unavailable"))?;
    let destination_id = destination.identity.id;
    crate::jobs::run(
        s,
        "Loading history state",
        true,
        move |ctx| {
            ctx.check()?;
            let document = destination.load_document().map_err(EngineError::Other)?;
            ctx.check()?;
            Ok(document)
        },
        move |session, document| {
            session.apply_history_destination(destination_id, document, None)?;
            let st = session.active_mut().ok_or(EngineError::NoDocument)?;
            st.history.prune_descendants(deleted);
            let entries = st.history.entries();
            Ok(json!({ "states": entries.len(), "current": entries.last() }))
        },
    )
}

/// History panel › New Document: a new untitled document holding the document as it was at a
/// state (the current one by default), named after that state, with a history of its own.
fn new_document(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "history.newDocument";
    let (i, current) = state_index(s, CMD, p)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let source = if i == current {
        photocraft_ops::HistoryState::from_document(st.history.entries().get(i).cloned().unwrap_or_default(), st.doc.clone())
    } else {
        st.history.checkpoint().undo.get(i).cloned().ok_or_else(|| bad(CMD, format!("state {i} is not held")))?
    };
    let label = st.history.entries().get(i).cloned().unwrap_or_default();
    let requested_name = p.get("name").and_then(Value::as_str).map(str::to_owned);
    crate::jobs::run(
        s,
        "Loading history state",
        true,
        move |ctx| {
            ctx.check()?;
            let document = source.load_document().map_err(EngineError::Other)?;
            ctx.check()?;
            Ok(document)
        },
        move |session, source| {
            let mut doc = (*source).clone();
            doc.id = photocraft_doc::DocId::fresh();
            doc.name = match requested_name.as_deref() {
                Some(name) if !name.trim().is_empty() => name.to_owned(),
                _ if !label.trim().is_empty() => label,
                _ => format!("{} copy", doc.name),
            };
            reidentify_copied_layers(&mut doc);
            let name = doc.name.clone();
            let index = session.add_document(doc, None);
            Ok(json!({ "document": index, "name": name }))
        },
    )
}

fn can_delete(s: &Session) -> std::result::Result<(), String> {
    enabled(s)?;
    s.active().filter(|d| d.history.can_undo()).map(|_| ()).ok_or_else(|| "the oldest state can't be deleted".into())
}

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "history.deleteState",
            label: "Delete State",
            menu: &[],
            shortcut: None,
            params: r##"{"state":index?} (History entry, 0 = oldest; default the current state) → {states,current}: removes the state and every later one"##,
            enabled: can_delete,
            run: delete_state,
            journal: true,
        },
        CommandSpec {
            id: "history.newDocument",
            label: "New Document from State",
            menu: &[],
            shortcut: None,
            params: r##"{"state":index?,"name":str?} (History entry, 0 = oldest; default the current state) → {document,name}"##,
            enabled: has_doc,
            run: new_document,
            journal: true,
        },
        CommandSpec {
            id: "history.jump",
            label: "Jump to History State",
            menu: &[],
            shortcut: None,
            params: r#"{"state":stable_state_id}"#,
            enabled,
            run: jump,
            journal: true,
        },
        CommandSpec { id: "history.inspect", label: "Inspect History", menu: &[], shortcut: None, params: "{}", enabled, run: inspect, journal: false },
        CommandSpec {
            id: "history.retryStorage",
            label: "Retry History Storage",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled,
            run: |s, _| {
                s.configure_history_cache();
                Ok(json!({"ok":true}))
            },
            journal: false,
        },
    ]
}

fn inspect(s: &mut Session, _: &Value) -> Result<Value> {
    let state = s.active().ok_or(EngineError::NoDocument)?;
    Ok(json!({"current":state.history.current_id(), "unlimited":s.prefs().performance.unlimited_history,
        "storageError":s.history_storage_error(), "states":state.history.timeline().iter().map(|entry|
        json!({"id":entry.identity.id,"parent":entry.identity.parent,"branch":entry.identity.branch,"action":entry.label,"current":entry.current})).collect::<Vec<_>>() }))
}

fn jump(s: &mut Session, params: &Value) -> Result<Value> {
    jump_on_path(s, params, None)
}

fn jump_on_path(s: &mut Session, params: &Value, branch: Option<u64>) -> Result<Value> {
    let id = params
        .get("state")
        .and_then(Value::as_u64)
        .ok_or_else(|| EngineError::BadParams { cmd: "history.jump".into(), msg: "state must be an unsigned state ID".into() })?;
    let active = s.active().ok_or(EngineError::NoDocument)?;
    let state = active.history.state_by_id(active.doc.clone(), id).ok_or_else(|| EngineError::Other("History state is no longer available".into()))?;
    if active.history.current_id() == id {
        return Ok(json!({"state":id,"changed":false}));
    }
    crate::jobs::run(
        s,
        "Loading history state",
        true,
        move |ctx| {
            ctx.check()?;
            let document = state.load_document().map_err(EngineError::Other)?;
            ctx.check()?;
            Ok(document)
        },
        move |session, document| {
            session.apply_history_destination(id, document, branch)?;
            Ok(json!({"state":id,"changed":true}))
        },
    )
}

/// Keyboard undo/redo use the same single-destination background restore as row jumps.
pub(crate) fn step(s: &mut Session, redo: bool) -> Result<Value> {
    if s.collaboration.room.is_some() && s.active().is_some_and(|st| s.collaboration.document_id == Some(st.doc.id)) {
        return Ok(json!(if redo { s.try_redo()? } else { s.try_undo()? }));
    }
    let id = s.active().and_then(|st| st.history.adjacent_id(redo)).ok_or_else(|| EngineError::Other("No adjacent history state".into()))?;
    let branch = s.active().map(|state| state.history.navigation_branch());
    let result = jump_on_path(s, &json!({"state": id}), branch)?;
    Ok(if result.get("job").is_some() { result } else { json!(true) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_doc::{ColorMode, Document, SampleType, Size};
    fn session() -> Session {
        let mut session = Session::new();
        session.add_document(Document::new("Original", Size::new(2, 2), ColorMode::Rgb, SampleType::U8), None);
        session
    }
    #[test]
    fn direct_jump_is_one_revision_and_preserves_alternative_paths() {
        let mut s = session();
        for name in ["Brush paint", "Erase", "New layer"] {
            s.edit(name, |doc, _| {
                doc.name = name.into();
                Ok(())
            })
            .unwrap();
        }
        let before = s.active().unwrap().revision;
        s.execute("history.jump", json!({"state":0})).unwrap();
        assert_eq!(s.active().unwrap().doc.name, "Original");
        assert_eq!(s.active().unwrap().revision, before + 1);
        s.edit("Move layer", |doc, _| {
            doc.name = "Moved".into();
            Ok(())
        })
        .unwrap();
        let entries = s.execute("history.inspect", json!({})).unwrap();
        assert_eq!(entries["states"].as_array().unwrap().len(), 5);
        s.execute("history.jump", json!({"state":3})).unwrap();
        assert_eq!(s.active().unwrap().doc.name, "New layer");
    }
    #[test]
    fn unlimited_defaults_and_bad_params_leave_document_and_history_unchanged() {
        let mut s = session();
        assert!(s.prefs().performance.unlimited_history);
        assert!(s.prefs().scratch_disks.use_available_space);
        assert_eq!(s.active().unwrap().history.max_states, 0);
        for params in [json!({}), json!({"state":-1}), json!({"state":"0"}), json!({"state":u64::MAX})] {
            assert!(s.execute("history.jump", params).is_err());
            assert_eq!(s.active().unwrap().doc.name, "Original");
            assert_eq!(s.active().unwrap().history.timeline().len(), 1);
        }
    }
    #[test]
    fn legacy_explicit_limits_still_select_bounded_mode() {
        let mut s = session();
        s.execute("prefs.set", json!({"values":{"performance.historyStates":5,"scratchDisks.budgetMb":32}})).unwrap();
        assert!(!s.prefs().performance.unlimited_history);
        assert!(!s.prefs().scratch_disks.use_available_space);
        assert_eq!(s.active().unwrap().history.max_states, 5);
    }
}

#[cfg(test)]
mod path_tests {
    use super::*;
    use photocraft_doc::{ColorMode, Document, SampleType, Size};
    use photocraft_ops::ArchivedDocument;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    #[test]
    fn undo_redo_follows_the_selected_branch_at_a_fork() {
        let mut s = Session::new();
        s.add_document(Document::new("Original", Size::new(2, 2), ColorMode::Rgb, SampleType::U8), None);
        for name in ["Paint", "Erase"] {
            s.edit(name, |doc, _| {
                doc.name = name.into();
                Ok(())
            })
            .unwrap();
        }
        s.execute("edit.undo", json!({})).unwrap();
        s.edit("Move", |doc, _| {
            doc.name = "Moved".into();
            Ok(())
        })
        .unwrap();
        s.execute("edit.undo", json!({})).unwrap();
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc.name, "Original");
        s.execute("edit.redo", json!({})).unwrap();
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc.name, "Moved");
    }
    #[derive(Debug)]
    struct Blocked {
        doc: Arc<Document>,
        release: Arc<AtomicBool>,
    }
    impl ArchivedDocument for Blocked {
        fn load(&self) -> std::result::Result<Arc<Document>, String> {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !self.release.load(Ordering::Relaxed) {
                if std::time::Instant::now() > deadline {
                    return Err("test read timed out".into());
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Ok(self.doc.clone())
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn cold_jump_returns_a_job_without_blocking_and_cancel_keeps_the_document() {
        for cancel in [false, true] {
            let mut s = Session::new();
            s.add_document(Document::new("Original", Size::new(2, 2), ColorMode::Rgb, SampleType::U8), None);
            let original = s.active().unwrap().doc.clone();
            s.edit("Paint", |doc, _| {
                doc.name = "Painted".into();
                Ok(())
            })
            .unwrap();
            let release = Arc::new(AtomicBool::new(false));
            s.active_mut().unwrap().history.replace_resident(&original, Arc::new(Blocked { doc: original.clone(), release: release.clone() }));
            let job = match s.start("history.jump", json!({"state":0})).unwrap() {
                crate::jobs::Started::Job(id) => id,
                _ => panic!("cold jump ran inline"),
            };
            assert_eq!(s.active().unwrap().doc.name, "Painted");
            if cancel {
                s.cancel_job(job);
            }
            release.store(true, Ordering::Relaxed);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while s.active_job().is_some() {
                s.poll_jobs();
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert_eq!(s.active().unwrap().doc.name, if cancel { "Painted" } else { "Original" });
        }
    }
}

#[cfg(test)]
mod upstream_tests {
    use super::*;

    /// A document with three steps after Open: entries Open, New Layer, Fill, Fill.
    fn session() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 20, "height": 10})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        s.execute("edit.fill", json!({"color": "#ff0000"})).unwrap();
        s.execute("edit.fill", json!({"color": "#00ff00"})).unwrap();
        s
    }

    fn entries(s: &Session) -> Vec<String> {
        s.active().unwrap().history.entries()
    }

    #[test]
    fn delete_current_state_steps_back_and_drops_it() {
        let mut s = session();
        let before = entries(&s);
        let prev_doc = s.active().unwrap().history.state(before.len() - 2).unwrap();
        let r = s.execute("history.deleteState", json!({})).unwrap();
        assert_eq!(r["states"], json!(before.len() - 1));
        assert_eq!(entries(&s), before[..before.len() - 1].to_vec());
        assert!(std::sync::Arc::ptr_eq(&s.active().unwrap().doc, &prev_doc), "the document is the state before");
        assert!(!s.active().unwrap().history.can_redo(), "the deleted state can't be redone");
        assert!(s.execute("edit.redo", json!({})).is_err());
    }

    #[test]
    fn delete_an_earlier_state_drops_it_and_every_later_one() {
        let mut s = session();
        let first = s.active().unwrap().history.state(0).unwrap();
        s.execute("history.deleteState", json!({"state": 1})).unwrap();
        assert_eq!(entries(&s), vec!["Open".to_string()]);
        assert!(std::sync::Arc::ptr_eq(&s.active().unwrap().doc, &first));
        assert!(!s.active().unwrap().history.can_redo());
        // Redo states after an undo go too.
        let mut s = session();
        s.execute("edit.undo", json!({})).unwrap();
        s.execute("history.deleteState", json!({})).unwrap();
        assert_eq!(entries(&s).len(), 2);
        assert!(!s.active().unwrap().history.can_redo());
    }

    #[test]
    fn delete_state_refuses_the_oldest_state_and_bad_params() {
        let mut s = Session::new();
        assert!(s.execute("history.deleteState", json!({})).is_err(), "no document");
        s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
        assert!(!s.is_enabled("history.deleteState"), "only the opening state");
        assert!(s.execute("history.deleteState", json!({})).is_err());
        let mut s = session();
        let before = entries(&s);
        for p in [json!({"state": 0}), json!({"state": -1}), json!({"state": 99}), json!({"state": "x"}), json!({"state": f64::MAX}), json!({"state": [1]})] {
            assert!(s.execute("history.deleteState", p.clone()).is_err(), "{p}");
            assert_eq!(entries(&s), before, "{p} changed nothing");
        }
    }

    #[test]
    fn new_document_from_a_state_copies_it_into_a_fresh_document() {
        let mut s = session();
        let src = s.active().unwrap().doc.clone();
        let label = entries(&s).last().cloned().unwrap();
        let r = s.execute("history.newDocument", json!({})).unwrap();
        assert_eq!(s.documents().len(), 2);
        assert_eq!(r["document"], json!(1));
        assert_eq!(r["name"], json!(label), "named after the state");
        let new = s.active().unwrap();
        assert_eq!(new.doc.size, src.size);
        assert_eq!(new.doc.layers.len(), src.layers.len());
        assert_ne!(new.doc.id, src.id);
        assert_eq!(new.history.entries().len(), 1, "a fresh history");
        // An earlier state, with a chosen name; the source document is untouched.
        s.set_active(0);
        let before = entries(&s);
        let open = s.active().unwrap().history.state(0).unwrap();
        s.execute("history.newDocument", json!({"state": 0, "name": "Start"})).unwrap();
        let new = s.active().unwrap();
        assert_eq!(new.doc.name, "Start");
        assert_eq!(new.doc.layers.len(), open.layers.len());
        s.set_active(0);
        assert_eq!(entries(&s), before);
    }

    #[test]
    fn new_document_refuses_bad_params() {
        let mut s = Session::new();
        assert!(s.execute("history.newDocument", json!({})).is_err(), "no document");
        let mut s = session();
        for p in [json!({"state": -1}), json!({"state": 99}), json!({"state": "x"}), json!({"state": f64::NAN.to_string()})] {
            assert!(s.execute("history.newDocument", p.clone()).is_err(), "{p}");
        }
        assert_eq!(s.documents().len(), 1);
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use std::sync::Arc;
    #[derive(Debug)]
    struct Broken;
    impl photocraft_ops::ArchivedDocument for Broken {
        fn load(&self) -> std::result::Result<Arc<photocraft_doc::Document>, String> {
            Err("corrupt history".into())
        }
    }
    #[test]
    fn delete_branch_preserves_unrelated_alternative_and_cold_failures_are_atomic() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width":8,"height":8})).unwrap();
        s.execute("layer.new.layer", json!({"name":"A"})).unwrap();
        s.execute("layer.new.layer", json!({"name":"B"})).unwrap();
        let branch_b = s.active().unwrap().history.current_id();
        s.undo();
        s.execute("layer.new.layer", json!({"name":"C"})).unwrap();
        let branch_c = s.active().unwrap().history.current_id();
        s.execute("history.deleteState", json!({})).unwrap();
        let st = s.active().unwrap();
        assert!(st.history.state_by_id(st.doc.clone(), branch_b).is_some());
        assert!(st.history.state_by_id(st.doc.clone(), branch_c).is_none());
        assert_eq!(st.history.adjacent_id(true), Some(branch_b), "an unrelated preserved branch remains redoable");
        s.redo();
        assert_eq!(s.active().unwrap().history.current_id(), branch_b);
        s.undo();
        s.execute("layer.new.layer", json!({"name":"D"})).unwrap();
        let original = s.active().unwrap().history.try_state(0).unwrap().unwrap();
        s.active_mut().unwrap().history.replace_resident(&original, Arc::new(Broken));
        let doc = s.active().unwrap().doc.clone();
        let stamp = s.active().unwrap().history.timeline_stamp();
        assert!(s.execute("history.deleteState", json!({"state":1})).is_err());
        assert!(s.execute("history.newDocument", json!({"state":0})).is_err());
        assert!(Arc::ptr_eq(&doc, &s.active().unwrap().doc));
        assert_eq!(s.active().unwrap().history.timeline_stamp(), stamp);
        assert_eq!(s.documents().len(), 1);
    }
}
