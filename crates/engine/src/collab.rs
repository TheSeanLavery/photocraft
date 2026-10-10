//! Session collaboration state and deterministic sparse-tile stroke replay.
use crate::{EngineError, Session};
pub use photocraft_collab::DEFAULT_SIGNALING_URL;
use photocraft_collab::*;
use photocraft_doc::{Document, LayerId};
use photocraft_paint::StrokeRenderer;
use photocraft_raster::Surface;
use std::collections::BTreeMap;

struct ReplayStroke {
    start: StrokeStart,
    owner: String,
    renderer: StrokeRenderer,
    selection: Option<Surface>,
    pre: Surface,
    last: Option<photocraft_paint::StrokePoint>,
    before_document: Option<std::sync::Arc<Document>>,
    chunks: Vec<Vec<photocraft_paint::StrokePoint>>,
    ended: bool,
    visible: bool,
}
struct ReplayCommand {
    edit: CommandEdit,
    owner: String,
    visible: bool,
    created: Vec<photocraft_doc::Layer>,
}
#[derive(Clone, Copy)]
enum JournalEntry {
    Stroke(usize),
    Command(usize),
}
#[derive(Default)]
pub struct Collaboration {
    pub room: Option<RoomState>,
    pub document_id: Option<photocraft_doc::DocId>,
    pub outbox: Vec<ClientMessage>,
    pub canonical_outbox: Vec<HostMessage>,
    pub sequencer: HostSequencer,
    pub applied_revision: u64,
    pub next_nonce: u64,
    pub network_intents: Vec<NetworkIntent>,
    pub queued_presence: Option<Presence>,
    canonical_document: Option<Document>,
    pending_commands: Vec<CommandEdit>,
    baseline: Option<Document>,
    baseline_bytes: Option<Vec<u8>>,
    history_bytes: usize,
    journal: Vec<JournalEntry>,
    commands: Vec<ReplayCommand>,
    bases: BTreeMap<LayerId, Surface>,
    strokes: Vec<ReplayStroke>,
    redo: BTreeMap<String, Vec<String>>,
    undo_floor: BTreeMap<String, usize>,
}
#[derive(Clone, Debug)]
pub enum NetworkIntent {
    Create { code: String, password: Option<String>, server: String },
    Join { code: String, password: Option<String>, server: String },
    Leave,
}
impl Collaboration {
    /// This artist's completed strokes in canonical creation order, including undone entries.
    pub fn own_history(&self) -> Vec<(String, bool)> {
        let Some(room) = &self.room else { return Vec::new() };
        self.journal
            .iter()
            .enumerate()
            .filter(|(position, _)| *position >= self.undo_floor.get(&room.local_peer).copied().unwrap_or(0))
            .filter_map(|(_, entry)| match entry {
                JournalEntry::Stroke(i) => self.strokes.get(*i).filter(|s| s.owner == room.local_peer && s.ended).map(|s| (s.start.id.clone(), s.visible)),
                JournalEntry::Command(i) => self.commands.get(*i).filter(|s| s.owner == room.local_peer).map(|s| (s.edit.key.clone(), s.visible)),
            })
            .collect()
    }

    pub fn submit(&mut self, operation: Operation) -> crate::Result<ClientMessage> {
        validate(&operation).map_err(error)?;
        let room = self.room.as_ref().ok_or_else(|| EngineError::Other("not in a room".into()))?;
        self.next_nonce = self.next_nonce.checked_add(1).ok_or_else(|| EngineError::Other("operation nonce exhausted".into()))?;
        Ok(ClientMessage { version: PROTOCOL_VERSION, peer: room.local_peer.clone(), nonce: self.next_nonce, operation })
    }
    fn history_charge(&mut self, doc: &Document, owner: &str, operation: &Operation) -> crate::Result<usize> {
        if self.baseline_bytes.is_none() {
            let mut shared = doc.clone();
            shared.selection = None;
            shared.quick_mask = None;
            shared.id = photocraft_doc::DocId(0);
            let bytes = photocraft_format::save_to_bytes(&shared, &photocraft_format::SaveOptions::default()).map_err(|e| EngineError::Other(e.to_string()))?;
            self.history_bytes = bytes.len();
            self.baseline_bytes = Some(bytes);
            self.baseline = Some(doc.clone());
        }
        #[derive(serde::Serialize)]
        struct Command<'a> {
            kind: &'static str,
            edit: &'a CommandEdit,
            owner: &'a str,
            visible: bool,
        }
        #[derive(serde::Serialize)]
        struct Stroke<'a> {
            kind: &'static str,
            start: &'a StrokeStart,
            owner: &'a str,
            chunks: Vec<Vec<photocraft_paint::StrokePoint>>,
            ended: bool,
            visible: bool,
        }
        let charge = match operation {
            Operation::Command { edit } => serialized_size(&Command { kind: "command", edit, owner, visible: true }).map_err(error)?,
            Operation::StrokeBegin { stroke } => {
                serialized_size(&Stroke { kind: "stroke", start: stroke, owner, chunks: Vec::new(), ended: false, visible: true }).map_err(error)?
            }
            Operation::StrokeChunk { points, .. } => serialized_size(points).map_err(error)?.saturating_add(16),
            _ => 0,
        };
        if self.history_bytes.saturating_add(charge) > 480 * 1024 * 1024 {
            return Err(EngineError::Other("room checkpoint history reached 480 MiB; save the drawing and create a fresh room".into()));
        }
        Ok(charge)
    }
    fn apply(&mut self, doc: &mut Document, event: &mut HostMessage) -> crate::Result<()> {
        if event.revision <= self.applied_revision {
            return Ok(());
        }
        if event.revision != self.applied_revision.saturating_add(1) {
            return Err(EngineError::Other("missing authoritative operation; request reconnect replay".into()));
        }
        validate(&event.message.operation).map_err(error)?;
        let owner = &event.message.peer;
        let history_charge = self.history_charge(doc, owner, &event.message.operation)?;
        if let Operation::Note { note } = &event.message.operation {
            let room = self.room.as_ref().ok_or_else(|| EngineError::Other("not in a room".into()))?;
            if let Some(existing) = room.notes.get(&note.id) {
                if (existing.author != *owner && room.host != *owner) || note.author != existing.author {
                    return Err(EngineError::Other("only the note author or host can edit this note".into()));
                }
            } else if note.author != *owner {
                return Err(EngineError::Other("note author identity mismatch".into()));
            }
        }

        if self.baseline.is_none() {
            self.baseline = Some(doc.clone());
        }
        let before = doc.clone();
        let mut full_replay = false;
        let mut affected = None;
        match &event.message.operation {
            Operation::StrokeBegin { stroke } => {
                if self.strokes.len() >= 4096 || self.strokes.iter().any(|s| s.start.id == stroke.id) {
                    return Err(EngineError::Other("stroke journal full or duplicate stroke".into()));
                }
                let layer = doc.layer(stroke.layer).ok_or(EngineError::NoLayer(stroke.layer))?;
                if layer.locks.all || layer.locks.pixels {
                    return Err(EngineError::Other("target layer is locked".into()));
                }
                let surface = layer.surface().ok_or_else(|| EngineError::Other("target layer has no raster pixels".into()))?;
                self.bases.entry(stroke.layer).or_insert_with(|| surface.clone());
                let mut selection =
                    Surface::new(photocraft_color::PixelFormat::new(photocraft_color::ColorMode::Grayscale, photocraft_color::SampleType::F32, false));
                for run in &stroke.selection {
                    for (offset, coverage) in run.coverage.iter().enumerate() {
                        let x = run.x.saturating_add(i32::try_from(offset).unwrap_or(i32::MAX));
                        selection.write_pixel(x, run.y, &[*coverage]);
                    }
                }
                let mut authoritative_start = (**stroke).clone();
                authoritative_start.lock_transparency = layer.locks.transparency;
                self.strokes.push(ReplayStroke {
                    start: authoritative_start,
                    owner: owner.clone(),
                    renderer: StrokeRenderer::new(&stroke.brush, Some(surface.format()), stroke.zoom),
                    selection: (!stroke.selection.is_empty()).then_some(selection),
                    pre: surface.clone(),
                    last: None,
                    before_document: self.room.as_ref().filter(|room| room.local_peer == *owner).map(|_| std::sync::Arc::new(doc.clone())),
                    chunks: Vec::new(),
                    ended: false,
                    visible: true,
                });
                self.journal.push(JournalEntry::Stroke(self.strokes.len().saturating_sub(1)));
                self.redo.remove(owner);
            }
            Operation::StrokeChunk { id, points, .. } => {
                let s = self
                    .strokes
                    .iter_mut()
                    .find(|s| s.start.id == *id && s.owner == *owner && !s.ended)
                    .ok_or_else(|| EngineError::Other("unknown active stroke".into()))?;
                ChunkBudget::new(&s.start.brush).validate(s.last, points).map_err(error)?;
                let mut previous = s.last;
                for point in points {
                    if let Some(last) = previous
                        && (point.time < last.time || point.time - last.time > 1000.0 || (point.x - last.x).hypot(point.y - last.y) > 4096.0)
                    {
                        return Err(EngineError::Other("stroke sample gap exceeds collaboration budget".into()));
                    }
                    previous = Some(*point);
                }
                let mut bounds = s.renderer.bounds();
                let radius = f64::from(s.start.brush.size) * 2.0;
                for point in points {
                    bounds = bounds.union(&photocraft_geom::Rect::new(
                        (point.x - radius).floor() as i32,
                        (point.y - radius).floor() as i32,
                        (point.x + radius).ceil() as i32,
                        (point.y + radius).ceil() as i32,
                    ));
                }
                if i64::from(bounds.width()).saturating_mul(i64::from(bounds.height())) > 16_777_216 {
                    return Err(EngineError::Other("stroke exceeds collaboration rendering budget; release and start another stroke".into()));
                }
                s.last = previous;
                s.renderer.push(points);
                s.chunks.push(points.clone());
                affected = Some(s.start.layer);
            }
            Operation::StrokeEnd { id, .. } => {
                let s = self
                    .strokes
                    .iter_mut()
                    .find(|s| s.start.id == *id && s.owner == *owner && !s.ended)
                    .ok_or_else(|| EngineError::Other("unknown active stroke".into()))?;
                s.renderer.finish();
                s.ended = true;
                affected = Some(s.start.layer);
            }
            Operation::Command { edit } => {
                if self.journal.len() >= 4096 || self.commands.iter().any(|c| c.edit.key == edit.key) {
                    return Err(EngineError::Other("edit journal full or duplicate edit".into()));
                }
                let (after, result) = if let Some(delta) = &event.delta {
                    (photocraft_collab::delta::apply_delta(doc, delta).map_err(error)?, event.result.clone().unwrap_or(serde_json::Value::Null))
                } else {
                    crate::collab_resources::execute_resolved(doc, edit)?
                };
                let created = all_layers(&after).into_iter().filter(|layer| doc.layer(layer.id).is_none()).cloned().collect();
                if event.delta.is_none() {
                    event.delta = Some(photocraft_collab::delta::delta_between(doc, &after).map_err(error)?);
                }
                *doc = after;
                event.result = Some(result);
                self.commands.push(ReplayCommand { edit: (**edit).clone(), owner: owner.clone(), visible: true, created });
                self.journal.push(JournalEntry::Command(self.commands.len().saturating_sub(1)));
                self.redo.remove(owner);
            }
            Operation::PurgeHistory => {
                self.undo_floor.insert(owner.clone(), self.journal.len());
                self.redo.remove(owner);
            }
            Operation::Undo => {
                for (position, entry) in self.journal.iter().enumerate().rev() {
                    if position < self.undo_floor.get(owner).copied().unwrap_or(0) {
                        break;
                    }
                    let key = match entry {
                        JournalEntry::Stroke(i) => self.strokes.get_mut(*i).filter(|s| s.owner == *owner && s.ended && s.visible).map(|s| {
                            s.visible = false;
                            s.start.id.clone()
                        }),
                        JournalEntry::Command(i) => self.commands.get_mut(*i).filter(|s| s.owner == *owner && s.visible).map(|s| {
                            s.visible = false;
                            s.edit.key.clone()
                        }),
                    };
                    if let Some(key) = key {
                        self.redo.entry(owner.clone()).or_default().push(key);
                        full_replay = true;
                        break;
                    }
                }
            }
            Operation::Redo => {
                if let Some(key) = self.redo.get_mut(owner).and_then(Vec::pop) {
                    if let Some(s) = self.strokes.iter_mut().find(|s| s.owner == *owner && s.start.id == key) {
                        s.visible = true;
                        full_replay = true;
                    }
                    if let Some(c) = self.commands.iter_mut().find(|c| c.owner == *owner && c.edit.key == key) {
                        c.visible = true;
                        full_replay = true;
                    }
                }
            }
            _ => {}
        }
        if let Operation::StrokeChunk { id, .. } | Operation::StrokeEnd { id, .. } = &event.message.operation
            && let Some(position) = self
                .journal
                .iter()
                .position(|entry| matches!(entry,JournalEntry::Stroke(index) if self.strokes.get(*index).is_some_and(|stroke|stroke.start.id==*id)))
        {
            full_replay |= self
                .journal
                .iter()
                .skip(position.saturating_add(1))
                .any(|entry| matches!(entry,JournalEntry::Command(index) if self.commands.get(*index).is_some_and(|command|command.visible)));
        }
        if full_replay {
            if let Some(delta) = &event.delta {
                *doc = photocraft_collab::delta::apply_delta(doc, delta).map_err(error)?;
            } else {
                self.rebuild_document(doc)?;
            }
            affected = None;
        }
        if let Some(layer) = affected {
            let changed = match &event.message.operation {
                Operation::StrokeChunk { id, .. } | Operation::StrokeEnd { id, .. } => Some(id.as_str()),
                _ => None,
            };
            let fast = changed.and_then(|id| self.strokes.iter().find(|s| s.start.id == id)).is_some_and(|stroke| {
                self.strokes
                    .iter()
                    .filter(|other| other.start.layer == layer && other.visible && other.start.id != stroke.start.id)
                    .filter(|other| {
                        !other.ended
                            || self.strokes.iter().position(|s| s.start.id == other.start.id) > self.strokes.iter().position(|s| s.start.id == stroke.start.id)
                    })
                    .all(|other| other.renderer.bounds().intersect(&stroke.renderer.bounds()).is_empty())
            });
            if fast {
                if let Some(stroke) = self.strokes.iter_mut().find(|s| Some(s.start.id.as_str()) == changed) {
                    let target = doc.layer_mut(layer).and_then(|l| l.surface_mut()).ok_or(EngineError::NoLayer(layer))?;
                    stroke.renderer.composite(&stroke.pre, target, stroke.selection.as_ref(), stroke.start.lock_transparency, false);
                }
            } else {
                if self.commands.is_empty() {
                    self.rebuild(doc, layer)?;
                } else {
                    self.rebuild_document(doc)?;
                }
            }
        }
        if let Some(room) = self.room.as_mut() {
            match &event.message.operation {
                Operation::Presence { presence } => {
                    room.members.insert(owner.clone(), presence.clone());
                }
                Operation::Chat { text } => {
                    room.chat.push_back((owner.clone(), text.clone()));
                    if room.chat.len() > 512 {
                        room.chat.pop_front();
                    }
                }
                Operation::Note { note } => {
                    if room.notes.len() < 1024 || room.notes.contains_key(&note.id) {
                        room.notes.insert(note.id.clone(), note.clone());
                    }
                }
                Operation::RemoveNote { id } => {
                    if room.notes.get(id).is_some_and(|n| n.author == *owner) || room.host == *owner {
                        room.notes.remove(id);
                    }
                }
                Operation::Visibility { mode, layers } => {
                    if room.local_peer == *owner {
                        room.visibility_mode = *mode;
                    }
                    if *mode == VisibilityMode::Shared {
                        if room.visibility_mode == VisibilityMode::Shared {
                            room.layer_visibility.extend(layers.clone());
                        }
                        for (id, visible) in layers {
                            if let Some(layer) = doc.layer_mut(*id) {
                                layer.visible = *visible;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if matches!(event.message.operation, Operation::Command { .. } | Operation::Undo | Operation::Redo) && event.delta.is_none() {
            event.delta = Some(photocraft_collab::delta::delta_between(&before, doc).map_err(error)?);
        }
        if let Some(delta) = event.delta.as_mut() {
            delta.base_resources.clear();
        }
        if serialized_size(event).map_err(error)? > 511 * 1024 * 1024 {
            return Err(EngineError::Other("canonical edit exceeds the 512 MiB transfer budget".into()));
        }
        self.history_bytes = self.history_bytes.saturating_add(history_charge);
        self.applied_revision = event.revision;
        Ok(())
    }
    fn rebuild_document(&mut self, doc: &mut Document) -> crate::Result<()> {
        let mut rebuilt = self.baseline.clone().ok_or(EngineError::NoDocument)?;
        for entry in self.journal.clone() {
            match entry {
                JournalEntry::Stroke(index) => {
                    if let Some(stroke) = self.strokes.get_mut(index).filter(|s| s.visible) {
                        let target = rebuilt.layer_mut(stroke.start.layer).and_then(|l| l.surface_mut()).ok_or(EngineError::NoLayer(stroke.start.layer))?;
                        self.bases.entry(stroke.start.layer).or_insert_with(|| target.clone());
                        if stroke.pre.format() != target.format() {
                            stroke.renderer = StrokeRenderer::new(&stroke.start.brush, Some(target.format()), stroke.start.zoom);
                            for points in &stroke.chunks {
                                stroke.renderer.push(points);
                            }
                            if stroke.ended {
                                stroke.renderer.finish();
                            }
                        }
                        stroke.pre = target.clone();
                        stroke.renderer.composite(&stroke.pre, target, stroke.selection.as_ref(), stroke.start.lock_transparency, true);
                    }
                }
                JournalEntry::Command(index) => {
                    let needs_prototypes = self.commands.get(index).is_some_and(|command| {
                        command.created.is_empty()
                            && command.edit.context.get("createdLayerIds").and_then(serde_json::Value::as_array).is_some_and(|ids| !ids.is_empty())
                    });
                    let recovered = if needs_prototypes {
                        let command = self.commands.get(index).ok_or_else(|| EngineError::Other("missing checkpoint command".into()))?;
                        let (next, _) = crate::collab_resources::execute_resolved(&rebuilt, &command.edit)?;
                        let created = all_layers(&next).into_iter().filter(|layer| rebuilt.layer(layer.id).is_none()).cloned().collect();
                        if let Some(command) = self.commands.get_mut(index) {
                            command.created = created;
                        }
                        Some(next)
                    } else {
                        None
                    };
                    if let Some(command) = self.commands.get(index) {
                        if command.visible {
                            let mut next = match recovered {
                                Some(next) => next,
                                None => match crate::collab_resources::execute_resolved(&rebuilt, &command.edit) {
                                    Ok((next, _)) => next,
                                    // Removing an author's source ink can leave a previously
                                    // accepted peer transform with no pixels. Keep the operator
                                    // in history so redo applies it when its input returns.
                                    Err(EngineError::Other(message)) if command.edit.id.starts_with("edit.transform") && message == "nothing to transform" => {
                                        rebuilt.clone()
                                    }
                                    Err(error) => return Err(error),
                                },
                            };
                            let fresh: Vec<_> = all_layers(&next).into_iter().filter(|l| rebuilt.layer(l.id).is_none()).map(|l| l.id).collect();
                            for (fresh, original) in fresh.into_iter().zip(&command.created) {
                                if let Some(layer) = next.layer_mut(fresh) {
                                    layer.id = original.id;
                                }
                            }
                            rebuilt = next;
                        } else {
                            // Retain structural prerequisites used by another artist, while removing this author's pixels.
                            let created: std::collections::BTreeSet<_> = command.created.iter().map(|layer| layer.id).collect();
                            let needed: std::collections::BTreeSet<_> = self
                                .commands
                                .iter()
                                .filter(|other| other.visible && other.owner != command.owner)
                                .flat_map(|other| other.edit.dependencies.iter().copied())
                                .chain(self.strokes.iter().filter(|stroke| stroke.visible && stroke.owner != command.owner).map(|stroke| stroke.start.layer))
                                .collect();
                            for layer in &command.created {
                                if rebuilt.layer(layer.id).is_none()
                                    && let Some(shell) = dependency_shell(layer, &created, &needed, &rebuilt, false, 0)?
                                {
                                    let mut descendants = Vec::new();
                                    let mut stack = vec![&shell];
                                    while let Some(layer) = stack.pop() {
                                        if layer.id != shell.id {
                                            descendants.push(layer.id);
                                        }
                                        if let Some(children) = layer.children() {
                                            stack.extend(children.iter());
                                        }
                                    }
                                    for id in descendants {
                                        let _ = rebuilt.remove(id);
                                    }
                                    let above = command.edit.context.get("activeLayer").and_then(serde_json::Value::as_u64).map(LayerId);
                                    rebuilt.insert_above(above, shell);
                                }
                            }
                        }
                    }
                }
            }
        }
        rebuilt.selection = doc.selection.clone();
        rebuilt.quick_mask = doc.quick_mask.clone();
        *doc = rebuilt;
        Ok(())
    }
    fn rebuild(&mut self, doc: &mut Document, id: LayerId) -> crate::Result<()> {
        let mut target = self.bases.get(&id).cloned().ok_or(EngineError::NoLayer(id))?;
        for stroke in self.strokes.iter_mut().filter(|s| s.start.layer == id && s.visible) {
            let pre = target.clone();
            stroke.pre = pre.clone();
            stroke.renderer.composite(&pre, &mut target, stroke.selection.as_ref(), stroke.start.lock_transparency, true);
        }
        let surface = doc.layer_mut(id).and_then(|l| l.surface_mut()).ok_or(EngineError::NoLayer(id))?;
        *surface = target;
        Ok(())
    }
}
fn dependency_shell(
    layer: &photocraft_doc::Layer,
    created: &std::collections::BTreeSet<LayerId>,
    needed: &std::collections::BTreeSet<LayerId>,
    current: &Document,
    ancestor: bool,
    depth: usize,
) -> crate::Result<Option<photocraft_doc::Layer>> {
    if depth > 128 {
        return Err(EngineError::Other("dependent layer hierarchy exceeds 128 levels".into()));
    }
    if !created.contains(&layer.id) {
        return Ok(current.layer(layer.id).cloned().filter(|_| ancestor || needed.contains(&layer.id)));
    }
    let required = ancestor || needed.contains(&layer.id);
    let mut shell = layer.clone();
    match &mut shell.content {
        photocraft_doc::LayerContent::Group(group) => {
            let mut children = Vec::new();
            for child in &group.children {
                if let Some(child) = dependency_shell(child, created, needed, current, required, depth.saturating_add(1))? {
                    children.push(child);
                }
            }
            group.children = children;
            if !required && group.children.is_empty() {
                return Ok(None);
            }
        }
        content if !required => {
            let _ = content;
            return Ok(None);
        }
        photocraft_doc::LayerContent::Raster(surface) => {
            *surface = Surface::new(surface.format());
        }
        photocraft_doc::LayerContent::Text(text) => {
            text.text.clear();
            text.cache = None;
            text.psd_raw = None;
            text.runs.clear();
            text.paragraphs.clear();
        }
        photocraft_doc::LayerContent::Shape(shape) => {
            shape.path = Default::default();
            shape.cache = None;
            shape.psd_raw = None;
            shape.live = None;
        }
        photocraft_doc::LayerContent::Fill(fill) => {
            *fill = photocraft_doc::Fill::Solid(photocraft_color::Color::rgba(0.0, 0.0, 0.0, 0.0));
        }
        photocraft_doc::LayerContent::Adjustment(adjustment) => {
            *adjustment = photocraft_doc::Adjustment::BrightnessContrast { brightness: 0.0, contrast: 0.0, legacy: false };
        }
        photocraft_doc::LayerContent::Smart(smart) => {
            let blank = Document::new("Empty prerequisite", current.size, current.mode, current.depth);
            let bytes = photocraft_format::save_to_bytes(&blank, &photocraft_format::SaveOptions::default()).map_err(|e| EngineError::Other(e.to_string()))?;
            smart.source = photocraft_doc::SmartSource::Embedded { file_name: "empty.pcraft".into(), bytes: std::sync::Arc::new(bytes) };
            smart.cache = None;
            smart.psd_raw = None;
        }
    }
    if let Some(mask) = &mut shell.mask {
        mask.surface = Surface::with_default(mask.surface.format(), &vec![1.0; mask.surface.channels()]);
    }
    shell.vector_mask = None;
    shell.fill_cache = None;
    shell.psd_blocks.clear();
    Ok(Some(shell))
}

/// Assign artist-scoped identities to layers created by a canonical command.
pub fn stabilize_created_layers(before: &Document, after: &mut Document, result: &mut serde_json::Value, edit: &CommandEdit) -> crate::Result<()> {
    let requested: Vec<LayerId> = edit
        .context
        .get("createdLayerIds")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| EngineError::Other(e.to_string()))?
        .unwrap_or_default();
    let fresh: Vec<_> = all_layers(after).into_iter().filter(|layer| before.layer(layer.id).is_none()).map(|layer| layer.id).collect();
    if !requested.is_empty() && requested.len() != fresh.len() {
        return Err(EngineError::Other("created layer count changed during replay".into()));
    }
    let mapping: BTreeMap<_, _> = fresh.iter().zip(&requested).map(|(old, new)| (old.0, new.0)).collect();
    if requested.iter().any(|id| before.layer(*id).is_some()) {
        return Err(EngineError::Other("created layer identity collides with existing layer".into()));
    }
    for (old, new) in mapping.iter() {
        if let Some(layer) = after.layer_mut(LayerId(*old)) {
            layer.id = LayerId(*new);
        }
    }
    remap_json_ids(result, &mapping);
    Ok(())
}
fn remap_json_ids(value: &mut serde_json::Value, mapping: &BTreeMap<u64, u64>) {
    fn visit(value: &mut serde_json::Value, mapping: &BTreeMap<u64, u64>, identity: bool, depth: usize) {
        if depth > 128 {
            return;
        }
        match value {
            serde_json::Value::Number(number) if identity => {
                if let Some(replacement) = number.as_u64().and_then(|id| mapping.get(&id)) {
                    *value = serde_json::json!(replacement);
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    visit(value, mapping, identity, depth.saturating_add(1));
                }
            }
            serde_json::Value::Object(values) => {
                let layer = values.contains_key("content") && values.contains_key("locks");
                for (key, value) in values {
                    let identity = matches!(
                        key.as_str(),
                        "layer" | "layers" | "activeLayer" | "selectedLayers" | "createdLayerIds" | "layer_anchor" | "layerId" | "layer_id"
                    ) || (layer && key == "id");
                    visit(value, mapping, identity, depth.saturating_add(1));
                }
            }
            _ => {}
        }
    }
    visit(value, mapping, true, 0);
}

fn all_layers(doc: &Document) -> Vec<&photocraft_doc::Layer> {
    let mut out = Vec::new();
    let mut stack: Vec<_> = doc.layers.iter().collect();
    while let Some(layer) = stack.pop() {
        out.push(layer);
        if let Some(children) = layer.children() {
            stack.extend(children.iter());
        }
    }
    out
}
fn error(e: ProtocolError) -> EngineError {
    EngineError::Other(e.to_string())
}
impl Session {
    pub fn collaboration_submit(&mut self, operation: Operation) -> crate::Result<()> {
        if let Operation::Visibility { mode: VisibilityMode::Personal, layers } = &operation {
            validate(&operation).map_err(error)?;
            let room = self.collaboration.room.as_mut().ok_or_else(|| EngineError::Other("not in a room".into()))?;
            room.visibility_mode = VisibilityMode::Personal;
            room.layer_visibility.extend(layers.clone());
            if let Some(state) = self.active_mut() {
                let doc = std::sync::Arc::make_mut(&mut state.doc);
                for (id, visible) in layers {
                    if let Some(layer) = doc.layer_mut(*id) {
                        layer.visible = *visible;
                    }
                }
                state.revision = state.revision.saturating_add(1);
                state.last_damage = None;
            }
            return Ok(());
        }

        if let Operation::Visibility { mode: VisibilityMode::Shared, .. } = &operation {
            self.collaboration.room.as_mut().ok_or_else(|| EngineError::Other("not in a room".into()))?.visibility_mode = VisibilityMode::Shared;
        }

        if let Operation::Presence { presence } = operation {
            validate(&Operation::Presence { presence: presence.clone() }).map_err(error)?;
            let room = self.collaboration.room.as_mut().ok_or_else(|| EngineError::Other("not in a room".into()))?;
            room.members.insert(room.local_peer.clone(), presence.clone());
            self.collaboration.queued_presence = Some(presence);
            return Ok(());
        }

        let message = self.collaboration.submit(operation)?;
        let host = self.collaboration.room.as_ref().is_some_and(|r| r.host == r.local_peer);
        if host {
            self.collaboration_accept(&message.peer.clone(), message)?;
        } else {
            self.collaboration.outbox.push(message);
        }
        Ok(())
    }
    pub fn collaboration_checkpoint(&self) -> crate::Result<JournalCheckpoint> {
        let collab = &self.collaboration;
        let baseline = collab
            .baseline
            .as_ref()
            .or_else(|| self.docs.iter().find(|st| Some(st.doc.id) == collab.document_id).map(|st| &*st.doc))
            .ok_or(EngineError::NoDocument)?;
        let baseline = if let Some(bytes) = &collab.baseline_bytes {
            bytes.clone()
        } else {
            let mut shared = baseline.clone();
            shared.selection = None;
            shared.quick_mask = None;
            shared.id = photocraft_doc::DocId(0);
            photocraft_format::save_to_bytes(&shared, &photocraft_format::SaveOptions::default()).map_err(|e| EngineError::Other(e.to_string()))?
        };
        let entries = collab
            .journal
            .iter()
            .filter_map(|entry| match entry {
                JournalEntry::Stroke(index) => collab.strokes.get(*index).map(|stroke| ArchivedEdit::Stroke {
                    start: Box::new(stroke.start.clone()),
                    owner: stroke.owner.clone(),
                    chunks: stroke.chunks.clone(),
                    ended: stroke.ended,
                    visible: stroke.visible,
                }),
                JournalEntry::Command(index) => collab.commands.get(*index).map(|command| ArchivedEdit::Command {
                    edit: Box::new(command.edit.clone()),
                    owner: command.owner.clone(),
                    visible: command.visible,
                }),
            })
            .collect();
        Ok(JournalCheckpoint { baseline, revision: collab.applied_revision, entries, redo: collab.redo.clone(), undo_floor: collab.undo_floor.clone() })
    }
    pub fn collaboration_install_checkpoint(&mut self, checkpoint: JournalCheckpoint) -> crate::Result<()> {
        if checkpoint.entries.len() > 4096
            || checkpoint.undo_floor.len() > 4096
            || checkpoint.undo_floor.values().any(|floor| *floor > checkpoint.entries.len())
            || serialized_size(&checkpoint).map_err(error)? > 480 * 1024 * 1024
        {
            return Err(EngineError::Other("checkpoint exceeds history budget".into()));
        }
        let mut document = photocraft_format::load_from_bytes(&checkpoint.baseline).map_err(|e| EngineError::Other(e.to_string()))?;
        let id = self.collaboration.document_id.ok_or(EngineError::NoDocument)?;
        document.id = id;
        document.selection = None;
        document.quick_mask = None;
        let mut restored = Collaboration {
            room: self.collaboration.room.clone(),
            document_id: Some(id),
            baseline: Some(document.clone()),
            history_bytes: serialized_size(&checkpoint).map_err(error)?,
            baseline_bytes: Some(checkpoint.baseline.clone()),
            ..Default::default()
        };
        // Build the archive once. Normal live apply computes deltas and repaints after
        // every chunk; replaying that path here made late joins quadratic in history.
        let mut nonce = BTreeMap::<String, u64>::new();
        for archived in checkpoint.entries {
            match archived {
                ArchivedEdit::Command { edit, owner, visible } => {
                    validate(&Operation::Command { edit: edit.clone() }).map_err(error)?;
                    if owner.is_empty() || owner.len() > 128 || restored.commands.iter().any(|command| command.edit.key == edit.key) {
                        return Err(EngineError::Other("invalid checkpoint command identity".into()));
                    }
                    restored.commands.push(ReplayCommand { edit: *edit, owner, visible, created: Vec::new() });
                    restored.journal.push(JournalEntry::Command(restored.commands.len().saturating_sub(1)));
                }
                ArchivedEdit::Stroke { start, owner, chunks, ended, visible } => {
                    let stroke_id = start.id.clone();
                    if restored.strokes.iter().any(|stroke| stroke.start.id == stroke_id) {
                        return Err(EngineError::Other("duplicate checkpoint stroke".into()));
                    }
                    let mut accept = |operation| -> crate::Result<()> {
                        let next = nonce.entry(owner.clone()).or_default();
                        *next = next.saturating_add(1);
                        let mut candidate = restored.sequencer.candidate();
                        candidate.accept(&owner, ClientMessage { version: PROTOCOL_VERSION, peer: owner.clone(), nonce: *next, operation }).map_err(error)?;
                        restored.sequencer = candidate.candidate();
                        Ok(())
                    };
                    accept(Operation::StrokeBegin { stroke: start.clone() })?;
                    let mut renderer = StrokeRenderer::new(&start.brush, Some(document.pixel_format()), start.zoom);
                    let mut last = None;
                    for (sequence, points) in chunks.iter().enumerate() {
                        accept(Operation::StrokeChunk {
                            id: stroke_id.clone(),
                            sequence: u32::try_from(sequence).map_err(|e| EngineError::Other(e.to_string()))?,
                            points: points.clone(),
                        })?;
                        ChunkBudget::new(&start.brush).validate(last, points).map_err(error)?;
                        let radius = f64::from(start.brush.size) * 2.0;
                        let mut bounds = renderer.bounds();
                        for point in points {
                            if let Some(previous) = last {
                                let previous: photocraft_paint::StrokePoint = previous;
                                if point.time < previous.time
                                    || point.time - previous.time > 1000.0
                                    || (point.x - previous.x).hypot(point.y - previous.y) > 4096.0
                                {
                                    return Err(EngineError::Other("invalid checkpoint sample gap".into()));
                                }
                            }
                            bounds = bounds.union(&photocraft_geom::Rect::new(
                                (point.x - radius).floor() as i32,
                                (point.y - radius).floor() as i32,
                                (point.x + radius).ceil() as i32,
                                (point.y + radius).ceil() as i32,
                            ));
                            last = Some(*point);
                        }
                        if i64::from(bounds.width()).saturating_mul(i64::from(bounds.height())) > 16_777_216 {
                            return Err(EngineError::Other("checkpoint stroke exceeds rendering budget".into()));
                        }
                        renderer.push(points);
                    }
                    if ended {
                        accept(Operation::StrokeEnd { id: stroke_id, sequence: u32::try_from(chunks.len()).map_err(|e| EngineError::Other(e.to_string()))? })?;
                        renderer.finish();
                    }
                    let mut selection =
                        Surface::new(photocraft_color::PixelFormat::new(photocraft_color::ColorMode::Grayscale, photocraft_color::SampleType::F32, false));
                    for run in &start.selection {
                        for (offset, coverage) in run.coverage.iter().enumerate() {
                            selection.write_pixel(run.x.saturating_add(i32::try_from(offset).unwrap_or(i32::MAX)), run.y, &[*coverage]);
                        }
                    }
                    // Completed historical identities need no active sequence metadata.
                    // Keep metadata only while this owner has an unfinished stream.
                    if ended && !restored.strokes.iter().any(|stroke| stroke.owner == owner && !stroke.ended) {
                        restored.sequencer.forget_peer(&owner);
                    }
                    restored.strokes.push(ReplayStroke {
                        selection: (!start.selection.is_empty()).then_some(selection),
                        start: *start,
                        owner,
                        renderer,
                        pre: Surface::new(document.pixel_format()),
                        last,
                        before_document: None,
                        chunks,
                        ended,
                        visible,
                    });
                    restored.journal.push(JournalEntry::Stroke(restored.strokes.len().saturating_sub(1)));
                }
            }
        }
        restored.rebuild_document(&mut document)?;
        restored.redo = checkpoint.redo;
        restored.undo_floor = checkpoint.undo_floor;
        restored.applied_revision = checkpoint.revision;
        restored.sequencer.revision = checkpoint.revision;
        restored.next_nonce = checkpoint.revision;
        restored.canonical_document = Some(document.clone());
        let st = self.docs.iter_mut().find(|st| st.doc.id == id).ok_or(EngineError::NoDocument)?;
        st.active_layer = document.top_layer();
        st.selected_layers = st.active_layer.into_iter().collect();
        st.doc = std::sync::Arc::new(document);
        st.revision = st.revision.saturating_add(1);
        st.last_damage = None;
        self.collaboration = restored;
        Ok(())
    }
    pub fn collaboration_execute(&mut self, id: &str, params: serde_json::Value) -> crate::Result<serde_json::Value> {
        let before = self.docs.iter().find(|st| Some(st.doc.id) == self.collaboration.document_id).map(|st| st.doc.clone()).ok_or(EngineError::NoDocument)?;
        let mut edit = crate::collab_resources::prepare_command(self, id, &params)?;
        let peer = self.collaboration.room.as_ref().ok_or_else(|| EngineError::Other("not in a room".into()))?.local_peer.clone();
        edit.key = format!("{peer}:edit:{}", self.collaboration.next_nonce.saturating_add(1));
        let original: Vec<LayerId> = edit
            .context
            .get("createdLayerIds")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| EngineError::Other(e.to_string()))?
            .unwrap_or_default();
        let mut mapping = BTreeMap::new();
        for (index, id) in original.iter().enumerate() {
            let hash = blake3::hash(format!("{}:{}:{index}", peer, self.collaboration.next_nonce.saturating_add(1)).as_bytes());
            let mut raw = [0u8; 8];
            raw.copy_from_slice(hash.as_bytes().get(..8).ok_or_else(|| EngineError::Other("identity hash truncated".into()))?);
            let first = 1_000_000 + u64::from_le_bytes(raw) % 8_000_000;
            let document = self.docs.iter().find(|st| Some(st.doc.id) == self.collaboration.document_id).ok_or(EngineError::NoDocument)?;
            let candidate = (0..4096u64)
                .map(|offset| 1_000_000 + (first - 1_000_000 + offset) % 8_000_000)
                .find(|candidate| document.doc.layer(LayerId(*candidate)).is_none() && !mapping.values().any(|existing| existing == candidate))
                .ok_or_else(|| EngineError::Other("could not reserve a collaborative layer identity".into()))?;
            mapping.insert(id.0, candidate);
        }
        if let Some(ids) = edit.context.get_mut("createdLayerIds") {
            remap_json_ids(ids, &mapping);
        }
        if let Some(result) = edit.context.get_mut("resolvedResult") {
            remap_json_ids(result, &mapping);
        }
        if let Some(outcome) = edit.outcome.as_mut() {
            let mut manifest = serde_json::to_value(&outcome.manifest).map_err(|e| EngineError::Other(e.to_string()))?;
            remap_json_ids(&mut manifest, &mapping);
            outcome.manifest = serde_json::from_value(manifest).map_err(|e| EngineError::Other(e.to_string()))?;
        }
        let result = edit.context.get("resolvedResult").cloned().unwrap_or(serde_json::Value::Null);
        let is_host = self.collaboration.room.as_ref().is_some_and(|room| room.local_peer == room.host);
        if !is_host {
            let target = self.collaboration.document_id;
            let st = self.docs.iter_mut().find(|st| Some(st.doc.id) == target).ok_or(EngineError::NoDocument)?;
            if self.collaboration.canonical_document.is_none() {
                self.collaboration.canonical_document = Some((*st.doc).clone());
            }
            let (predicted, _) = crate::collab_resources::execute_resolved(&st.doc, &edit)?;
            st.doc = std::sync::Arc::new(predicted);
            st.revision = st.revision.saturating_add(1);
            if let Some(id) = result.get("layer").and_then(serde_json::Value::as_u64).map(LayerId).filter(|id| st.doc.layer(*id).is_some()) {
                st.active_layer = Some(id);
                st.selected_layers = vec![id];
            }
            self.collaboration.pending_commands.push(edit.clone());
        }
        self.collaboration_submit(Operation::Command { edit: Box::new(edit.clone()) })?;
        crate::collab_resources::apply_artist_effects(self, &edit, &before)?;
        if let Some(st) = self.docs.iter_mut().find(|st| Some(st.doc.id) == self.collaboration.document_id) {
            st.history.record(id, before, st.layer_target());
        }
        self.after_command(id, params, true);
        if id == "layer.smartObjects.saveContents"
            && let Some(st) = self.active_mut()
        {
            st.saved_revision = st.revision;
        }
        Ok(self.collaboration.canonical_outbox.last().and_then(|event| event.result.clone()).unwrap_or(result))
    }
    pub fn collaboration_accept(&mut self, identity: &str, message: ClientMessage) -> crate::Result<()> {
        // Validate rendering transaction before advancing the authoritative sequencer.
        let mut sequencer = self.collaboration.sequencer.candidate();
        if let Some(mut event) = sequencer.accept(identity, message).map_err(error)? {
            self.collaboration_receive_mut(&mut event)?;
            if let Some(last) = sequencer.log.back_mut() {
                *last = event.clone();
            }
            self.collaboration.sequencer.commit_candidate(sequencer);
            self.collaboration.canonical_outbox.push(event);
        }
        Ok(())
    }
    pub fn collaboration_receive(&mut self, event: &HostMessage) -> crate::Result<()> {
        self.collaboration_receive_mut(&mut event.clone())
    }
    fn collaboration_receive_mut(&mut self, event: &mut HostMessage) -> crate::Result<()> {
        if event.revision <= self.collaboration.applied_revision {
            return Ok(());
        }
        let mut collab = std::mem::take(&mut self.collaboration);
        let metadata = matches!(event.message.operation, Operation::Command { .. } | Operation::Undo | Operation::Redo).then(|| {
            (
                collab.strokes.len(),
                collab.commands.len(),
                collab.journal.len(),
                collab.strokes.iter().map(|s| s.visible).collect::<Vec<_>>(),
                collab.commands.iter().map(|c| c.visible).collect::<Vec<_>>(),
                collab.redo.clone(),
            )
        });
        let result = if let Some(st) = self.docs.iter_mut().find(|st| Some(st.doc.id) == collab.document_id) {
            let mut doc = collab.canonical_document.clone().unwrap_or_else(|| (*st.doc).clone());
            doc.selection = st.doc.selection.clone();
            doc.quick_mask = st.doc.quick_mask.clone();
            collab.apply(&mut doc, event).map(|()| {
                if let Operation::Command { edit } = &event.message.operation {
                    collab.pending_commands.retain(|pending| pending.key != edit.key);
                }
                collab.canonical_document = Some(doc.clone());
                for pending in &collab.pending_commands {
                    if let Ok((predicted, _)) = crate::collab_resources::execute_resolved(&doc, pending) {
                        doc = predicted;
                    }
                }
                if let Some(room) = collab.room.as_ref().filter(|room| room.visibility_mode == VisibilityMode::Personal) {
                    for layer in all_layers(&st.doc) {
                        if let Some(local) = doc.layer_mut(layer.id) {
                            local.visible = layer.visible;
                        }
                    }
                    for (id, visible) in &room.layer_visibility {
                        if let Some(layer) = doc.layer_mut(*id) {
                            layer.visible = *visible;
                        }
                    }
                }
                st.doc = std::sync::Arc::new(doc);
                if collab.room.as_ref().is_some_and(|room| room.local_peer == event.message.peer)
                    && let Some(id) = event
                        .result
                        .as_ref()
                        .and_then(|r| r.get("layer"))
                        .and_then(serde_json::Value::as_u64)
                        .map(LayerId)
                        .filter(|id| st.doc.layer(*id).is_some())
                {
                    st.active_layer = Some(id);
                    st.selected_layers = vec![id];
                }
                if st.active_layer.is_some_and(|id| st.doc.layer(id).is_none()) {
                    st.active_layer = st.doc.top_layer();
                }
                st.selected_layers.retain(|id| st.doc.layer(*id).is_some());
                st.revision = st.revision.saturating_add(1);
                if let Some(mut floating) = st.floating.take()
                    && let Ok(parts) = floating.parts.rebased(&st.doc)
                {
                    floating.parts = std::sync::Arc::new(parts);
                    floating.revision = st.revision;
                    st.floating = Some(floating);
                }
                st.last_damage = None;
            })
        } else {
            Err(EngineError::NoDocument)
        };
        if result.is_err()
            && let Some(metadata) = metadata
        {
            collab.strokes.truncate(metadata.0);
            collab.commands.truncate(metadata.1);
            collab.journal.truncate(metadata.2);
            for (stroke, visible) in collab.strokes.iter_mut().zip(metadata.3) {
                stroke.visible = visible;
            }
            for (command, visible) in collab.commands.iter_mut().zip(metadata.4) {
                command.visible = visible;
            }
            collab.redo = metadata.5;
        }
        let own_stroke_end = if result.is_ok() && collab.room.as_ref().is_some_and(|room| room.local_peer == event.message.peer) {
            if let Operation::StrokeEnd { id, .. } = &event.message.operation {
                collab.strokes.iter_mut().find(|stroke| stroke.start.id == *id).and_then(|stroke| {
                    let before = stroke.before_document.take()?;
                    let points: Vec<_> = stroke.chunks.iter().flatten().map(|point| serde_json::json!([point.x, point.y, point.pressure, point.tilt_x, point.tilt_y, point.rotation, point.time, point.wheel])).collect();
                    let params = serde_json::json!({"target":"pixels","brush":stroke.start.brush,"color":stroke.start.brush.color,"erase":stroke.start.brush.erase,"seed":stroke.start.brush.seed,"zoom":stroke.start.zoom,"points":points});
                    Some((before, params))
                })
            } else {
                None
            }
        } else {
            None
        };
        self.collaboration = collab;
        if let Some((before, params)) = own_stroke_end {
            if let Some(st) = self.docs.iter_mut().find(|st| Some(st.doc.id) == self.collaboration.document_id) {
                st.history.record("Brush stroke", before, photocraft_ops::LayerTarget { active: st.active_layer, selected: st.selected_layers.clone() });
            }
            self.after_command("paint.stroke", params, true);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_paint::{BrushSettings, StrokePoint};
    use serde_json::json;
    fn session(depth: u32) -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width":64,"height":64,"depth":depth,"background":"transparent"})).unwrap();
        s.execute("collab.room.create", json!({"peer":"host","code":"TEST01"})).unwrap();
        s
    }
    fn emit(s: &mut Session, peer: &str, op: Operation) {
        let nonce = s
            .collaboration
            .sequencer
            .log
            .iter()
            .filter(|m| m.message.peer == peer)
            .map(|m| m.message.nonce)
            .max()
            .unwrap_or(0)
            .max(s.collaboration.applied_revision)
            + 1;
        s.collaboration_accept(peer, ClientMessage { version: 1, peer: peer.into(), nonce, operation: op }).unwrap();
        if s.collaboration.room.as_ref().is_some_and(|room| room.local_peer == peer) {
            s.collaboration.next_nonce = s.collaboration.next_nonce.max(nonce);
        }
    }
    fn begin(s: &mut Session, peer: &str, id: &str, color: [f32; 4]) {
        let layer = s.active().unwrap().active_layer.unwrap();
        let mut brush = BrushSettings { size: 6.0, color, seed: 234, opacity: 0.6, pressure_opacity: true, noise: true, ..Default::default() };
        if id == "s" {
            let tip = photocraft_paint::GrayTile::from_fn(8, 8, |x, y| if (3.5 - x as f32).hypot(3.5 - y as f32) < 3.5 { 1.0 } else { 0.0 });
            brush.tip = photocraft_paint::TipShape::Sampled(tip.clone());
            brush.dual_brush.tip = photocraft_paint::TipShape::Sampled(tip);
            brush.dual_brush.enabled = true;
            brush.dual_brush.size = 5.0;
            brush.scattering.enabled = true;
            brush.scattering.count = 2;
            brush.scattering.scatter.jitter = 0.5;
            brush.shape_dynamics.enabled = true;
            brush.shape_dynamics.size.jitter = 0.4;
            brush.color_dynamics.enabled = true;
            brush.color_dynamics.hue_jitter = 0.2;
            brush.texture.enabled = true;
            brush.texture.pattern = photocraft_paint::Pattern::Tile(photocraft_paint::GrayTile::from_fn(8, 8, |x, y| if (x + y) % 2 == 0 { 0.6 } else { 1.0 }));
        }
        emit(
            s,
            peer,
            Operation::StrokeBegin {
                stroke: Box::new(StrokeStart { zoom: 1.0, id: id.into(), layer, brush, selection: Vec::new(), lock_transparency: false }),
            },
        );
    }
    fn chunk(s: &mut Session, peer: &str, id: &str, sequence: u32, points: Vec<StrokePoint>) {
        emit(s, peer, Operation::StrokeChunk { id: id.into(), sequence, points });
    }
    fn end(s: &mut Session, peer: &str, id: &str, sequence: u32) {
        emit(s, peer, Operation::StrokeEnd { id: id.into(), sequence });
    }
    fn pixels(s: &Session) -> Vec<f32> {
        let d = s.active().unwrap();
        d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap().read_region(photocraft_geom::Rect::new(0, 0, 64, 64))
    }
    #[test]
    fn chunks_replay_exactly_at_multiple_depths() {
        for depth in [8, 16, 32] {
            let mut a = session(depth);
            let mut b = session(depth);
            let points: Vec<_> = (0..20).map(|i| StrokePoint::new(10.0 + f64::from(i), 20.0, f32::from(i as u16) / 20.0)).collect();
            for s in [&mut a, &mut b] {
                begin(s, "host", "s", [0.8, 0.2, 0.3, 1.0]);
            }
            chunk(&mut a, "host", "s", 0, points.clone());
            end(&mut a, "host", "s", 1);
            for (i, part) in points.chunks(3).enumerate() {
                chunk(&mut b, "host", "s", i as u32, part.to_vec());
            }
            end(&mut b, "host", "s", 7);
            assert_eq!(pixels(&a), pixels(&b));
        }
    }
    #[test]
    fn own_undo_preserves_one_hundred_newer_peer_strokes() {
        let mut s = session(16);
        let mut oracle = session(16);
        begin(&mut s, "host", "mine", [1.0, 0.0, 0.0, 1.0]);
        chunk(&mut s, "host", "mine", 0, vec![StrokePoint::new(20.0, 20.0, 1.0)]);
        end(&mut s, "host", "mine", 1);
        for i in 0..100 {
            for target in [&mut s, &mut oracle] {
                let id = format!("peer-{i}");
                begin(target, "peer", &id, [0.0, 0.0, 1.0, 1.0]);
                chunk(target, "peer", &id, 0, vec![StrokePoint::new(if i == 0 { 20.0 } else { 45.0 + f64::from(i % 10) }, 20.0, 0.4)]);
                end(target, "peer", &id, 1);
            }
        }
        let before = pixels(&s);
        assert_ne!(before, pixels(&oracle));
        emit(&mut s, "host", Operation::Undo);
        assert_eq!(pixels(&s), pixels(&oracle));
        emit(&mut s, "host", Operation::Redo);
        assert_eq!(pixels(&s), before);
    }
    #[test]
    fn undo_while_drawing_refreshes_active_pre_stroke_pixels() {
        let mut s = session(16);
        let mut oracle = session(16);
        begin(&mut s, "host", "old", [1.0, 0.0, 0.0, 1.0]);
        chunk(&mut s, "host", "old", 0, vec![StrokePoint::new(20.0, 20.0, 1.0)]);
        end(&mut s, "host", "old", 1);
        begin(&mut s, "host", "active", [0.0, 1.0, 0.0, 1.0]);
        chunk(&mut s, "host", "active", 0, vec![StrokePoint::new(20.0, 20.0, 1.0)]);
        emit(&mut s, "host", Operation::Undo);
        chunk(&mut s, "host", "active", 1, vec![StrokePoint::new(22.0, 20.0, 1.0)]);
        end(&mut s, "host", "active", 2);
        begin(&mut oracle, "host", "active", [0.0, 1.0, 0.0, 1.0]);
        chunk(&mut oracle, "host", "active", 0, vec![StrokePoint::new(20.0, 20.0, 1.0), StrokePoint::new(22.0, 20.0, 1.0)]);
        end(&mut oracle, "host", "active", 1);
        assert_eq!(pixels(&s), pixels(&oracle));
        assert_eq!(s.collaboration.own_history(), vec![("old".into(), false), ("active".into(), true)]);
    }
    #[test]
    fn room_document_stays_pinned_when_local_documents_are_added() {
        let mut s = session(8);
        let pinned = s.collaboration.document_id;
        let added = s.add_document(
            photocraft_doc::Document::new("Local", photocraft_doc::Size::new(64, 64), photocraft_color::ColorMode::Rgb, photocraft_color::SampleType::U8),
            None,
        );
        assert!(s.set_active(added));
        let local = s.active().unwrap().doc.id;
        let layer = s.documents().iter().find(|st| Some(st.doc.id) == pinned).unwrap().active_layer.unwrap();
        emit(
            &mut s,
            "host",
            Operation::StrokeBegin {
                stroke: Box::new(StrokeStart {
                    zoom: 1.0,
                    id: "pin".into(),
                    layer,
                    brush: BrushSettings::default(),
                    selection: Vec::new(),
                    lock_transparency: false,
                }),
            },
        );
        chunk(&mut s, "host", "pin", 0, vec![StrokePoint::new(10.0, 10.0, 1.0)]);
        end(&mut s, "host", "pin", 1);
        assert_eq!(s.active().unwrap().doc.id, local);
        assert!(s.set_active(0));
        assert!(s.close(0).is_some());
        assert!(s.collaboration.room.is_none());
    }
    #[test]
    fn selections_are_private_and_bad_inputs_do_not_advance_host() {
        let mut s = session(8);
        let layer = s.active().unwrap().active_layer.unwrap();
        let start = StrokeStart {
            zoom: 1.0,
            id: "masked".into(),
            layer,
            brush: BrushSettings { size: 4.0, ..Default::default() },
            selection: vec![SelectionRun { x: 10, y: 10, coverage: vec![1.0] }],
            lock_transparency: false,
        };
        emit(&mut s, "host", Operation::StrokeBegin { stroke: Box::new(start) });
        chunk(&mut s, "host", "masked", 0, vec![StrokePoint::new(10.0, 10.0, 1.0)]);
        end(&mut s, "host", "masked", 1);
        let revision = s.collaboration.sequencer.revision;
        assert!(
            s.collaboration_accept(
                "peer",
                ClientMessage {
                    version: 1,
                    peer: "peer".into(),
                    nonce: 1,
                    operation: Operation::StrokeChunk { id: "masked".into(), sequence: 0, points: vec![StrokePoint::new(f64::NAN, 0.0, 1.0)] }
                }
            )
            .is_err()
        );
        assert_eq!(s.collaboration.sequencer.revision, revision);
        begin(&mut s, "peer", "free", [0.0, 1.0, 0.0, 1.0]);
        chunk(&mut s, "peer", "free", 0, vec![StrokePoint::new(40.0, 40.0, 1.0)]);
        end(&mut s, "peer", "free", 1);
        assert!(s.active().unwrap().doc.layer(layer).unwrap().surface().unwrap().rgba(40, 40)[1] > 0.0);
    }
    #[test]
    fn generic_layer_creation_undo_preserves_peer_strokes_and_redo() {
        for depth in [8, 16, 32] {
            let mut s = session(depth);
            let result = s.execute("layer.new.layer", json!({"name":"Shared ink"})).unwrap();
            let id = LayerId(result["layer"].as_u64().unwrap());
            assert_eq!(s.active().unwrap().active_layer, Some(id));
            begin(&mut s, "peer", "foreign", [0.0, 0.5, 1.0, 1.0]);
            chunk(&mut s, "peer", "foreign", 0, vec![StrokePoint::new(8.0, 8.0, 0.8)]);
            end(&mut s, "peer", "foreign", 1);
            let painted = pixels(&s);
            s.execute("edit.undo", json!({})).unwrap();
            assert!(s.active().unwrap().doc.layer(id).is_some());
            assert_eq!(pixels(&s), painted);
            assert_eq!(s.collaboration.own_history().len(), 1);
            assert!(!s.collaboration.own_history()[0].1);
            s.execute("edit.redo", json!({})).unwrap();
            assert_eq!(pixels(&s), painted);
        }
    }
    #[test]
    fn canonical_generic_delta_replays_on_guest_with_private_selection() {
        let mut host = session(16);
        let initial = (*host.active().unwrap().doc).clone();
        let mut guest = Session::new();
        guest.add_document(initial, None);
        guest.execute("collab.room.join", json!({"peer":"guest","code":"TEST01"})).unwrap();
        guest.collaboration.document_id = host.collaboration.document_id;
        guest.execute("select.rect", json!({"x":1,"y":1,"width":5,"height":5})).unwrap();
        let selection = guest.active().unwrap().doc.selection.clone();
        host.execute("layer.new.layer", json!({"name":"Author's layer"})).unwrap();
        let event = host.collaboration.canonical_outbox.last().unwrap().clone();
        let before = photocraft_collab::delta::DocumentBundle::capture(&guest.active().unwrap().doc).unwrap();
        assert_eq!(
            before.hash().unwrap(),
            event.delta.as_ref().unwrap().base_hash,
            "guest initial manifest: {:?} vs {:?}",
            before.manifest,
            event.delta.as_ref().unwrap().base_manifest
        );
        guest.collaboration_receive(&event).unwrap();
        assert_eq!(guest.active().unwrap().doc.layer_count(), host.active().unwrap().doc.layer_count());
        assert_eq!(guest.active().unwrap().doc.selection, selection);
        assert!(guest.collaboration.own_history().is_empty());
        host.execute("edit.undo", json!({})).unwrap();
        let undo = host.collaboration.canonical_outbox.last().unwrap();
        let before = photocraft_collab::delta::DocumentBundle::capture(&guest.active().unwrap().doc).unwrap();
        assert_eq!(
            before.hash().unwrap(),
            undo.delta.as_ref().unwrap().base_hash,
            "guest preundo manifest: {:?} vs {:?}",
            before.manifest,
            undo.delta.as_ref().unwrap().base_manifest
        );
        guest.collaboration_receive(undo).unwrap();
        assert_eq!(guest.active().unwrap().doc.layer_count(), host.active().unwrap().doc.layer_count());
    }
    #[test]
    fn floating_selection_drop_replicates_pixels_and_keeps_selections_private() {
        for depth in [8, 16, 32] {
            let mut host = session(depth);
            let mut guest = Session::new();
            guest.add_document((*host.active().unwrap().doc).clone(), None);
            guest.execute("collab.room.join", json!({"peer":"guest","code":"TEST01"})).unwrap();
            guest.execute("select.rect", json!({"x":50,"y":50,"width":3,"height":3})).unwrap();
            let private_selection = guest.active().unwrap().doc.selection.clone();
            host.execute("select.rect", json!({"x":2,"y":2,"width":4,"height":4})).unwrap();
            host.execute("edit.fill", json!({"color":"#ff0000"})).unwrap();
            let painted = pixels(&host);
            host.execute("select.float", json!({"dx":12,"dy":0})).unwrap();
            assert_eq!(pixels(&host), painted);
            host.execute("select.drop", json!({})).unwrap();
            assert_ne!(pixels(&host), painted);
            assert!(host.active().unwrap().floating.is_none());
            assert_eq!(host.active().unwrap().doc.selection.as_ref().unwrap().content_bounds().x0, 14);
            for event in &host.collaboration.canonical_outbox {
                guest.collaboration_receive(event).unwrap();
            }
            assert_eq!(pixels(&guest), pixels(&host));
            assert_eq!(guest.active().unwrap().doc.selection, private_selection);
            host.try_undo().unwrap();
            assert_eq!(pixels(&host), painted);
        }
    }
    #[test]
    fn floating_lift_stays_private_and_drop_preserves_intervening_peer_strokes() {
        for depth in [8, 16, 32] {
            let mut host = session(depth);
            host.execute("select.rect", json!({"x":2,"y":2,"width":8,"height":8})).unwrap();
            host.execute("edit.fill", json!({"color":"#ff0000"})).unwrap();
            let original = pixels(&host);
            let revision = host.collaboration.applied_revision;
            host.execute("select.float", json!({"dx":20,"dy":0})).unwrap();
            assert_eq!(pixels(&host), original);
            assert_eq!(host.collaboration.applied_revision, revision);
            begin(&mut host, "peer", "between-lift-drop", [0.0, 0.5, 1.0, 1.0]);
            chunk(&mut host, "peer", "between-lift-drop", 0, vec![StrokePoint::new(4.0, 4.0, 0.8), StrokePoint::new(40.0, 40.0, 0.8)]);
            end(&mut host, "peer", "between-lift-drop", 1);
            assert!(crate::float_cmds::floating(host.active().unwrap()).is_some());
            let before_drop = pixels(&host);
            let layer = host.active().unwrap().active_layer.unwrap();
            let peer_inside = host.active().unwrap().doc.layer(layer).unwrap().surface().unwrap().rgba(4, 4);
            let peer_outside = host.active().unwrap().doc.layer(layer).unwrap().surface().unwrap().rgba(40, 40);
            host.execute("select.drop", json!({})).unwrap();
            let surface = host.active().unwrap().doc.layer(layer).unwrap().surface().unwrap();
            assert_eq!(surface.rgba(4, 4), peer_inside);
            assert_eq!(surface.rgba(40, 40), peer_outside);
            assert!(surface.rgba(24, 4)[0] > 0.9);
            host.try_undo().unwrap();
            assert_eq!(pixels(&host), before_drop);
        }
    }
    #[test]
    fn room_actions_record_streamed_brush_inputs_without_painting_twice() {
        let mut host = session(16);
        host.execute("actions.record", json!({"name":"Pressure stroke"})).unwrap();
        begin(&mut host, "host", "recorded-stream", [0.2, 0.5, 0.9, 1.0]);
        chunk(&mut host, "host", "recorded-stream", 0, vec![StrokePoint::new(5.0, 5.0, 0.25), StrokePoint::new(15.0, 5.0, 0.75)]);
        end(&mut host, "host", "recorded-stream", 1);
        host.execute("actions.stop", json!({})).unwrap();
        let action = host.execute("actions.get", json!({"action":"Pressure stroke"})).unwrap();
        let steps = action["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0][0], "paint.stroke");
        assert_eq!(steps[0][1]["points"][0][2], 0.25);
        assert_eq!(steps[0][1]["points"][1][2], 0.75);
        assert_eq!(host.collaboration.own_history().len(), 1);
        assert!(
            !host.collaboration.canonical_outbox.iter().any(|event| matches!(&event.message.operation, Operation::Command{edit} if edit.id=="paint.stroke"))
        );
    }
    #[test]
    fn room_actions_play_dispatches_each_step_and_preserves_nested_authorization() {
        let mut host = session(8);
        host.execute("actions.record", json!({"name":"Shared edits"})).unwrap();
        host.execute("layer.new.layer", json!({"name":"Recorded"})).unwrap();
        host.execute("select.rect", json!({"x":2,"y":2,"width":5,"height":5})).unwrap();
        host.execute("edit.fill", json!({"color":"#44cc88"})).unwrap();
        host.execute("actions.stop", json!({})).unwrap();
        let count = host.active().unwrap().doc.layer_count();
        let revision = host.collaboration.applied_revision;
        let result = host.execute("actions.play", json!({"action":"Shared edits"})).unwrap();
        assert!(result.get("failed").is_none());
        assert_eq!(host.active().unwrap().doc.layer_count(), count + 1);
        assert_eq!(host.collaboration.applied_revision, revision + 2);
        fn deny_layer(id: &str, _: &serde_json::Value) -> crate::Result<()> {
            if id == "layer.new.layer" {
                return Err(EngineError::Other("test denies nested layer mutation".into()));
            }
            Ok(())
        }
        host.authorize = Some(deny_layer);
        let revision = host.collaboration.applied_revision;
        let result = host.execute("actions.play", json!({"action":"Shared edits"})).unwrap();
        assert!(result.get("failed").is_some());
        assert_eq!(host.collaboration.applied_revision, revision);
        assert!(host.execute("edit.fill", json!([1, 2])).is_err());
        assert_eq!(host.collaboration.applied_revision, revision);
    }
    #[test]
    fn checkpoint_accepts_more_than_64_historical_authors_and_keeps_active_sequences() {
        let mut host = session(8);
        begin(&mut host, "active", "unfinished", [1.0, 0.0, 0.0, 1.0]);
        chunk(&mut host, "active", "unfinished", 0, vec![StrokePoint::new(4.0, 4.0, 0.6)]);
        for n in 0..65 {
            let peer = format!("historical-{n}");
            let id = format!("ended-{n}");
            begin(&mut host, &peer, &id, [0.0, 0.5, 1.0, 1.0]);
            chunk(&mut host, &peer, &id, 0, vec![StrokePoint::new(f64::from(n % 40), 15.0, 0.4)]);
            end(&mut host, &peer, &id, 1);
            host.collaboration.sequencer.forget_peer(&peer);
        }
        let mut checkpoint = host.collaboration_checkpoint().unwrap();
        for n in 0..65 {
            checkpoint.undo_floor.insert(format!("historical-{n}"), n);
        }
        let expected = pixels(&host);
        let mut restored = session(8);
        restored.collaboration.document_id = restored.active().map(|state| state.doc.id);
        restored.collaboration_install_checkpoint(checkpoint).unwrap();
        assert_eq!(pixels(&restored), expected);
        chunk(&mut restored, "active", "unfinished", 1, vec![StrokePoint::new(7.0, 4.0, 0.8)]);
        end(&mut restored, "active", "unfinished", 2);
        assert!(restored.collaboration.strokes.iter().find(|stroke| stroke.start.id == "unfinished").unwrap().ended);
        let mut duplicate = restored.collaboration_checkpoint().unwrap();
        duplicate.entries.push(duplicate.entries.last().unwrap().clone());
        assert!(restored.collaboration_install_checkpoint(duplicate).is_err());
    }
    #[test]
    fn bulk_checkpoint_preserves_mixed_history_and_hidden_creation_dependencies() {
        let mut host = session(16);
        host.execute("layer.new.layer", json!({"name":"Shared prerequisite"})).unwrap();
        for n in 0..100 {
            let id = format!("checkpoint-peer-{n}");
            begin(&mut host, "peer", &id, [0.0, 0.5, 1.0, 1.0]);
            chunk(&mut host, "peer", &id, 0, vec![StrokePoint::new(f64::from(n % 40), f64::from(n % 30), 0.4)]);
            chunk(&mut host, "peer", &id, 1, vec![StrokePoint::new(f64::from(n % 40) + 1.0, f64::from(n % 30), 0.8)]);
            end(&mut host, "peer", &id, 2);
        }
        host.execute("edit.undo", json!({})).unwrap();
        let expected = pixels(&host);
        let checkpoint = host.collaboration_checkpoint().unwrap();
        let mut restored = session(16);
        restored.collaboration.document_id = restored.active().map(|s| s.doc.id);
        restored.collaboration_install_checkpoint(checkpoint).unwrap();
        assert_eq!(pixels(&restored), expected);
        assert_eq!(restored.collaboration.strokes.len(), 100);
        restored.execute("edit.redo", json!({})).unwrap();
        assert_eq!(pixels(&restored), expected);
        restored.execute("edit.undo", json!({})).unwrap();
        assert_eq!(pixels(&restored), expected);
    }
    #[test]
    fn checkpoint_retains_author_undo_and_inflight_chunk_sequence() {
        let mut host = session(8);
        host.execute("layer.new.layer", json!({"name":"Collaborative"})).unwrap();
        begin(&mut host, "peer", "active-peer", [1.0, 0.0, 0.0, 1.0]);
        chunk(&mut host, "peer", "active-peer", 0, vec![StrokePoint::new(5.0, 5.0, 0.5)]);
        let checkpoint = host.collaboration_checkpoint().unwrap();
        let mut restored = session(8);
        restored.collaboration.document_id = restored.active().map(|s| s.doc.id);
        restored.collaboration_install_checkpoint(checkpoint).unwrap();
        assert_eq!(pixels(&restored), pixels(&host));
        assert_eq!(restored.collaboration.own_history(), host.collaboration.own_history());
        chunk(&mut restored, "peer", "active-peer", 1, vec![StrokePoint::new(8.0, 8.0, 0.6)]);
        end(&mut restored, "peer", "active-peer", 2);
        restored.execute("edit.undo", json!({})).unwrap();
        assert!(restored.active().unwrap().doc.layer_count() >= 2);
        assert!(!restored.collaboration.own_history()[0].1);
    }
    #[test]
    fn layer_identity_remapping_never_changes_brush_or_coordinates() {
        let mut value = json!({"createdLayerIds":[5],"layer":5,"brush":{"size":5,"seed":5},"points":[[5,5,5]],"document":{"id":5,"layers":[{"id":5,"locks":{},"content":{}}]}});
        remap_json_ids(&mut value, &BTreeMap::from([(5, 10005)]));
        assert_eq!(value["layer"], 10005);
        assert_eq!(value["createdLayerIds"], json!([10005]));
        assert_eq!(value["brush"]["size"], 5);
        assert_eq!(value["points"], json!([[5, 5, 5]]));
        assert_eq!(value["document"]["id"], 5);
        assert_eq!(value["document"]["layers"][0]["id"], 10005);
    }
    #[test]
    fn author_undo_reexecutes_peer_filters_and_transforms_at_every_depth() {
        for depth in [8, 16, 32] {
            let mut host = session(depth);
            let initial = (*host.active().unwrap().doc).clone();
            let mut oracle = Session::new();
            oracle.add_document(initial, None);
            begin(&mut host, "host", "own-ink", [1.0, 0.1, 0.3, 1.0]);
            chunk(&mut host, "host", "own-ink", 0, vec![StrokePoint::new(12.0, 10.0, 0.7), StrokePoint::new(20.0, 12.0, 0.8)]);
            end(&mut host, "host", "own-ink", 1);
            for (index, (id, params)) in
                [("image.adjustments.invert", json!({})), ("edit.transform", json!({"matrix":[1,0,0,1,3,4],"interpolation":"nearest"}))].into_iter().enumerate()
            {
                let mut edit = crate::collab_resources::prepare_command(&host, id, &params).unwrap();
                edit.key = format!("peer-operation-{index}");
                emit(&mut host, "peer", Operation::Command { edit: Box::new(edit) });
                match oracle.execute(id, params) {
                    Ok(_) => {}
                    Err(EngineError::Other(message)) if id == "edit.transform" && message == "nothing to transform" => {}
                    Err(error) => panic!("unexpected oracle failure: {error}"),
                }
            }
            host.execute("edit.undo", json!({})).unwrap();
            assert!(pixels(&host) == pixels(&oracle), "peer filters/transform changed after ownundo at depth {depth}");
            assert_eq!(host.collaboration.own_history(), vec![("own-ink".into(), false)]);
            let undone = pixels(&host);
            host.execute("edit.redo", json!({})).unwrap();
            assert_ne!(pixels(&host), undone);
        }
    }
    #[test]
    fn checkpoint_budget_rejects_before_document_or_authority_changes() {
        let mut host = session(8);
        let doc = (*host.active().unwrap().doc).clone();
        host.collaboration.history_charge(&doc, "host", &Operation::Undo).unwrap();
        host.collaboration.history_bytes = 480 * 1024 * 1024 - 1;
        let count = doc.layer_count();
        assert!(host.execute("layer.new.layer", json!({"name":"Too much history"})).is_err());
        assert_eq!(host.collaboration.applied_revision, 0);
        assert_eq!(host.collaboration.sequencer.revision, 0);
        assert_eq!(host.active().unwrap().doc.layer_count(), count);
        assert!(host.collaboration.own_history().is_empty());
        assert!(host.collaboration.canonical_outbox.is_empty());
    }
    #[test]
    fn dependent_group_shell_preserves_existing_peer_children_and_blanks_new_art() {
        for depth in [8, 16, 32] {
            let current = (*session(depth).active().unwrap().doc).clone();
            let existing = current.layers.first().unwrap().clone();
            let mut own = photocraft_doc::Layer::raster("Owned", current.pixel_format());
            own.surface_mut().unwrap().write_pixel(2, 2, &[1.0, 0.0, 0.0, 1.0]);
            let group = photocraft_doc::Layer::group("Dependent container", vec![existing.clone(), own.clone()]);
            let created = std::collections::BTreeSet::from([group.id, own.id]);
            let needed = std::collections::BTreeSet::from([group.id]);
            let shell = dependency_shell(&group, &created, &needed, &current, false, 0).unwrap().unwrap();
            let children = shell.children().unwrap();
            assert_eq!(children.len(), 2);
            assert_eq!(children[0], existing);
            assert_eq!(children[1].id, own.id);
            assert_eq!(children[1].surface().unwrap().tile_count(), 0);
        }
    }
    #[test]
    fn author_purge_keeps_canvas_and_peer_undo_history_and_survives_checkpoint() {
        let mut host = session(8);
        begin(&mut host, "host", "mine", [1.0, 0.0, 0.0, 1.0]);
        chunk(&mut host, "host", "mine", 0, vec![StrokePoint::new(5.0, 5.0, 0.8)]);
        end(&mut host, "host", "mine", 1);
        begin(&mut host, "peer", "theirs", [0.0, 0.5, 1.0, 1.0]);
        chunk(&mut host, "peer", "theirs", 0, vec![StrokePoint::new(15.0, 15.0, 0.8)]);
        end(&mut host, "peer", "theirs", 1);
        let image = pixels(&host);
        host.execute("edit.purge.undo", json!({})).unwrap();
        assert!(host.collaboration.own_history().is_empty());
        assert_eq!(pixels(&host), image);
        assert!(host.undo());
        assert_eq!(pixels(&host), image);
        let checkpoint = host.collaboration_checkpoint().unwrap();
        let mut restored = session(8);
        restored.collaboration_install_checkpoint(checkpoint).unwrap();
        assert!(restored.collaboration.own_history().is_empty());
        assert_eq!(pixels(&restored), image);
        emit(&mut restored, "peer", Operation::Undo);
        assert_ne!(pixels(&restored), image);
    }
}

#[cfg(test)]
mod zoom_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn zoom_smoothing_matches_engine_brush_at_multiple_depths() {
        for depth in [8, 16, 32] {
            let mut collaborative = Session::new();
            let mut oracle = Session::new();
            for s in [&mut collaborative, &mut oracle] {
                s.execute("file.new", json!({"width":64,"height":64,"depth":depth,"background":"transparent"})).unwrap();
            }
            let params = json!({"seed":924,"zoom":0.4,"size":6,"smoothing":0.75,"points":[[10,10,0.3],[20,20,0.7],[32,14,0.8],[40,30,1.0]]});
            oracle.execute("paint.stroke", params.clone()).unwrap();
            collaborative.execute("collab.room.create", json!({"peer":"host","code":"TEST01"})).unwrap();
            collaborative.execute("collab.stroke.begin", json!({"id":"zoom","command":"paint.stroke","params":params})).unwrap();
            let points = vec![
                photocraft_paint::StrokePoint::new(10.0, 10.0, 0.3),
                photocraft_paint::StrokePoint::new(20.0, 20.0, 0.7),
                photocraft_paint::StrokePoint::new(32.0, 14.0, 0.8),
                photocraft_paint::StrokePoint::new(40.0, 30.0, 1.0),
            ];
            collaborative.collaboration_submit(Operation::StrokeChunk { id: "zoom".into(), sequence: 0, points }).unwrap();
            collaborative.collaboration_submit(Operation::StrokeEnd { id: "zoom".into(), sequence: 1 }).unwrap();
            let read = |s: &Session| {
                let d = s.active().unwrap();
                d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap().read_region(photocraft_geom::Rect::new(0, 0, 64, 64))
            };
            assert_eq!(read(&collaborative), read(&oracle));
        }
    }
}
