//! Captured artist context and safe resource resolution for shared document commands.
use crate::{EngineError, Result, Session};
use photocraft_collab::CommandEdit;
use photocraft_doc::{Document, LayerContent, LayerId};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandMode {
    Local,
    Shared,
    Resolved,
}

/// Files, fonts, plug-ins and private clipboard data resolve on the initiating machine.
/// A network peer never gets to ask the host to read a filesystem path or load a plug-in.
pub fn command_mode(id: &str, params: &Value) -> CommandMode {
    // Actions run locally; each recorded step uses normal authorized command dispatch,
    // producing its own canonical edit rather than forwarding an opaque action program.
    if id.starts_with("actions.") {
        return CommandMode::Local;
    }
    if id == "select.drop" {
        return CommandMode::Resolved;
    }
    let view_document = matches!(
        id,
        "view.newGuide"
            | "view.moveGuide"
            | "view.deleteGuide"
            | "view.clearGuides"
            | "view.newGuideLayout"
            | "view.newGuidesFromShape"
            | "view.clearCanvasGuides"
            | "view.clearSelectedArtboardGuides"
            | "view.clearSlices"
            | "view.lockSlices"
    );
    let palette_apply = id.ends_with(".apply")
        || id.ends_with(".place")
        || ((id.starts_with("gradient.presets.") || id.starts_with("pattern.presets."))
            && id.ends_with(".select")
            && params.get("applyToLayer").and_then(Value::as_bool) != Some(false));
    if id.starts_with("collab.")
        || id.starts_with("tools.")
        || id.starts_with("prefs.")
        || id.starts_with("brush.presets.")
        || id.starts_with("tool.presets.")
        || id.starts_with("cloneSource.")
        || id.starts_with("edit.preferences.")
        || id.starts_with("edit.purge.")
        || id.starts_with("edit.presets.")
        || (id == "edit.findAndReplaceText" && params.get("action").and_then(Value::as_str) == Some("find"))
        || (id == "edit.checkSpelling" && !matches!(params.get("action").and_then(Value::as_str), Some("change" | "changeAll")))
        || ((id.starts_with("preset.")
            || id.starts_with("pattern.")
            || id.starts_with("gradient.presets.")
            || id.starts_with("style.presets.")
            || id.starts_with("shape.presets."))
            && !palette_apply)
        || (id.starts_with("view.") && !view_document)
        || id.starts_with("window.")
        || id.starts_with("file.export.")
        || id.starts_with("jobs.")
        || id.starts_with("history.")
        || id == "layer.select"
        || id.starts_with("channel.target")
        || id == "channel.setVisible"
        || (id.starts_with("select.") && id != "select.saveSelection")
        || id.starts_with("selection.")
        || matches!(
            id,
            "file.new"
                | "file.open"
                | "file.openAs"
                | "file.close"
                | "file.closeAll"
                | "file.closeOthers"
                | "file.save"
                | "file.saveAs"
                | "file.saveACopy"
                | "file.print"
                | "file.printOneCopy"
                | "file.scripts.scriptEventsManager"
                | "file.automate.createDroplet"
                | "file.automate.batch"
                | "file.scripts.imageProcessor"
                | "file.generate.imageAssets"
                | "edit.copy"
                | "edit.copyMerged"
                | "edit.colorSettings"
                | "brush.defineFromSelection"
                | "brush.texturePattern"
                | "edit.defineBrushPreset"
                | "edit.defineCustomShape"
                | "edit.definePattern"
                | "edit.keyboardShortcuts"
                | "edit.menus"
                | "edit.toolbar"
                | "edit.presets.migratePresets"
                | "type.saveDefaultTypeStyles"
                | "layer.layerStyle.copy"
                | "layer.smartObjects.exportContents"
                | "layer.smartObjects.editContents"
        )
        || crate::commands::find(id).is_some_and(|spec| !spec.journal)
    {
        return CommandMode::Local;
    }
    if matches!(id, "edit.findAndReplaceText" | "edit.checkSpelling")
        || id.starts_with("text.")
        || id.starts_with("type.")
        || id.starts_with("plugin.")
        || matches!(
            id,
            "paint.cloneStamp"
                | "paint.healingBrush"
                | "paint.patternStamp"
                | "image.applyDataSet"
                | "image.applyImage"
                | "image.calculations"
                | "file.revert"
                | "edit.transform.again"
        )
        || id.contains(".presets.")
        || id.to_ascii_lowercase().contains("pattern")
        || params.get("contents").and_then(Value::as_str) == Some("history")
        || params.get("preset").is_some()
        || id.starts_with("edit.paste")
        || id.contains("historyBrush")
        || id.contains("fade")
        || id.starts_with("layer.smartObjects.")
        || id.starts_with("layer.layerStyle.paste")
        || id.starts_with("file.scripts.")
        || id.starts_with("file.automate.")
        || has_external_reference(params)
    {
        CommandMode::Resolved
    } else {
        CommandMode::Shared
    }
}

fn has_external_reference(value: &Value) -> bool {
    external_reference_at(value, 0)
}
fn external_reference_at(value: &Value, depth: usize) -> bool {
    if depth >= 128 {
        return true;
    }
    match value {
        Value::Object(map) => map.iter().any(|(key, value)| {
            (matches!(key.as_str(), "file" | "path" | "folder" | "paths" | "input") && (value.is_string() || value.is_array()))
                || (matches!(key.as_str(), "profile" | "workingSpace") && value.as_str().is_some_and(|s| s.contains('/') || s.contains('\\')))
                || external_reference_at(value, depth + 1)
        }),
        Value::Array(values) => values.iter().any(|value| external_reference_at(value, depth + 1)),
        _ => false,
    }
}
fn context_has_paths(value: &Value) -> bool {
    context_paths_at(value, 0)
}
fn context_paths_at(value: &Value, depth: usize) -> bool {
    if depth >= 128 {
        return true;
    }
    match value {
        Value::String(s) => s.contains('/') || s.contains('\\'),
        Value::Array(v) => v.iter().any(|value| context_paths_at(value, depth + 1)),
        Value::Object(v) => v.values().any(|value| context_paths_at(value, depth + 1)),
        _ => false,
    }
}
fn has_linked_assets(doc: &Document) -> bool {
    doc.walk()
        .into_iter()
        .any(|(_, _, layer)| matches!(&layer.content,LayerContent::Smart(object) if matches!(object.source,photocraft_doc::SmartSource::Linked{..})))
}
fn other(e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(e.to_string())
}
fn decode<T: DeserializeOwned>(context: &Value, key: &str) -> Result<T> {
    serde_json::from_value(context.get(key).cloned().ok_or_else(|| other(format!("missing artist context {key}")))?).map_err(other)
}

pub fn capture_context(s: &Session) -> Result<Value> {
    let state = s.active().ok_or(EngineError::NoDocument)?;
    let mut context_doc = Document::new("Artist context", state.doc.size, state.doc.mode, state.doc.depth);
    context_doc.selection = state.doc.selection.clone();
    context_doc.quick_mask = state.doc.quick_mask.clone();
    let mut bundle = photocraft_format::save_to_bytes(&context_doc, &Default::default()).map_err(other)?;
    let local_selection_resources = bundle.len() > 32 << 20;
    if local_selection_resources {
        context_doc.selection = None;
        context_doc.quick_mask = None;
        bundle = photocraft_format::save_to_bytes(&context_doc, &Default::default()).map_err(other)?;
    }
    Ok(
        json!({"bundle":bundle,"localSelectionResources":local_selection_resources,"activeLayer":state.active_layer,"selectedLayers":state.selected_layers,"layerVisibility":state.doc.walk().into_iter().map(|(_,_,layer)|(layer.id,layer.visible)).collect::<std::collections::BTreeMap<_,_>>(),
        "foreground":s.tools.foreground,"background":s.tools.background,"brush":s.tools.brush,"mixer":s.tools.mixer,
        "brushPresets":Vec::<photocraft_paint::BrushPreset>::new(),"gradient":s.presets.gradient,"gradients":Vec::<Value>::new(),
        "styles":Vec::<Value>::new(),"shapes":Vec::<Value>::new(),"pattern":s.presets.pattern,
        "clone":s.presets.clone,"toolPresets":Vec::<Value>::new(),
        "customShapes":Vec::<crate::edit_menu_cmds::CustomShape>::new(),"typeDefaults":s.type_defaults,
        "colorSettings":s.color.settings,"preferences":crate::prefs::Preferences::default().to_json(),
        "channelTarget":state.channel_view.target}),
    )
}
fn isolated(doc: &Document, context: &Value) -> Result<Session> {
    let bundle: Vec<u8> = decode(context, "bundle")?;
    if bundle.len() > 32 << 20 {
        return Err(other("artist context exceeds 32 MiB"));
    }
    let limits = photocraft_format::LoadOptions { max_manifest_bytes: 8 << 20, max_blob_bytes: 32 << 20, max_total_bytes: 64 << 20, preserve_ids: true };
    let context_doc = photocraft_format::load_from_bytes_with(&bundle, &limits).map_err(other)?;
    let mut doc = doc.clone();
    let visibility: std::collections::BTreeMap<LayerId, bool> = decode(context, "layerVisibility")?;
    for (id, visible) in visibility {
        if let Some(layer) = doc.layer_mut(id) {
            layer.visible = visible;
        }
    }
    doc.selection = context_doc.selection;
    doc.quick_mask = context_doc.quick_mask;
    let mut s = Session::new();
    s.add_document(doc, None);
    s.tools.foreground = decode(context, "foreground")?;
    s.tools.background = decode(context, "background")?;
    s.tools.brush = decode(context, "brush")?;
    s.tools.mixer = decode(context, "mixer")?;
    s.tools.presets = decode(context, "brushPresets")?;
    s.presets.gradient = decode(context, "gradient")?;
    s.presets.gradients = decode(context, "gradients")?;
    s.presets.styles = decode(context, "styles")?;
    s.presets.shapes = decode(context, "shapes")?;
    s.presets.pattern = decode(context, "pattern")?;
    s.presets.clone = decode(context, "clone")?;
    s.presets.tool_presets = decode(context, "toolPresets")?;
    s.patterns.items = context_doc.patterns;
    s.edit_state.custom_shapes = decode(context, "customShapes")?;
    s.type_defaults = decode(context, "typeDefaults")?;
    s.color.settings = decode(context, "colorSettings")?;
    let preferences: crate::prefs::Preferences = decode(context, "preferences")?;
    s.prefs.edit(|p| *p = preferences);
    let active: Option<LayerId> = decode(context, "activeLayer")?;
    let selected: Vec<LayerId> = decode(context, "selectedLayers")?;
    if let Some(state) = s.active_mut() {
        state.active_layer = active.filter(|id| state.doc.layer(*id).is_some());
        state.selected_layers = selected.into_iter().filter(|id| state.doc.layer(*id).is_some()).collect();
        let target = context.get("channelTarget").ok_or_else(|| other("missing channel target"))?;
        state.channel_view.target = match target.get("kind").and_then(Value::as_str) {
            Some("color") => crate::channel_cmds::ChannelTarget::Color(
                target.get("index").and_then(Value::as_u64).and_then(|v| usize::try_from(v).ok()).ok_or_else(|| other("invalid color channel"))?,
            ),
            Some("alpha") => crate::channel_cmds::ChannelTarget::Alpha(
                target.get("index").and_then(Value::as_u64).and_then(|v| usize::try_from(v).ok()).ok_or_else(|| other("invalid alpha channel"))?,
            ),
            Some("composite") => crate::channel_cmds::ChannelTarget::Composite,
            _ => return Err(other("invalid artist channel target")),
        };
    }
    Ok(s)
}

pub fn prepare_command(s: &Session, id: &str, params: &Value) -> Result<CommandEdit> {
    if id == "layer.smartObjects.saveContents" {
        let child = s.active().ok_or(EngineError::NoDocument)?;
        let link = s.smart_links.iter().find(|link| link.child == child.doc.id).copied().ok_or_else(|| other("not a smart-object contents document"))?;
        let parent = s.documents().iter().find(|state| state.doc.id == link.parent).ok_or(EngineError::NoDocument)?;
        let mut context = capture_context(s)?;
        context["targetDocument"] = json!(link.parent);
        context["activeLayer"] = json!(link.layer);
        context["selectedLayers"] = json!([link.layer]);
        let mut scratch = isolated(&parent.doc, &context)?;
        let child_index = scratch.add_document((*child.doc).clone(), None);
        scratch.smart_links.push(link);
        if !scratch.set_active(child_index) {
            return Err(other("could not activate smart-object contents"));
        }
        let result = scratch.execute(id, params.clone())?;
        let after = scratch.documents().iter().find(|state| state.doc.id == link.parent).ok_or(EngineError::NoDocument)?;
        context["resolvedResult"] = result;
        return Ok(CommandEdit {
            key: String::new(),
            id: id.into(),
            params: params.clone(),
            context,
            outcome: Some(photocraft_collab::delta::delta_between(&parent.doc, &after.doc).map_err(other)?),
            base_revision: s.collaboration.applied_revision,
            dependencies: vec![link.layer],
        });
    }
    let doc = &s.active().ok_or(EngineError::NoDocument)?.doc;
    let context = capture_context(s)?;
    let dependencies = s.active().map(|st| st.selected_layers()).unwrap_or_default();
    let mut edit = CommandEdit {
        key: String::new(),
        id: id.into(),
        params: params.clone(),
        context,
        outcome: None,
        base_revision: s.collaboration.applied_revision,
        dependencies,
    };
    let mut scratch = isolated(doc, &edit.context)?;
    // Client-only state resolves before any network request; it is never loaded from peer paths.
    scratch.tools = s.tools.clone();
    scratch.actions = s.actions.clone();
    scratch.authorize = s.authorize;
    scratch.presets = s.presets.clone();
    scratch.patterns = s.patterns.clone();
    scratch.prefs = s.prefs.clone();
    scratch.type_defaults = s.type_defaults.clone();
    scratch.quick_mask_options = s.quick_mask_options;
    scratch.clipboard = s.clipboard.clone();
    scratch.style_clipboard = s.style_clipboard.clone();
    scratch.edit_state = s.edit_state.clone();
    scratch.smart_links = s.smart_links.clone();
    scratch.journal = s.journal.clone();
    // Preserve source-document indices and each private history exactly while resolving locally.
    scratch.docs = s.docs.clone();
    scratch.active = s.active;
    let result = scratch.execute(id, params.clone())?;
    let after = &scratch.documents().iter().find(|state| state.doc.id == doc.id).ok_or(EngineError::NoDocument)?.doc;
    if command_mode(id, params) == CommandMode::Resolved
        || edit.context.get("localSelectionResources").and_then(Value::as_bool) == Some(true)
        || context_has_paths(edit.context.get("colorSettings").unwrap_or(&Value::Null))
        || has_linked_assets(doc)
    {
        edit.outcome = Some(photocraft_collab::delta::delta_between(doc, after).map_err(other)?);
    }
    let mut stack: Vec<_> = after.layers.iter().collect();
    let mut created = Vec::new();
    while let Some(layer) = stack.pop() {
        if doc.layer(layer.id).is_none() {
            created.push(layer.id);
        }
        if let Some(children) = layer.children() {
            stack.extend(children.iter());
        }
    }
    edit.context["createdLayerIds"] = json!(created);
    edit.context["resolvedResult"] = result;
    if matches!(id, "paint.cloneStamp" | "paint.healingBrush") {
        edit.context["cloneAfter"] = json!(scratch.presets.clone);
    }
    if id == "edit.findAndReplaceText" {
        edit.context["findCursorAfter"] = json!(scratch.edit_state.find_cursor);
    }
    if matches!(id, "paint.mixerBrush" | "gradient.presets.select" | "pattern.presets.select" | "gradient.presets.apply" | "pattern.presets.apply") {
        edit.context["artistEffects"] = capture_context(&scratch)?;
    }
    Ok(edit)
}

/// Update only private initiating-artist state after accepting their prepared operation.
pub fn apply_artist_effects(s: &mut Session, edit: &CommandEdit, before: &Document) -> Result<()> {
    if matches!(edit.id.as_str(), "paint.cloneStamp" | "paint.healingBrush") {
        s.presets.clone = decode(&edit.context, "cloneAfter")?;
    }
    if edit.id == "edit.findAndReplaceText" {
        s.edit_state.find_cursor = decode(&edit.context, "findCursorAfter")?;
        if let Some((layer, _, _)) = s.edit_state.find_cursor {
            s.select_layer(layer)?;
        }
    }
    if edit.id == "edit.cut" {
        let mut scratch = isolated(before, &edit.context)?;
        if let Some(state) = scratch.active_mut() {
            let doc = std::sync::Arc::make_mut(&mut state.doc);
            doc.selection = before.selection.clone();
            doc.quick_mask = before.quick_mask.clone();
        }
        scratch.execute("edit.copy", Value::Null)?;
        s.clipboard = scratch.clipboard;
    }
    if edit.id == "select.drop" {
        let offset = edit.context.get("resolvedResult").and_then(|result| result.get("offset")).and_then(Value::as_array);
        let coordinate = |index| offset.and_then(|values| values.get(index)).and_then(Value::as_i64).and_then(|value| i32::try_from(value).ok()).unwrap_or(0);
        let selection = before.selection.as_ref().map(|mask| photocraft_algo::resample::translate_surface(mask, coordinate(0), coordinate(1)));
        if let Some(state) = s.active_mut() {
            state.floating = None;
            std::sync::Arc::make_mut(&mut state.doc).selection = selection;
        }
    }
    let Some(context) = edit.context.get("artistEffects") else { return Ok(()) };
    let doc = &s.active().ok_or(EngineError::NoDocument)?.doc;
    let scratch = isolated(doc, context)?;

    if edit.id == "paint.mixerBrush" {
        s.tools.mixer = scratch.tools.mixer;
    }
    if edit.id.starts_with("gradient.presets.") {
        s.presets.gradient = scratch.presets.gradient;
        s.presets.rev = s.presets.rev.saturating_add(1);
    }
    if edit.id.starts_with("pattern.presets.") {
        s.presets.pattern = scratch.presets.pattern;
        s.presets.rev = s.presets.rev.saturating_add(1);
    }
    Ok(())
}

pub fn execute_resolved(doc: &Document, edit: &CommandEdit) -> Result<(Document, Value)> {
    let (mut after, mut result) = if let Some(outcome) = &edit.outcome {
        (photocraft_collab::delta::apply_delta_rebased(doc, outcome, false).map_err(other)?, edit.context.get("resolvedResult").cloned().unwrap_or(Value::Null))
    } else {
        if command_mode(&edit.id, &edit.params) != CommandMode::Shared
            || edit.context.get("localSelectionResources").and_then(Value::as_bool) == Some(true)
            || context_has_paths(edit.context.get("colorSettings").unwrap_or(&Value::Null))
            || has_linked_assets(doc)
        {
            return Err(other("resource command requires initiating artist's resolved outcome"));
        }
        let mut scratch = isolated(doc, &edit.context)?;
        let result = scratch.execute(&edit.id, edit.params.clone())?;
        ((*scratch.active().ok_or(EngineError::NoDocument)?.doc).clone(), result)
    };
    crate::collab::stabilize_created_layers(doc, &mut after, &mut result, edit)?;
    for (_, _, layer) in doc.walk() {
        if let Some(after_layer) = after.layer_mut(layer.id) {
            after_layer.visible = layer.visible;
        }
    }
    Ok((after, result))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn captured_context_replays_private_selection_and_color_at_every_depth() {
        for depth in [8, 16, 32] {
            let mut artist = Session::new();
            artist.execute("file.new", json!({"width":16,"height":16,"depth":depth,"fill":"transparent"})).unwrap();
            artist.execute("tools.setColors", json!({"foreground":"#ff0000"})).unwrap();
            artist.execute("select.rect", json!({"x":0,"y":0,"width":8,"height":16})).unwrap();
            let params = json!({"points":[[4,8,0.8],[12,8,0.8]],"size":6,"hardness":1,"smoothing":0});
            let edit = prepare_command(&artist, "paint.stroke", &params).unwrap();
            let before = (*artist.active().unwrap().doc).clone();
            let (received, _) = execute_resolved(&before, &edit).unwrap();
            artist.execute("paint.stroke", params).unwrap();
            let expected = &artist.active().unwrap().doc;
            let rect = photocraft_geom::Rect::new(0, 0, 16, 16);
            assert_eq!(photocraft_compose::render(expected, rect).px, photocraft_compose::render(&received, rect).px);
        }
    }
    #[test]
    fn a_peer_cannot_turn_a_resource_command_into_host_filesystem_access() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width":4,"height":4})).unwrap();
        let mut edit = prepare_command(&s, "paint.stroke", &json!({"points":[[1,1]],"size":1})).unwrap();
        edit.id = "layer.smartObjects.replaceContents".into();
        edit.params = json!({"path":"/private/does-not-exist"});
        assert!(execute_resolved(&s.active().unwrap().doc, &edit).is_err());
        edit.id = "image.adjustments.colorLookup".into();
        edit.params = json!({"file":"/private/does-not-exist"});
        assert!(execute_resolved(&s.active().unwrap().doc, &edit).is_err());
    }
    #[test]
    fn cut_keeps_clipboard_private_and_preserves_local_copy_behavior() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width":16,"height":16})).unwrap();
        s.execute("paint.stroke", json!({"points":[[4,4],[8,8]],"color":"#ff0000","size":4})).unwrap();
        s.execute("select.rect", json!({"x":0,"y":0,"width":10,"height":10})).unwrap();
        s.execute("edit.copy", Value::Null).unwrap();
        let expected = s.clipboard.clone().unwrap();
        let context_before = capture_context(&s).unwrap();
        s.clipboard = None;
        let context_after = capture_context(&s).unwrap();
        let before_bundle: Vec<u8> = decode(&context_before, "bundle").unwrap();
        let after_bundle: Vec<u8> = decode(&context_after, "bundle").unwrap();
        assert!(before_bundle.len().abs_diff(after_bundle.len()) < 16);
        let context_doc = photocraft_format::load_from_bytes(&before_bundle).unwrap();
        assert!(context_doc.layers.is_empty());
        let before = (*s.active().unwrap().doc).clone();
        let edit = prepare_command(&s, "edit.cut", &Value::Null).unwrap();
        s.execute("edit.cut", Value::Null).unwrap();
        s.clipboard = None;
        apply_artist_effects(&mut s, &edit, &before).unwrap();
        let clip = s.clipboard.as_ref().unwrap();
        assert_eq!(clip.bounds, expected.bounds);
        assert_eq!(clip.surface.read_region(clip.bounds), expected.surface.read_region(expected.bounds));
    }
    #[test]
    fn text_find_cursor_and_private_dictionary_actions_keep_their_semantics() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width":128,"height":64})).unwrap();
        s.execute("type.create", json!({"text":"cat cat","x":1,"y":1,"size":16})).unwrap();
        let find = json!({"find":"cat","action":"find"});
        assert_eq!(command_mode("edit.findAndReplaceText", &find), CommandMode::Local);
        s.execute("edit.findAndReplaceText", find).unwrap();
        let params = json!({"find":"cat","replace":"dog","action":"changeFind"});
        let before = (*s.active().unwrap().doc).clone();
        let edit = prepare_command(&s, "edit.findAndReplaceText", &params).unwrap();
        assert!(edit.outcome.is_some());
        let (after, _) = execute_resolved(&before, &edit).unwrap();
        s.execute("edit.findAndReplaceText", params).unwrap();
        let expected_cursor = s.edit_state.find_cursor;
        s.edit_state.find_cursor = None;
        apply_artist_effects(&mut s, &edit, &before).unwrap();
        assert_eq!(s.edit_state.find_cursor, expected_cursor);
        assert!(after.walk().iter().any(|(_, _, layer)| matches!(&layer.content, LayerContent::Text(text) if text.text=="dog cat")));
        assert_eq!(command_mode("edit.checkSpelling", &json!({"action":"addToDictionary"})), CommandMode::Local);
        assert_eq!(command_mode("edit.purge.clipboard", &Value::Null), CommandMode::Local);
        assert_eq!(command_mode("edit.presets.presetManager", &Value::Null), CommandMode::Local);
    }
    #[test]
    fn saved_selections_and_applied_library_presets_are_document_edits() {
        assert_ne!(command_mode("select.saveSelection", &Value::Null), CommandMode::Local);
        assert_ne!(command_mode("pattern.presets.apply", &Value::Null), CommandMode::Local);
        assert_ne!(command_mode("file.placeEmbedded", &json!({"path":"image.png"})), CommandMode::Local);
        assert_eq!(command_mode("select.rect", &Value::Null), CommandMode::Local);
    }
}
