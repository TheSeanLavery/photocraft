//! Session collaboration state and deterministic sparse-tile stroke replay.
use crate::{EngineError, Session};
#[path = "collab_restore.rs"]
mod restore;
pub use photocraft_collab::DEFAULT_SIGNALING_URL;
use photocraft_collab::*;
pub use photocraft_collab::{Profile, RoomState, VisibilityMode, invite};
use photocraft_doc::{Document, LayerId};
use photocraft_geom::Rect;
use photocraft_paint::{StrokeRenderer, render::COV_TILE};
use photocraft_raster::Surface;
pub use restore::{CheckpointProgress, CheckpointRestore};
use std::collections::{BTreeMap, VecDeque};

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
struct PredictedStroke {
    start: StrokeStart,
    renderer: StrokeRenderer,
    selection: Option<Surface>,
    last: Option<photocraft_paint::StrokePoint>,
    sequence: u32,
    end_nonce: Option<u64>,
}
struct ReplayCommand {
    independent_strokes: bool,
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
    predicted_stroke: Option<PredictedStroke>,
    baseline: Option<Document>,
    baseline_bytes: Option<Vec<u8>>,
    history_bytes: usize,
    journal: Vec<JournalEntry>,
    commands: Vec<ReplayCommand>,
    bases: BTreeMap<LayerId, Surface>,
    strokes: Vec<ReplayStroke>,
    redo: BTreeMap<String, Vec<String>>,
    undo_floor: BTreeMap<String, usize>,
    operation_damage: Option<Rect>,
    damage_ledger: VecDeque<(photocraft_doc::DocId, u64, Option<Rect>)>,
}
#[derive(Clone, Debug)]
pub enum NetworkIntent {
    Create { code: String, password: Option<String>, server: String },
    Join { code: String, password: Option<String>, server: String },
    Leave,
}
impl Collaboration {
    /// Union known damage across consecutive document revisions. Missing history or
    /// semantic document edits require a full refresh (`None`). Metadata yields EMPTY.
    pub fn damage_since(&self, doc_id: photocraft_doc::DocId, seen: u64, current: u64) -> Option<Rect> {
        if seen > current || self.document_id != Some(doc_id) {
            return None;
        }
        if seen == current {
            return Some(Rect::EMPTY);
        }
        let mut expected = seen.checked_add(1)?;
        let mut damage = Rect::EMPTY;
        for (id, revision, rect) in &self.damage_ledger {
            if *id != doc_id || *revision < expected {
                continue;
            }
            if *revision != expected {
                return None;
            }
            damage = damage.union(&(*rect)?);
            if *revision == current {
                return Some(damage);
            }
            expected = expected.checked_add(1)?;
        }
        None
    }

    fn record_damage(&mut self, doc_id: photocraft_doc::DocId, revision: u64, damage: Option<Rect>) {
        self.damage_ledger.push_back((doc_id, revision, damage));
        if self.damage_ledger.len() > 256 {
            self.damage_ledger.pop_front();
        }
    }

    // Prediction is deliberately limited to one unacknowledged gesture. Canonical
    // history remains independent, and a stalled connection cannot accumulate renderers.
    fn predict_input(&mut self, doc: &Document, message: &ClientMessage) -> crate::Result<()> {
        if !self.pending_commands.is_empty() {
            return Ok(());
        }
        match &message.operation {
            Operation::StrokeBegin { stroke } if self.predicted_stroke.is_none() => {
                let Some(layer) = doc.layer(stroke.layer) else { return Ok(()) };
                if layer.locks.all || layer.locks.pixels {
                    return Err(EngineError::Other("target layer is locked".into()));
                }
                let surface = layer.surface().ok_or(EngineError::NoLayer(stroke.layer))?;
                let mut selection =
                    Surface::new(photocraft_color::PixelFormat::new(photocraft_color::ColorMode::Grayscale, photocraft_color::SampleType::F32, false));
                for run in &stroke.selection {
                    for (offset, coverage) in run.coverage.iter().enumerate() {
                        selection.write_pixel(run.x.saturating_add(i32::try_from(offset).unwrap_or(i32::MAX)), run.y, &[*coverage]);
                    }
                }
                let mut start = (**stroke).clone();
                start.lock_transparency = layer.locks.transparency;
                self.predicted_stroke = Some(PredictedStroke {
                    renderer: StrokeRenderer::new(&start.brush, Some(surface.format()), start.zoom),
                    selection: (!start.selection.is_empty()).then_some(selection),
                    start,
                    last: None,
                    sequence: 0,
                    end_nonce: None,
                });
            }
            Operation::StrokeChunk { id, sequence, points } => {
                let Some(prediction) = self.predicted_stroke.as_mut().filter(|p| p.start.id == *id) else { return Ok(()) };
                if prediction.end_nonce.is_some() || prediction.sequence != *sequence {
                    return Err(EngineError::Other("invalid predicted stroke sequence".into()));
                }
                ChunkBudget::new(&prediction.start.brush).validate(prediction.last, points).map_err(error)?;
                let mut last = prediction.last;
                let mut bounds = prediction.renderer.bounds();
                let radius = f64::from(prediction.start.brush.size) * 2.0;
                for point in points {
                    if let Some(previous) = last
                        && (point.time < previous.time || point.time - previous.time > 1000.0 || (point.x - previous.x).hypot(point.y - previous.y) > 4096.0)
                    {
                        return Err(EngineError::Other("stroke sample gap exceeds collaboration budget".into()));
                    }
                    last = Some(*point);
                    bounds = bounds.union(&Rect::new(
                        (point.x - radius).floor() as i32,
                        (point.y - radius).floor() as i32,
                        (point.x + radius).ceil() as i32,
                        (point.y + radius).ceil() as i32,
                    ));
                }
                if i64::from(bounds.width()).saturating_mul(i64::from(bounds.height())) > 16_777_216 {
                    return Err(EngineError::Other("stroke exceeds collaboration rendering budget; release and start another stroke".into()));
                }
                prediction.last = last;
                prediction.renderer.push(points);
                prediction.sequence = prediction.sequence.saturating_add(1);
            }
            Operation::StrokeEnd { id, sequence } => {
                if let Some(prediction) = self.predicted_stroke.as_mut().filter(|p| p.start.id == *id) {
                    if prediction.sequence != *sequence || prediction.end_nonce.is_some() {
                        return Err(EngineError::Other("invalid predicted stroke end".into()));
                    }
                    prediction.renderer.finish();
                    prediction.end_nonce = Some(message.nonce);
                }
            }
            _ => {}
        }
        Ok(())
    }
    // Retain predicted pixels outside canonical damage, including same256px tile
    // neighbors. Sparse stroke history changes have already refreshed live prefixes.
    fn reuse_prediction(&mut self, doc: &mut Document, event: &HostMessage, previous: &Surface) -> bool {
        let Some(prediction) = self.predicted_stroke.as_ref() else {
            return false;
        };
        if !self.pending_commands.is_empty() {
            return false;
        }
        let same_layer = match &event.message.operation {
            Operation::StrokeChunk { id, .. } | Operation::StrokeEnd { id, .. } => {
                let Some(remote) = self.strokes.iter().find(|stroke| stroke.start.id == *id) else { return false };
                remote.start.layer == prediction.start.layer
            }
            // `Some` operation damage below proves a sparse stroke undo/redo.
            // Its layer need not be guessed: repairing this layer's canonical
            // rectangle is safe even if the change occurred on another layer.
            Operation::Undo | Operation::Redo => true,
            _ => return false,
        };
        let Some(damage) = self.operation_damage else {
            return false;
        };
        let Some(target) = doc.layer_mut(prediction.start.layer).and_then(|layer| layer.surface_mut()) else {
            return false;
        };
        if target.format() != previous.format() {
            return false;
        }
        let mut merged = previous.clone();
        if same_layer && !damage.is_empty() {
            merged.write_region(damage, &target.read_region(damage));
        }
        *target = merged;
        if same_layer && !damage.intersect(&prediction.renderer.bounds()).is_empty() {
            let repaired = self.predict_document_region(doc, false, Some(damage));
            self.operation_damage = self.operation_damage.map(|known| known.union(&repaired));
        }
        true
    }

    // Recompose the full predicted own coverage from its canonical prefix; never
    // source-over an acknowledged prefix again. Later canonical peers retain order.
    fn predict_document(&mut self, doc: &mut Document, dirty_only: bool) -> Rect {
        self.predict_document_region(doc, dirty_only, None)
    }

    // A canonical chunk changes only its damaged tiles. Retain the previous COW
    // prediction elsewhere instead of repainting the whole gesture on every ACK.
    fn predict_document_region(&mut self, doc: &mut Document, dirty_only: bool, repair: Option<Rect>) -> Rect {
        let Some(prediction) = self.predicted_stroke.as_mut() else { return Rect::EMPTY };
        let bounds = prediction.renderer.bounds();
        let index = self.strokes.iter().position(|stroke| stroke.start.id == prediction.start.id);
        if !self.pending_commands.is_empty()
            || index.is_some_and(|index| {
                self.journal
                    .iter()
                    .skip_while(|entry| !matches!(entry, JournalEntry::Stroke(i) if *i == index))
                    .any(|entry| matches!(entry, JournalEntry::Command(i) if self.commands.get(*i).is_some_and(|command| command.visible && !command.independent_strokes)))
            })
        {
            self.predicted_stroke = None;
            return bounds;
        }
        let Some(target) = doc.layer_mut(prediction.start.layer).and_then(|layer| layer.surface_mut()) else {
            self.predicted_stroke = None;
            return bounds;
        };
        if let Some(stroke) = index.and_then(|i| self.strokes.get(i)) {
            prediction.start.lock_transparency = stroke.start.lock_transparency;
        }
        let pre = index
            .and_then(|i| self.strokes.get(i))
            .map(|stroke| stroke.pre.clone())
            .or_else(|| self.canonical_document.as_ref().and_then(|doc| doc.layer(prediction.start.layer)).and_then(|layer| layer.surface()).cloned())
            .unwrap_or_else(|| target.clone());
        if pre.format() != target.format() {
            self.predicted_stroke = None;
            return bounds;
        }
        let mut tiles = if dirty_only { prediction.renderer.dirty_tiles() } else { prediction.renderer.coverage_tiles() };
        if let Some(repair) = repair {
            tiles.retain(|&(x, y)| {
                !Rect::new(
                    x.saturating_mul(COV_TILE),
                    y.saturating_mul(COV_TILE),
                    x.saturating_add(1).saturating_mul(COV_TILE),
                    y.saturating_add(1).saturating_mul(COV_TILE),
                )
                .intersect(&repair)
                .is_empty()
            });
        }
        let mut damage = Rect::EMPTY;
        // Restore only own bounds, not whole coverage tiles: unrelated peer pixels
        // outside this gesture must remain canonical.
        for &(x, y) in &tiles {
            let rect = Rect::new(
                x.saturating_mul(COV_TILE),
                y.saturating_mul(COV_TILE),
                x.saturating_add(1).saturating_mul(COV_TILE),
                y.saturating_add(1).saturating_mul(COV_TILE),
            )
            .intersect(&bounds);
            damage = damage.union(&rect);
            target.write_region(rect, &pre.read_region(rect));
        }
        prediction.renderer.composite_tiles_in_rect(&pre, target, prediction.selection.as_ref(), prediction.start.lock_transparency, &tiles, bounds);
        if let Some(index) = index {
            for stroke in self
                .strokes
                .iter_mut()
                .skip(index.saturating_add(1))
                .filter(|stroke| stroke.visible && stroke.start.layer == prediction.start.layer && !stroke.renderer.bounds().intersect(&bounds).is_empty())
            {
                let prefix = target.clone();
                stroke.renderer.composite_tiles_in_rect(&prefix, target, stroke.selection.as_ref(), stroke.start.lock_transparency, &tiles, bounds);
            }
        }
        damage
    }

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
        self.operation_damage = Some(Rect::EMPTY);
        if matches!(event.message.operation, Operation::Command { .. } | Operation::Visibility { mode: VisibilityMode::Shared, .. }) {
            self.operation_damage = None;
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
        let mut history_stroke = None;
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
                self.commands.push(ReplayCommand {
                    independent_strokes: independent_layer_creation(&edit.id, event.delta.as_ref()),
                    edit: (**edit).clone(),
                    owner: owner.clone(),
                    visible: true,
                    created,
                });
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
                        if let JournalEntry::Stroke(index) = entry
                            && self.commands.iter().all(|command| command.independent_strokes)
                            && event.delta.is_none()
                        {
                            history_stroke = Some(*index);
                            affected = self.strokes.get(*index).map(|stroke| stroke.start.layer);
                        } else {
                            full_replay = true;
                        }
                        break;
                    }
                }
            }
            Operation::Redo => {
                if let Some(key) = self.redo.get_mut(owner).and_then(Vec::pop) {
                    if let Some((index, stroke)) = self.strokes.iter_mut().enumerate().find(|(_, stroke)| stroke.owner == *owner && stroke.start.id == key) {
                        stroke.visible = true;
                        if self.commands.iter().all(|command| command.independent_strokes)
                            && event.delta.is_none()
                            && doc.layer(stroke.start.layer).and_then(|layer| layer.surface()).is_some()
                        {
                            history_stroke = Some(index);
                            affected = Some(stroke.start.layer);
                        } else {
                            full_replay = true;
                        }
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
                .any(|entry| matches!(entry,JournalEntry::Command(index) if self.commands.get(*index).is_some_and(|command|command.visible && !command.independent_strokes)));
        }
        if full_replay {
            self.operation_damage = None;
            if let Some(delta) = &event.delta {
                let authoritative = photocraft_collab::delta::apply_delta(doc, delta).map_err(error)?;
                // Undo/redo also changes the prefix caches used by live overlapping
                // strokes. Refresh those once, while retaining the host's exact result.
                self.rebuild_document(doc)?;
                *doc = authoritative;
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
            if let Some(index) = history_stroke.or_else(|| changed.and_then(|id| self.strokes.iter().position(|stroke| stroke.start.id == id))) {
                let tiles = self
                    .strokes
                    .get(index)
                    .map(|stroke| if history_stroke.is_some() { stroke.renderer.coverage_tiles() } else { stroke.renderer.dirty_tiles() })
                    .unwrap_or_default();
                let damage = self.rebuild_changed_tiles(doc, layer, index, &tiles, history_stroke.is_some())?;
                self.operation_damage = self.operation_damage.map(|known| known.union(&damage));
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
        if (matches!(event.message.operation, Operation::Command { .. })
            || (full_replay && matches!(event.message.operation, Operation::Undo | Operation::Redo)))
            && event.delta.is_none()
        {
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
    fn rebuild_command(&mut self, index: usize, rebuilt: &mut Document) -> crate::Result<()> {
        let needs_prototypes = self.commands.get(index).is_some_and(|command| {
            command.created.is_empty() && command.edit.context.get("createdLayerIds").and_then(serde_json::Value::as_array).is_some_and(|ids| !ids.is_empty())
        });
        let recovered = if needs_prototypes {
            let command = self.commands.get(index).ok_or_else(|| EngineError::Other("missing checkpoint command".into()))?;
            let (next, _) = crate::collab_resources::execute_resolved(rebuilt, &command.edit)?;
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
                    None => match crate::collab_resources::execute_resolved(rebuilt, &command.edit) {
                        Ok((next, _)) => next,
                        // Removing an author's source ink can leave a previously
                        // accepted peer transform with no pixels. Keep the operator
                        // in history so redo applies it when its input returns.
                        Err(EngineError::Other(message)) if command.edit.id.starts_with("edit.transform") && message == "nothing to transform" => {
                            (*rebuilt).clone()
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
                let independent = independent_creation_documents(&command.edit.id, rebuilt, &next);
                if let Some(command) = self.commands.get_mut(index) {
                    command.independent_strokes = independent;
                }
                *rebuilt = next;
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
                        && let Some(shell) = dependency_shell(layer, &created, &needed, rebuilt, false, 0)?
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

        Ok(())
    }
    fn rebuild_document(&mut self, doc: &mut Document) -> crate::Result<()> {
        let mut rebuilt = self.baseline.clone().ok_or(EngineError::NoDocument)?;
        for entry in self.journal.clone() {
            match entry {
                JournalEntry::Stroke(index) => {
                    if let Some(stroke) = self.strokes.get_mut(index) {
                        let Some(target) = rebuilt.layer_mut(stroke.start.layer).and_then(|layer| layer.surface_mut()) else {
                            if !stroke.visible {
                                continue;
                            }
                            return Err(EngineError::NoLayer(stroke.start.layer));
                        };
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
                        // Hidden strokes still need the current prefix for a later
                        // sparse redo, including after checkpoint installation.
                        stroke.pre = target.clone();
                        if stroke.visible {
                            stroke.renderer.composite(&stroke.pre, target, stroke.selection.as_ref(), stroke.start.lock_transparency, true);
                        }
                    }
                }
                JournalEntry::Command(index) => self.rebuild_command(index, &mut rebuilt)?,
            }
        }
        rebuilt.selection = doc.selection.clone();
        rebuilt.quick_mask = doc.quick_mask.clone();
        *doc = rebuilt;
        Ok(())
    }
    /// A stroke's saved pre-surface is the canonical prefix immediately before
    /// that stroke. Replay only changed coverage tiles through its later siblings;
    /// earlier strokes and commands cannot depend on the changed suffix.
    fn rebuild_changed_tiles(&mut self, doc: &mut Document, id: LayerId, changed: usize, tiles: &[(i32, i32)], history: bool) -> crate::Result<Rect> {
        if tiles.is_empty() {
            return Ok(Rect::EMPTY);
        }
        let pre = self.strokes.get(changed).ok_or_else(|| EngineError::Other("missing changed stroke".into()))?.pre.clone();
        let clip = history.then(|| self.strokes.get(changed).map(|stroke| stroke.renderer.bounds()).unwrap_or(Rect::EMPTY));
        let target = doc.layer_mut(id).and_then(|layer| layer.surface_mut()).ok_or(EngineError::NoLayer(id))?;
        let mut damage = Rect::EMPTY;
        let mut regions = Vec::with_capacity(tiles.len());
        for &(x, y) in tiles {
            let rect = Rect::new(
                x.saturating_mul(COV_TILE),
                y.saturating_mul(COV_TILE),
                x.saturating_add(1).saturating_mul(COV_TILE),
                y.saturating_add(1).saturating_mul(COV_TILE),
            );
            let rect = clip.map_or(rect, |bounds| rect.intersect(&bounds));
            if rect.is_empty() {
                continue;
            }
            regions.push(rect);
            damage = damage.union(&rect);
            target.write_region(rect, &pre.read_region(rect));
        }
        for stroke in self.strokes.iter_mut().skip(changed).filter(|stroke| stroke.start.layer == id) {
            let intersects = regions.iter().any(|rect| !rect.intersect(&stroke.renderer.bounds()).is_empty());
            if stroke.ended && !intersects {
                continue;
            }
            // A live later stroke can enter this tile in a future chunk, so its
            // prefix must advance even while its current coverage is disjoint.
            for rect in &regions {
                // Ended coverage cannot grow; retain only the prefix pixels its
                // compositor can read. Live strokes keep the whole future tile.
                let prefix = if stroke.ended { rect.intersect(&stroke.renderer.bounds()) } else { *rect };
                if !prefix.is_empty() {
                    stroke.pre.write_region(prefix, &target.read_region(prefix));
                }
            }
            if !intersects || !stroke.visible {
                continue;
            }
            if let Some(rect) = clip {
                stroke.renderer.composite_tiles_in_rect(&stroke.pre, target, stroke.selection.as_ref(), stroke.start.lock_transparency, tiles, rect);
            } else {
                stroke.renderer.composite_tiles(&stroke.pre, target, stroke.selection.as_ref(), stroke.start.lock_transparency, tiles);
            }
        }
        Ok(damage)
    }
}
// Narrow semantic allowlist plus proof from the actual authoritative transaction:
// adding a text/vector/empty raster layer cannot depend on later brush samples.
fn independent_creation_documents(id: &str, before: &Document, after: &Document) -> bool {
    if !matches!(id, "type.create" | "shape.create" | "layer.new.layer")
        || after.layers.len() <= before.layers.len()
        || before.layers.iter().any(|layer| after.layers.iter().find(|candidate| candidate.id == layer.id) != Some(layer))
    {
        return false;
    }
    let mut before = before.clone();
    let mut after = after.clone();
    before.layers.clear();
    after.layers.clear();
    before == after
}

fn independent_layer_creation(id: &str, delta: Option<&photocraft_collab::delta::DocumentDelta>) -> bool {
    if !matches!(id, "type.create" | "shape.create" | "layer.new.layer") {
        return false;
    }
    let Some(delta) = delta else {
        return false;
    };
    let mut before = delta.base_manifest.document.clone();
    let mut after = delta.manifest.document.clone();
    if after.layers.len() <= before.layers.len()
        || before.layers.iter().any(|layer| after.layers.iter().find(|candidate| candidate.id == layer.id) != Some(layer))
    {
        return false;
    }
    before.layers.clear();
    after.layers.clear();
    before == after
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
            if matches!(message.operation, Operation::StrokeBegin { .. } | Operation::StrokeChunk { .. } | Operation::StrokeEnd { .. }) {
                let state = self.docs.iter_mut().find(|state| Some(state.doc.id) == self.collaboration.document_id).ok_or(EngineError::NoDocument)?;
                let canonical = self.collaboration.canonical_document.get_or_insert_with(|| (*state.doc).clone()).clone();
                self.collaboration.predict_input(&canonical, &message)?;
                let mut predicted = (*state.doc).clone();
                let damage = self.collaboration.predict_document(&mut predicted, true);
                state.doc = std::sync::Arc::new(predicted);
                state.revision = state.revision.saturating_add(1);
                state.last_damage = Some(damage);
                self.collaboration.record_damage(state.doc.id, state.revision, Some(damage));
            }
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
        let id = self.collaboration.document_id.or_else(|| self.active().map(|state| state.doc.id)).ok_or(EngineError::NoDocument)?;
        let mut restore = CheckpointRestore::new(checkpoint)?;
        while !restore.step(64)?.done {}
        restore.install_existing(self, id)
    }
    /// Commit a fully restored guest room atomically. Dropping an unfinished builder cancels it.
    pub fn collaboration_commit_checkpoint(&mut self, restore: CheckpointRestore, room: RoomState) -> crate::Result<photocraft_doc::DocId> {
        restore.install_new(self, room)
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
        self.after_command(id, params, true, None);
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
            let previous_prediction =
                collab.predicted_stroke.as_ref().and_then(|prediction| st.doc.layer(prediction.start.layer)).and_then(|layer| layer.surface()).cloned();
            let mut doc = collab.canonical_document.clone().unwrap_or_else(|| (*st.doc).clone());
            doc.selection = st.doc.selection.clone();
            doc.quick_mask = st.doc.quick_mask.clone();
            collab.apply(&mut doc, event).map(|()| {
                let predicted_command = if let Operation::Command { edit } = &event.message.operation {
                    let predicted = collab.pending_commands.iter().any(|pending| pending.key == edit.key);
                    collab.pending_commands.retain(|pending| pending.key != edit.key);
                    predicted
                } else {
                    false
                };
                collab.canonical_document = Some(doc.clone());
                let old_prediction = collab.predicted_stroke.as_ref().map(|p| p.renderer.bounds()).unwrap_or(Rect::EMPTY);
                if collab.predicted_stroke.as_ref().is_some_and(|prediction| {
                    prediction.end_nonce.is_some_and(|nonce| {
                        event.message.peer == collab.room.as_ref().map(|room| room.local_peer.as_str()).unwrap_or("") && event.message.nonce >= nonce
                    })
                }) {
                    collab.predicted_stroke = None;
                }
                if !previous_prediction.as_ref().is_some_and(|previous| collab.reuse_prediction(&mut doc, event, previous)) {
                    let prediction_damage = old_prediction.union(&collab.predict_document(&mut doc, false));
                    collab.operation_damage = collab.operation_damage.map(|damage| damage.union(&prediction_damage));
                }
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
                        .filter(|id| !predicted_command || (st.active_layer == Some(*id) && st.selected_layers == [*id]))
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
                let damage = if collab.pending_commands.is_empty() { collab.operation_damage } else { None };
                st.last_damage = damage;
                collab.record_damage(st.doc.id, st.revision, damage);
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
            self.after_command("paint.stroke", params, true, None);
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
    fn remote_other_layer_reuses_prediction_even_at_overlapping_coordinates() {
        let mut host = session(8);
        let layer = host.active().unwrap().active_layer.unwrap();
        let format = host.active().unwrap().doc.layer(layer).unwrap().surface().unwrap().format();
        let other = std::sync::Arc::make_mut(&mut host.active_mut().unwrap().doc)
            .insert_above(Some(layer), photocraft_doc::Layer::new("peer", photocraft_doc::LayerContent::Raster(Surface::new(format))));
        let mut guest = Session::new();
        guest.add_document((*host.active().unwrap().doc).clone(), None);
        guest.execute("collab.room.join", json!({"peer":"guest","code":"TEST01"})).unwrap();
        guest.collaboration.document_id = guest.active().map(|state| state.doc.id); // Simulate completed transport admission.
        guest
            .collaboration_submit(Operation::StrokeBegin {
                stroke: Box::new(StrokeStart {
                    id: "other-layer-prediction".into(),
                    layer,
                    zoom: 1.0,
                    brush: BrushSettings { size: 11.0, pressure_size: false, ..Default::default() },
                    selection: Vec::new(),
                    lock_transparency: false,
                }),
            })
            .unwrap();
        host.collaboration_accept("guest", guest.collaboration.outbox.last().unwrap().clone()).unwrap();
        guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
        guest
            .collaboration_submit(Operation::StrokeChunk { id: "other-layer-prediction".into(), sequence: 0, points: vec![StrokePoint::new(120.0, 20.0, 0.8)] })
            .unwrap();
        let own = guest.active().unwrap().doc.layer(layer).unwrap().surface().unwrap().clone();
        emit(
            &mut host,
            "peer",
            Operation::StrokeBegin {
                stroke: Box::new(StrokeStart {
                    id: "other-layer".into(),
                    layer: other,
                    zoom: 1.0,
                    brush: BrushSettings { size: 12.0, color: [0.1, 0.8, 0.4, 1.0], ..Default::default() },
                    selection: Vec::new(),
                    lock_transparency: false,
                }),
            },
        );
        guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
        for (sequence, x) in [20.0, 120.0].into_iter().enumerate() {
            chunk(&mut host, "peer", "other-layer", sequence as u32, vec![StrokePoint::new(x, 20.0, 0.8)]);
            guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
            assert_eq!(guest.active().unwrap().last_damage, host.collaboration.operation_damage);
            assert_eq!(guest.active().unwrap().doc.layer(layer).unwrap().surface(), Some(&own));
            assert_eq!(guest.active().unwrap().doc.layer(other).unwrap().surface(), host.active().unwrap().doc.layer(other).unwrap().surface());
        }
        end(&mut host, "peer", "other-layer", 2);
        guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
        assert_eq!(guest.active().unwrap().last_damage, host.collaboration.operation_damage);
        assert_eq!(guest.active().unwrap().doc.layer(layer).unwrap().surface(), Some(&own));
    }

    fn full_prediction_oracle(collab: &Collaboration) -> Document {
        let mut doc = collab.canonical_document.as_ref().unwrap().clone();
        let prediction = collab.predicted_stroke.as_ref().unwrap();
        let index = collab.strokes.iter().position(|stroke| stroke.start.id == prediction.start.id).unwrap();
        let pre = &collab.strokes[index].pre;
        let target = doc.layer_mut(prediction.start.layer).unwrap().surface_mut().unwrap();
        let bounds = prediction.renderer.bounds();
        let tiles = prediction.renderer.coverage_tiles();
        for &(x, y) in &tiles {
            let rect = Rect::new(x * COV_TILE, y * COV_TILE, (x + 1) * COV_TILE, (y + 1) * COV_TILE).intersect(&bounds);
            target.write_region(rect, &pre.read_region(rect));
        }
        prediction.renderer.clone().composite_tiles_in_rect(pre, target, prediction.selection.as_ref(), prediction.start.lock_transparency, &tiles, bounds);
        for stroke in collab.strokes.iter().skip(index + 1).filter(|stroke| stroke.visible && stroke.start.layer == prediction.start.layer) {
            let prefix = target.clone();
            stroke.renderer.clone().composite_tiles_in_rect(&prefix, target, stroke.selection.as_ref(), stroke.start.lock_transparency, &tiles, bounds);
        }
        doc
    }

    #[test]
    fn dense_history_prediction_repairs_only_changed_tiles_exactly() {
        for depth in [8, 16, 32] {
            for erase in [false, true] {
                let mut host = session(depth);
                std::sync::Arc::make_mut(&mut host.active_mut().unwrap().doc).size = photocraft_geom::Size { width: 512, height: 512 };
                let layer = host.active().unwrap().active_layer.unwrap();
                for index in 0..128 {
                    let id = format!("history-{index}");
                    begin(&mut host, "peer", &id, [0.2, 0.5, 0.8, 1.0]);
                    let x = 180.0 + f64::from(index % 16) * 8.0;
                    let y = 180.0 + f64::from(index / 16) * 8.0;
                    chunk(&mut host, "peer", &id, 0, vec![StrokePoint::new(x, y, 0.4), StrokePoint::new(x + 4.0, y + 4.0, 0.9)]);
                    end(&mut host, "peer", &id, 1);
                }
                let mut guest = Session::new();
                guest.add_document((*host.active().unwrap().doc).clone(), None);
                guest.execute("collab.room.join", json!({"peer":"guest","code":"TEST01"})).unwrap();
                guest.collaboration.document_id = guest.active().map(|state| state.doc.id); // Simulate completed transport admission.
                guest.collaboration_install_checkpoint(host.collaboration_checkpoint().unwrap()).unwrap();
                guest
                    .collaboration_submit(Operation::StrokeBegin {
                        stroke: Box::new(StrokeStart {
                            id: "dense-prediction".into(),
                            layer,
                            zoom: 1.3,
                            brush: BrushSettings {
                                size: 240.0,
                                opacity: 0.4,
                                pressure_size: false,
                                pressure_opacity: true,
                                seed: 99,
                                erase,
                                color: [0.9, 0.1, 0.3, 1.0],
                                ..Default::default()
                            },
                            selection: (128..384)
                                .map(|y| SelectionRun { x: 128, y, coverage: (0..256).map(|x| if (x + y) % 32 < 8 { 0.0 } else { 0.7 }).collect() })
                                .collect(),
                            lock_transparency: false,
                        }),
                    })
                    .unwrap();
                host.collaboration_accept("guest", guest.collaboration.outbox.last().unwrap().clone()).unwrap();
                guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                guest
                    .collaboration_submit(Operation::StrokeChunk {
                        id: "dense-prediction".into(),
                        sequence: 0,
                        points: vec![StrokePoint::new(250.0, 250.0, 0.8)],
                    })
                    .unwrap();
                let first = guest.collaboration.outbox.last().unwrap().clone();
                guest.collaboration_submit(Operation::StrokeEnd { id: "dense-prediction".into(), sequence: 1 }).unwrap();
                let ending = guest.collaboration.outbox.last().unwrap().clone();
                let count = guest.collaboration.predicted_stroke.as_ref().unwrap().renderer.coverage_tiles().len();
                assert!(count >= 12);
                for index in 0..8 {
                    // Before the first later stroke, undo/redo changes an earlier
                    // prefix. Subsequent rounds change a later overlapping stroke.
                    for operation in [Operation::Undo, Operation::Redo] {
                        emit(&mut host, "peer", operation);
                        guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                        assert_eq!(
                            guest.active().unwrap().doc.layer(layer).unwrap().surface(),
                            full_prediction_oracle(&guest.collaboration).layer(layer).unwrap().surface()
                        );
                    }
                    let id = format!("later-{index}");
                    begin(&mut host, "peer", &id, [0.1, 0.9, 0.4, 1.0]);
                    guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                    for sequence in 0..3 {
                        chunk(&mut host, "peer", &id, sequence, vec![StrokePoint::new(240.0 + f64::from(sequence) * 4.0, 240.0 + f64::from(index), 0.6)]);
                        guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                        let oracle = full_prediction_oracle(&guest.collaboration);
                        assert_eq!(
                            guest.active().unwrap().doc.layer(layer).unwrap().surface(),
                            oracle.layer(layer).unwrap().surface(),
                            "partial repair must equal independent full coverage oracle"
                        );
                    }
                    end(&mut host, "peer", &id, 3);
                    guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                }
                host.collaboration_accept("guest", first).unwrap();
                guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                assert_eq!(
                    guest.active().unwrap().doc.layer(layer).unwrap().surface(),
                    full_prediction_oracle(&guest.collaboration).layer(layer).unwrap().surface()
                );
                host.collaboration_accept("guest", ending).unwrap();
                guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                assert_eq!(guest.active().unwrap().doc.layer(layer).unwrap().surface(), host.active().unwrap().doc.layer(layer).unwrap().surface());
                for operation in [Operation::Undo, Operation::Redo] {
                    emit(&mut host, "guest", operation);
                    guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                    assert_eq!(guest.active().unwrap().doc.layer(layer).unwrap().surface(), host.active().unwrap().doc.layer(layer).unwrap().surface());
                }
            }
        }
    }

    #[test]
    fn four_guests_twelve_delayed_gestures_keep_canonical_pixels_and_ended_history() {
        for depth in [8, 16, 32] {
            let mut host = session(depth);
            std::sync::Arc::make_mut(&mut host.active_mut().unwrap().doc).size = photocraft_geom::Size { width: 512, height: 512 };
            let layer = host.active().unwrap().active_layer.unwrap();
            for index in 0..128 {
                let id = format!("baseline-{index}");
                begin(&mut host, "peer", &id, [0.2, 0.5, 0.8, 1.0]);
                chunk(&mut host, "peer", &id, 0, vec![StrokePoint::new(200.0, 200.0, 0.4), StrokePoint::new(210.0, 210.0, 0.9)]);
                end(&mut host, "peer", &id, 1);
            }
            let mut guests: Vec<_> = (0..4)
                .map(|artist| {
                    let mut guest = Session::new();
                    guest.add_document((*host.active().unwrap().doc).clone(), None);
                    guest.execute("collab.room.join", json!({"peer":format!("guest-{artist}"),"code":"TEST01"})).unwrap();
                    guest.collaboration.document_id = guest.active().map(|state| state.doc.id); // Simulate completed transport admission.
                    guest.collaboration_install_checkpoint(host.collaboration_checkpoint().unwrap()).unwrap();
                    guest
                })
                .collect();
            for (artist, guest) in guests.iter_mut().enumerate() {
                for gesture in 0..12 {
                    let id = format!("guest-{artist}-gesture-{gesture}");
                    guest
                        .collaboration_submit(Operation::StrokeBegin {
                            stroke: Box::new(StrokeStart {
                                id: id.clone(),
                                layer,
                                zoom: 1.2,
                                brush: BrushSettings {
                                    size: 64.0,
                                    pressure_opacity: true,
                                    opacity: 0.4,
                                    color: [artist as f32 / 4.0, 0.3, 0.8, 1.0],
                                    erase: gesture % 3 == 2,
                                    seed: 121,
                                    ..Default::default()
                                },
                                selection: Vec::new(),
                                lock_transparency: false,
                            }),
                        })
                        .unwrap();
                    for sequence in 0..3 {
                        let points = (0..4)
                            .map(|sample| {
                                let mut point =
                                    StrokePoint::new(180.0 + f64::from(sequence * 4 + sample) * 8.0, 200.0 + f64::from(gesture), 0.3 + sample as f32 * 0.15);
                                point.tilt_x = 0.2;
                                point.tilt_y = -0.1;
                                point.rotation = 0.4;
                                point
                            })
                            .collect();
                        guest.collaboration_submit(Operation::StrokeChunk { id: id.clone(), sequence, points }).unwrap();
                    }
                    guest.collaboration_submit(Operation::StrokeEnd { id, sequence: 3 }).unwrap();
                }
                assert!(guest.collaboration.own_history().is_empty(), "completed input is not an acknowledged canonical stroke");
                assert!(guest.collaboration.predicted_stroke.as_ref().is_some_and(|prediction| prediction.end_nonce.is_some()));
                assert_ne!(
                    guest.active().unwrap().doc.layer(layer).unwrap().surface(),
                    guest.collaboration.canonical_document.as_ref().unwrap().layer(layer).unwrap().surface(),
                    "unacknowledged display must not be mistaken for canonical convergence"
                );
            }
            let mut queued: Vec<_> = guests.iter().map(|guest| guest.collaboration.outbox.clone().into_iter()).collect();
            while queued.iter().any(|messages| messages.len() > 0) {
                for (artist, messages) in queued.iter_mut().enumerate() {
                    let Some(message) = messages.next() else { continue };
                    host.collaboration_accept(&format!("guest-{artist}"), message).unwrap();
                    let event = host.collaboration.canonical_outbox.last().unwrap().clone();
                    for guest in &mut guests {
                        guest.collaboration_receive(&event).unwrap();
                        assert_eq!(
                            guest.collaboration.canonical_document.as_ref().unwrap().layer(layer).unwrap().surface(),
                            host.active().unwrap().doc.layer(layer).unwrap().surface(),
                            "prediction must never contaminate canonical pixels"
                        );
                    }
                }
            }
            for guest in &guests {
                assert_eq!(guest.collaboration.own_history().len(), 12);
                assert!(guest.collaboration.predicted_stroke.is_none());
                assert_eq!(guest.active().unwrap().doc.layer(layer).unwrap().surface(), host.active().unwrap().doc.layer(layer).unwrap().surface());
            }
        }
    }

    #[test]
    #[ignore = "bounded native engine comparison; does not measure browser presentation FPS"]
    fn guest_disjoint_prediction_latency_benchmark() {
        for depth in [8, 32] {
            for (size, overlap) in [(512.0, false), (1024.0, false), (512.0, true), (1024.0, true)] {
                let mut host = session(depth);
                std::sync::Arc::make_mut(&mut host.active_mut().unwrap().doc).size = photocraft_geom::Size { width: 2048, height: 1536 };
                let layer = host.active().unwrap().active_layer.unwrap();
                let mut guest = Session::new();
                guest.add_document((*host.active().unwrap().doc).clone(), None);
                guest.execute("collab.room.join", json!({"peer":"guest","code":"TEST01"})).unwrap();
                guest.collaboration.document_id = guest.active().map(|state| state.doc.id); // Simulate completed transport admission.
                guest
                    .collaboration_submit(Operation::StrokeBegin {
                        stroke: Box::new(StrokeStart {
                            id: "large-prediction".into(),
                            layer,
                            zoom: 1.0,
                            brush: BrushSettings { size, pressure_size: false, opacity: 0.4, ..Default::default() },
                            selection: Vec::new(),
                            lock_transparency: false,
                        }),
                    })
                    .unwrap();
                host.collaboration_accept("guest", guest.collaboration.outbox.last().unwrap().clone()).unwrap();
                guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                guest
                    .collaboration_submit(Operation::StrokeChunk {
                        id: "large-prediction".into(),
                        sequence: 0,
                        points: vec![StrokePoint::new(768.0, 768.0, 0.8)],
                    })
                    .unwrap();
                guest.collaboration_submit(Operation::StrokeEnd { id: "large-prediction".into(), sequence: 1 }).unwrap();
                begin(&mut host, "peer", "far-away", [0.1, 0.8, 0.4, 1.0]);
                guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                chunk(
                    &mut host,
                    "peer",
                    "far-away",
                    0,
                    (0..4).map(|index| StrokePoint::new((if overlap { 760.0 } else { 1750.0 }) + f64::from(index) * 10.0, 768.0, 0.8)).collect(),
                );
                assert!(host.collaboration.operation_damage.is_some_and(|damage| !damage.is_empty()), "benchmark must merge real remote pixel damage");
                let event = host.collaboration.canonical_outbox.last().unwrap().clone();
                guest.collaboration_receive(&event).unwrap();
                let previous = guest.active().unwrap().doc.layer(layer).unwrap().surface().unwrap().clone();
                let canonical = guest.collaboration.canonical_document.as_ref().unwrap().clone();
                let coverage = guest.collaboration.predicted_stroke.as_ref().unwrap().renderer.coverage_tiles().len();
                assert!(coverage >= 32, "large prediction benchmark must render real large coverage");
                let mut full = Vec::new();
                let mut reuse = Vec::new();
                for _ in 0..16 {
                    let mut doc = canonical.clone();
                    let started = std::time::Instant::now();
                    guest.collaboration.predict_document(&mut doc, false);
                    full.push(started.elapsed().as_secs_f64() * 1000.0);
                    assert_eq!(doc.layer(layer).unwrap().surface(), Some(&previous));
                    let mut doc = canonical.clone();
                    let started = std::time::Instant::now();
                    assert!(guest.collaboration.reuse_prediction(&mut doc, &event, &previous));
                    reuse.push(started.elapsed().as_secs_f64() * 1000.0);
                    assert_eq!(doc.layer(layer).unwrap().surface(), Some(&previous));
                }
                full.sort_by(f64::total_cmp);
                reuse.sort_by(f64::total_cmp);
                println!(
                    "{}",
                    json!({"depth":depth,"brushSize":size,"overlap":overlap,"coverageTiles":coverage,"iterations":16,
                    "fullRebaseMs":{"p50":full[8],"p95":full[15],"p99":full[15]},
                    "reuseMs":{"p50":reuse[8],"p95":reuse[15],"p99":reuse[15]},
                    "replayedCoverageTiles":if overlap { guest.collaboration.operation_damage.map(|damage| guest.collaboration.predicted_stroke.as_ref().unwrap().renderer.coverage_tiles().into_iter().filter(|&(x,y)| !Rect::new(x*COV_TILE,y*COV_TILE,(x+1)*COV_TILE,(y+1)*COV_TILE).intersect(&damage).is_empty()).count()).unwrap_or(0) } else { 0 },"exact":true})
                );
            }
        }
    }

    #[test]
    fn own_creation_echo_preserves_newer_private_layer_choice() {
        for choice in ["untouched", "away", "multi"] {
            let mut host = session(8);
            let original = host.active().unwrap().active_layer.unwrap();
            let mut guest = Session::new();
            guest.add_document((*host.active().unwrap().doc).clone(), None);
            guest.execute("collab.room.join", json!({"peer":"guest","code":"TEST01"})).unwrap();
            guest.collaboration.document_id = guest.active().map(|state| state.doc.id); // Simulate completed transport admission.
            let created = LayerId(guest.execute("layer.new.layer", json!({"name":"pending creation"})).unwrap()["layer"].as_u64().unwrap());
            assert_eq!(guest.active().unwrap().active_layer, Some(created));
            if choice != "untouched" {
                guest.execute("layer.select", json!({"layer":original.0})).unwrap();
            }
            if choice == "multi" {
                guest.execute("layer.select", json!({"layer":created.0,"mode":"add"})).unwrap();
            }
            let selected = guest.active().unwrap().selected_layers.clone();
            let active = guest.active().unwrap().active_layer;
            host.collaboration_accept("guest", guest.collaboration.outbox.last().unwrap().clone()).unwrap();
            guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
            assert_eq!(guest.active().unwrap().active_layer, active, "{choice}");
            assert_eq!(guest.active().unwrap().selected_layers, selected, "{choice}");
            assert!(guest.active().unwrap().doc.layer(created).is_some());
        }
    }

    #[test]
    fn disjoint_prediction_reuse_keeps_same_tile_pixels_and_future_overlap_prefix() {
        for depth in [8, 16, 32] {
            for erase in [false, true] {
                let mut host = session(depth);
                let layer = host.active().unwrap().active_layer.unwrap();
                let mut guest = Session::new();
                guest.add_document((*host.active().unwrap().doc).clone(), None);
                guest.execute("collab.room.join", json!({"peer":"guest","code":"TEST01"})).unwrap();
                guest.collaboration.document_id = guest.active().map(|state| state.doc.id); // Simulate completed transport admission.
                begin(&mut host, "peer", "earlier-disjoint", [0.1, 0.6, 0.9, 1.0]);
                guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                guest
                    .collaboration_submit(Operation::StrokeBegin {
                        stroke: Box::new(StrokeStart {
                            id: "reuse".into(),
                            layer,
                            zoom: 1.0,
                            brush: BrushSettings {
                                size: 12.0,
                                erase,
                                opacity: 0.4,
                                pressure_size: false,
                                color: [0.9, 0.1, 0.3, 1.0],
                                seed: 77,
                                ..Default::default()
                            },
                            selection: Vec::new(),
                            lock_transparency: false,
                        }),
                    })
                    .unwrap();
                host.collaboration_accept("guest", guest.collaboration.outbox.last().unwrap().clone()).unwrap();
                guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                guest
                    .collaboration_submit(Operation::StrokeChunk { id: "reuse".into(), sequence: 0, points: vec![StrokePoint::new(20.0, 20.0, 0.7)] })
                    .unwrap();
                host.collaboration_accept("guest", guest.collaboration.outbox.last().unwrap().clone()).unwrap();
                guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                guest
                    .collaboration_submit(Operation::StrokeChunk { id: "reuse".into(), sequence: 1, points: vec![StrokePoint::new(30.0, 20.0, 0.8)] })
                    .unwrap();
                let pending = guest.collaboration.outbox.last().unwrap().clone();
                let old = guest.active().unwrap().doc.layer(layer).unwrap().surface().unwrap().read_region(Rect::new(0, 0, 48, 48));
                chunk(&mut host, "peer", "earlier-disjoint", 0, vec![StrokePoint::new(120.0, 20.0, 0.8)]);
                let damage = host.collaboration.operation_damage;
                guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                assert_eq!(guest.active().unwrap().last_damage, damage, "reuse must not invalidate own unaffected coverage");
                assert_eq!(
                    guest.active().unwrap().doc.layer(layer).unwrap().surface().unwrap().read_region(Rect::new(0, 0, 48, 48)),
                    old,
                    "same256tile retains unacknowledged pixels"
                );
                host.collaboration_accept("guest", pending).unwrap();
                assert_eq!(guest.active().unwrap().doc.layer(layer).unwrap().surface(), host.active().unwrap().doc.layer(layer).unwrap().surface());
                guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                guest
                    .collaboration_submit(Operation::StrokeChunk { id: "reuse".into(), sequence: 2, points: vec![StrokePoint::new(120.0, 20.0, 0.65)] })
                    .unwrap();
                host.collaboration_accept("guest", guest.collaboration.outbox.last().unwrap().clone()).unwrap();
                assert_eq!(
                    guest.active().unwrap().doc.layer(layer).unwrap().surface(),
                    host.active().unwrap().doc.layer(layer).unwrap().surface(),
                    "growth into earlier peer dirty area uses current prefix"
                );
                guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                guest.collaboration_submit(Operation::StrokeEnd { id: "reuse".into(), sequence: 3 }).unwrap();
                host.collaboration_accept("guest", guest.collaboration.outbox.last().unwrap().clone()).unwrap();
                guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                end(&mut host, "peer", "earlier-disjoint", 1);
                guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                assert_eq!(guest.active().unwrap().doc.layer(layer).unwrap().surface(), host.active().unwrap().doc.layer(layer).unwrap().surface());
            }
        }
    }

    #[test]
    fn dependent_generic_edit_suspends_prediction_and_clears_old_damage() {
        let mut host = session(8);
        let layer = host.active().unwrap().active_layer.unwrap();
        let mut guest = Session::new();
        guest.add_document((*host.active().unwrap().doc).clone(), None);
        guest.execute("collab.room.join", json!({"peer":"guest","code":"TEST01"})).unwrap();
        guest.collaboration.document_id = guest.active().map(|state| state.doc.id); // Simulate completed transport admission.
        guest
            .collaboration_submit(Operation::StrokeBegin {
                stroke: Box::new(StrokeStart {
                    id: "dependent".into(),
                    layer,
                    zoom: 1.0,
                    brush: BrushSettings { size: 9.0, opacity: 0.4, ..Default::default() },
                    selection: Vec::new(),
                    lock_transparency: false,
                }),
            })
            .unwrap();
        host.collaboration_accept("guest", guest.collaboration.outbox.last().unwrap().clone()).unwrap();
        guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
        guest.collaboration_submit(Operation::StrokeChunk { id: "dependent".into(), sequence: 0, points: vec![StrokePoint::new(20.0, 20.0, 0.8)] }).unwrap();
        assert!(guest.collaboration.predicted_stroke.is_some());
        host.execute("edit.fill", json!({"color":"#55aa77"})).unwrap();
        guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
        assert!(guest.collaboration.predicted_stroke.is_none());
        assert_eq!(guest.active().unwrap().last_damage, None);
        assert_eq!(pixels(&guest), pixels(&host), "dependent edit clears stale own overlay");
    }

    #[test]
    fn rapid_unacknowledged_strokes_keep_first_prediction_until_ack_and_order_undo() {
        for depth in [8, 16, 32] {
            let mut host = session(depth);
            let layer = host.active().unwrap().active_layer.unwrap();
            let mut guest = Session::new();
            guest.add_document((*host.active().unwrap().doc).clone(), None);
            guest.execute("collab.room.join", json!({"peer":"guest","code":"TEST01"})).unwrap();
            guest.collaboration.document_id = guest.active().map(|state| state.doc.id); // Simulate completed transport admission.
            let mut first = None;
            for index in 0..3 {
                let id = format!("rapid-{index}");
                guest
                    .collaboration_submit(Operation::StrokeBegin {
                        stroke: Box::new(StrokeStart {
                            id: id.clone(),
                            layer,
                            zoom: 1.0,
                            brush: BrushSettings { size: 10.0, opacity: 0.4, color: [0.9, 0.1, 0.2, 1.0], seed: 22, ..Default::default() },
                            selection: Vec::new(),
                            lock_transparency: false,
                        }),
                    })
                    .unwrap();
                guest
                    .collaboration_submit(Operation::StrokeChunk {
                        id: id.clone(),
                        sequence: 0,
                        points: vec![StrokePoint::new(20.0 + f64::from(index) * 5.0, 24.0, 0.75)],
                    })
                    .unwrap();
                guest.collaboration_submit(Operation::StrokeEnd { id, sequence: 1 }).unwrap();
                if index == 0 {
                    first = Some(pixels(&guest));
                }
                assert_eq!(Some(pixels(&guest)), first, "bounded prediction retains earlier ended gesture while later gestures queue");
            }
            guest.collaboration_submit(Operation::Undo).unwrap();
            let queued = std::mem::take(&mut guest.collaboration.outbox);
            for (index, message) in queued.into_iter().enumerate() {
                host.collaboration_accept("guest", message).unwrap();
                guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                if index == 2 {
                    emit(
                        &mut host,
                        "peer",
                        Operation::StrokeBegin {
                            stroke: Box::new(StrokeStart {
                                id: "rapid-eraser".into(),
                                layer,
                                zoom: 1.0,
                                brush: BrushSettings { size: 12.0, erase: true, opacity: 0.6, ..Default::default() },
                                selection: Vec::new(),
                                lock_transparency: false,
                            }),
                        },
                    );
                    guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                    chunk(&mut host, "peer", "rapid-eraser", 0, vec![StrokePoint::new(25.0, 24.0, 0.6)]);
                    guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                    end(&mut host, "peer", "rapid-eraser", 1);
                    guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                }
            }
            assert!(guest.collaboration.predicted_stroke.is_none());
            assert_eq!(pixels(&guest), pixels(&host));
            assert_eq!(guest.collaboration.own_history().iter().map(|(_, visible)| *visible).collect::<Vec<_>>(), vec![true, true, false]);
        }
    }

    #[test]
    fn independent_created_layers_do_not_disable_stroke_prediction_or_sparse_replay() {
        let mut host = session(8);
        let layer = host.active().unwrap().active_layer.unwrap();
        let mut guest = Session::new();
        guest.add_document((*host.active().unwrap().doc).clone(), None);
        guest.execute("collab.room.join", json!({"peer":"guest","code":"TEST01"})).unwrap();
        guest.collaboration.document_id = guest.active().map(|state| state.doc.id); // Simulate completed transport admission.
        guest
            .collaboration_submit(Operation::StrokeBegin {
                stroke: Box::new(StrokeStart {
                    id: "layer-independent".into(),
                    layer,
                    zoom: 1.0,
                    brush: BrushSettings { size: 10.0, ..Default::default() },
                    selection: Vec::new(),
                    lock_transparency: false,
                }),
            })
            .unwrap();
        host.collaboration_accept("guest", guest.collaboration.outbox.last().unwrap().clone()).unwrap();
        guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
        for (id, params) in [
            ("shape.create", json!({"kind":"rectangle","rect":[2,2,12,12]})),
            ("type.create", json!({"text":"Peer","x":2,"y":20,"size":12})),
            ("layer.new.layer", json!({"name":"peer layer"})),
        ] {
            host.execute(id, params).unwrap();
            assert!(host.collaboration.commands.last().unwrap().independent_strokes, "{id}");
            let mut hostile = host.collaboration.canonical_outbox.last().unwrap().delta.clone().unwrap();
            hostile.manifest.document.size.width += 1;
            assert!(!independent_layer_creation(id, Some(&hostile)), "dimension changes require full replay");
            let mut hostile = host.collaboration.canonical_outbox.last().unwrap().delta.clone().unwrap();
            hostile.manifest.document.layers.iter_mut().find(|candidate| candidate.id == layer.0).unwrap().opacity = 0.2;
            assert!(!independent_layer_creation(id, Some(&hostile)), "existing layer changes require full replay");

            guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
            assert!(guest.collaboration.predicted_stroke.is_some(), "independent layers retain prediction");
        }
        guest
            .collaboration_submit(Operation::StrokeChunk { id: "layer-independent".into(), sequence: 0, points: vec![StrokePoint::new(30.0, 30.0, 0.8)] })
            .unwrap();
        host.collaboration_accept("guest", guest.collaboration.outbox.last().unwrap().clone()).unwrap();
        assert!(host.collaboration.operation_damage.is_some(), "independent creation avoids full journal replay");
        assert_eq!(guest.active().unwrap().doc.layer(layer).unwrap().surface(), host.active().unwrap().doc.layer(layer).unwrap().surface());
        guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
        let mut oracle = (*host.active().unwrap().doc).clone();
        host.collaboration.rebuild_document(&mut oracle).unwrap();
        assert_eq!(oracle.layer(layer).unwrap().surface(), host.active().unwrap().doc.layer(layer).unwrap().surface());
        guest.collaboration_submit(Operation::StrokeEnd { id: "layer-independent".into(), sequence: 1 }).unwrap();
        host.collaboration_accept("guest", guest.collaboration.outbox.last().unwrap().clone()).unwrap();
        guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
        for operation in [Operation::Undo, Operation::Redo] {
            emit(&mut host, "guest", operation);
            let event = host.collaboration.canonical_outbox.last().unwrap();
            assert!(event.delta.is_none(), "independent creation history permits sparse stroke undo/redo");
            assert!(host.collaboration.operation_damage.is_some());
            guest.collaboration_receive(event).unwrap();
            assert_eq!(guest.active().unwrap().doc.layer(layer).unwrap().surface(), host.active().unwrap().doc.layer(layer).unwrap().surface());
        }
    }

    #[test]
    fn guest_prediction_rebases_pressure_alpha_erase_and_end_echo() {
        for depth in [8, 16, 32] {
            for erase in [false, true] {
                for locked in [false, true] {
                    let mut host = session(depth);
                    let layer = host.active().unwrap().active_layer.unwrap();
                    let state = host.active_mut().unwrap();
                    let layer_data = std::sync::Arc::make_mut(&mut state.doc).layer_mut(layer).unwrap();
                    for y in 0..64 {
                        for x in 0..64 {
                            layer_data.surface_mut().unwrap().write_pixel(x, y, &[0.2, 0.3, 0.4, 0.7]);
                        }
                    }
                    layer_data.locks.transparency = locked;
                    let mut guest = Session::new();
                    guest.add_document((*host.active().unwrap().doc).clone(), None);
                    guest.execute("collab.room.join", json!({"peer":"guest","code":"TEST01"})).unwrap();
                    guest.collaboration.document_id = guest.active().map(|state| state.doc.id); // Simulate completed transport admission.
                    let private_selection = guest.active().unwrap().doc.selection.clone();
                    let start = StrokeStart {
                        id: "prediction".into(),
                        layer,
                        zoom: 1.7,
                        brush: BrushSettings {
                            size: 13.0,
                            color: [0.8, 0.1, 0.2, 1.0],
                            opacity: 0.45,
                            pressure_opacity: true,
                            pressure_size: true,
                            seed: 998,
                            noise: true,
                            erase,
                            ..Default::default()
                        },
                        selection: (0..64).map(|y| SelectionRun { x: 0, y, coverage: (0..64).map(|x| if x < 32 { 0.35 } else { 0.85 }).collect() }).collect(),
                        lock_transparency: locked,
                    };
                    guest.collaboration_submit(Operation::StrokeBegin { stroke: Box::new(start) }).unwrap();
                    let message = guest.collaboration.outbox.last().unwrap().clone();
                    host.collaboration_accept("guest", message).unwrap();
                    guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                    begin(&mut host, "peer", "peer-prediction", [0.1, 0.8, 0.3, 1.0]);
                    guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                    let mut visible_chunks = 0;
                    for sequence in 0..8 {
                        let mut point = StrokePoint::new(16.0 + f64::from(sequence) * 4.0, 25.0, 0.2 + sequence as f32 * 0.09);
                        point.tilt_x = 0.25;
                        point.tilt_y = -0.1;
                        point.rotation = 0.4;
                        let before = pixels(&guest);
                        guest.collaboration_submit(Operation::StrokeChunk { id: "prediction".into(), sequence, points: vec![point] }).unwrap();
                        if !(erase && locked) && pixels(&guest) != before {
                            visible_chunks += 1;
                        }
                        let message = guest.collaboration.outbox.last().unwrap().clone();
                        chunk(&mut host, "peer", "peer-prediction", sequence, vec![StrokePoint::new(18.0 + f64::from(sequence) * 4.0, 26.0, 0.8)]);
                        guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                        host.collaboration_accept("guest", message).unwrap();
                        assert_eq!(pixels(&guest), pixels(&host), "rebased own prediction preserves later peer order");
                        guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                        assert_eq!(pixels(&guest), pixels(&host), "own echo cannot double alpha");
                    }
                    assert!(erase && locked || visible_chunks > 0, "own pressure input must appear before echo");
                    guest.collaboration_submit(Operation::StrokeEnd { id: "prediction".into(), sequence: 8 }).unwrap();
                    let ending = pixels(&guest);
                    assert!(guest.collaboration.predicted_stroke.is_some(), "release retains prediction until end echo");
                    let message = guest.collaboration.outbox.last().unwrap().clone();
                    host.collaboration_accept("guest", message).unwrap();
                    assert_eq!(ending, pixels(&host));
                    guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                    assert!(guest.collaboration.predicted_stroke.is_none());
                    assert_eq!(pixels(&guest), pixels(&host));
                    assert_eq!(guest.active().unwrap().doc.selection, private_selection);
                    end(&mut host, "peer", "peer-prediction", 8);
                    guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                    emit(&mut host, "guest", Operation::Undo);
                    guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                    assert_eq!(pixels(&guest), pixels(&host));
                    emit(&mut host, "guest", Operation::Redo);
                    guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                    assert_eq!(pixels(&guest), pixels(&host));
                }
            }
        }
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
        guest.collaboration.document_id = guest.active().map(|state| state.doc.id); // Simulate completed transport admission.
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
            guest.collaboration.document_id = guest.active().map(|state| state.doc.id); // Simulate completed transport admission.
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
    fn staged_checkpoint_dense_atomic_cancel_and_fifo_all_depths() {
        for depth in [8, 16, 32] {
            let mut host = session(depth);
            for n in 0..128 {
                let id = format!("staged-{n}");
                begin(&mut host, "host", &id, [0.2, 0.5, 0.8, 0.6]);
                let points = (0..24)
                    .map(|i| {
                        let mut p = StrokePoint::new(8.0 + f64::from(i), 8.0 + f64::from(n % 36), 0.2 + f64::from(i % 8) as f32 / 10.0);
                        p.tilt_x = 20.0;
                        p.rotation = 45.0;
                        p
                    })
                    .collect();
                chunk(&mut host, "host", &id, 0, points);
                end(&mut host, "host", &id, 1);
            }
            host.execute("edit.undo", json!({})).unwrap();
            let checkpoint = host.collaboration_checkpoint().unwrap();
            let mut guest = session(depth);
            let local = pixels(&guest);
            let original_id = guest.active().unwrap().doc.id;
            let mut cancelled = CheckpointRestore::new(checkpoint.clone()).unwrap();
            for _ in 0..20 {
                assert!(!cancelled.step(1).unwrap().done);
            }
            drop(cancelled);
            assert_eq!(pixels(&guest), local);
            assert_eq!(guest.docs.len(), 1);
            let mut job = CheckpointRestore::new(checkpoint).unwrap();
            assert!(
                guest
                    .collaboration_commit_checkpoint(
                        CheckpointRestore::new(host.collaboration_checkpoint().unwrap()).unwrap(),
                        host.collaboration.room.clone().unwrap()
                    )
                    .is_err()
            );
            let mut steps = 0;
            let mut worst = 0.0f64;
            while !job.progress().done {
                let start = std::time::Instant::now();
                job.step(1).unwrap();
                worst = worst.max(start.elapsed().as_secs_f64() * 1000.0);
                steps += 1;
                assert_eq!(guest.active().unwrap().doc.id, original_id);
                assert_eq!(pixels(&guest), local);
                assert!(steps < 100000);
            }
            let expected = pixels(&host);
            let history = host.collaboration.own_history();
            host.collaboration.canonical_outbox.clear();
            begin(&mut host, "peer", "post-checkpoint", [0.8, 0.1, 0.3, 0.5]);
            chunk(&mut host, "peer", "post-checkpoint", 0, vec![StrokePoint::new(30.0, 30.0, 0.8)]);
            end(&mut host, "peer", "post-checkpoint", 1);
            let events = std::mem::take(&mut host.collaboration.canonical_outbox);
            let id = guest.collaboration_commit_checkpoint(job, host.collaboration.room.clone().unwrap()).unwrap();
            assert_ne!(id, original_id);
            assert_eq!(guest.docs.len(), 2);
            assert_eq!(pixels(&guest), expected);
            assert_eq!(guest.collaboration.own_history(), history);
            for event in events {
                guest.collaboration_receive(&event).unwrap();
            }
            assert_eq!(pixels(&guest), pixels(&host));
            assert_eq!(guest.collaboration.applied_revision, host.collaboration.applied_revision);
            println!("staged checkpoint depth={depth} steps={steps} max_step_ms={worst:.3}");
        }
    }
    #[test]
    fn staged_checkpoint_guest_local_edits_survive_cancel_and_atomic_commit() {
        let host = session(8);
        let mut guest = session(8);
        let local_id = guest.active().unwrap().doc.id;
        let mut room = host.collaboration.room.clone().unwrap();
        room.local_peer = "guest".into();
        guest.collaboration.room = Some(room.clone());
        guest.collaboration.document_id = None;
        let pending = CheckpointRestore::new(host.collaboration_checkpoint().unwrap()).unwrap();
        guest.execute("layer.new.layer", json!({"name":"Private during admission"})).unwrap();
        guest.execute("edit.fill", json!({"color":[0.3,0.6,0.8,1.0]})).unwrap();
        let private = guest.active().unwrap().doc.clone();
        let private_revision = guest.active().unwrap().revision;
        assert!(guest.collaboration.outbox.is_empty());
        drop(pending);
        assert_eq!(guest.active().unwrap().doc.id, local_id);
        assert_eq!(guest.active().unwrap().revision, private_revision);
        let mut restore = CheckpointRestore::new(host.collaboration_checkpoint().unwrap()).unwrap();
        while !restore.step(1).unwrap().done {}
        guest.collaboration_commit_checkpoint(restore, room).unwrap();
        assert_eq!(guest.docs.len(), 2);
        assert_eq!(guest.docs[0].doc.id, local_id);
        assert_eq!(guest.docs[0].revision, private_revision);
        assert_eq!(guest.docs[0].doc.layers.len(), private.layers.len());
        assert_eq!(
            guest.docs[0].doc.layer(guest.docs[0].active_layer.unwrap()).unwrap().surface(),
            private.layer(guest.docs[0].active_layer.unwrap()).unwrap().surface()
        );
    }
    #[test]
    fn staged_checkpoint_errors_do_not_mutate_session() {
        let mut host = session(8);
        begin(&mut host, "peer", "duplicate", [1.0, 0.0, 0.0, 0.5]);
        chunk(&mut host, "peer", "duplicate", 0, vec![StrokePoint::new(8.0, 8.0, 0.5)]);
        end(&mut host, "peer", "duplicate", 1);
        let mut checkpoint = host.collaboration_checkpoint().unwrap();
        checkpoint.entries.push(checkpoint.entries[0].clone());
        let mut guest = session(8);
        let before = pixels(&guest);
        let revision = guest.collaboration.applied_revision;
        let mut job = CheckpointRestore::new(checkpoint).unwrap();
        while job.step(1).is_ok() {}
        assert!(guest.collaboration_commit_checkpoint(job, host.collaboration.room.clone().unwrap()).is_err());
        assert_eq!(pixels(&guest), before);
        assert_eq!(guest.collaboration.applied_revision, revision);
        assert_eq!(guest.docs.len(), 1);
    }
    #[test]
    fn staged_checkpoint_hidden_missing_layer_ink_survives_peer_redo() {
        for depth in [8, 16, 32] {
            let mut host = session(depth);
            host.execute("layer.new.layer", json!({"name":"Hidden prerequisite"})).unwrap();
            begin(&mut host, "peer", "hidden-ink", [0.2, 0.7, 0.3, 0.6]);
            chunk(&mut host, "peer", "hidden-ink", 0, vec![StrokePoint::new(20.0, 20.0, 0.7)]);
            end(&mut host, "peer", "hidden-ink", 1);
            emit(&mut host, "peer", Operation::Undo);
            host.execute("edit.undo", json!({})).unwrap();
            let mut restored = session(depth);
            restored.collaboration_install_checkpoint(host.collaboration_checkpoint().unwrap()).unwrap();
            assert_eq!(pixels(&restored), pixels(&host));
            emit(&mut host, "peer", Operation::Redo);
            emit(&mut restored, "peer", Operation::Redo);
            assert_eq!(pixels(&restored), pixels(&host));
        }
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
    fn overlapping_suffix_replay_matches_full_journal_with_erase_and_private_undo() {
        for depth in [8, 16, 32] {
            let mut host = session(depth);
            host.execute("layer.new.layer", json!({"name":"Overlap"})).unwrap();
            begin(&mut host, "host", "earlier", [0.9, 0.2, 0.1, 1.0]);
            let layer = host.active().unwrap().active_layer.unwrap();
            let brush = photocraft_paint::BrushSettings { size: 12.0, erase: true, opacity: 0.7, pressure_opacity: true, ..Default::default() };
            emit(
                &mut host,
                "peer",
                Operation::StrokeBegin {
                    stroke: Box::new(StrokeStart { id: "later".into(), layer, brush, zoom: 1.0, selection: Vec::new(), lock_transparency: false }),
                },
            );
            let mut oracle = session(depth);
            for step in 0..16 {
                for (peer, id) in [("host", "earlier"), ("peer", "later")] {
                    chunk(&mut host, peer, id, step, vec![StrokePoint::new(10.0 + f64::from(step) * 2.0, 22.0, 0.2 + step as f32 * 0.04)]);
                    oracle.collaboration_install_checkpoint(host.collaboration_checkpoint().unwrap()).unwrap();
                    assert_eq!(pixels(&host), pixels(&oracle), "depth {depth}, step {step}, {peer}");
                }
            }
            end(&mut host, "host", "earlier", 16);
            end(&mut host, "peer", "later", 16);
            let original = pixels(&host);
            host.execute("collab.undo", json!({})).unwrap();
            assert_ne!(pixels(&host), original);
            host.execute("collab.redo", json!({})).unwrap();
            assert_eq!(pixels(&host), original);
        }
    }

    #[test]
    fn disjoint_live_stroke_prefix_updates_before_it_grows_into_changed_tile() {
        for depth in [8, 16, 32] {
            let mut host = session(depth);
            begin(&mut host, "host", "earlier", [0.8, 0.2, 0.1, 1.0]);
            begin(&mut host, "peer", "later", [0.1, 0.2, 0.8, 1.0]);
            chunk(&mut host, "host", "earlier", 0, vec![StrokePoint::new(8.0, 18.0, 1.0)]);
            chunk(&mut host, "peer", "later", 0, vec![StrokePoint::new(180.0, 18.0, 0.5)]);
            chunk(&mut host, "host", "earlier", 1, vec![StrokePoint::new(28.0, 18.0, 0.8)]);
            chunk(&mut host, "peer", "later", 1, vec![StrokePoint::new(28.0, 18.0, 0.7)]);
            let actual = pixels(&host);
            let mut oracle = (*host.active().unwrap().doc).clone();
            host.collaboration.rebuild_document(&mut oracle).unwrap();
            let layer = host.active().unwrap().active_layer.unwrap();
            assert_eq!(actual, oracle.layer(layer).unwrap().surface().unwrap().read_region(Rect::new(0, 0, 64, 64)), "depth {depth}");
        }
    }

    #[test]
    fn peer_undo_refreshes_live_prefix_before_more_overlapping_chunks() {
        for depth in [8, 16, 32] {
            let mut host = session(depth);
            begin(&mut host, "host", "finished", [0.8, 0.2, 0.1, 1.0]);
            chunk(&mut host, "host", "finished", 0, vec![StrokePoint::new(20.0, 20.0, 1.0)]);
            end(&mut host, "host", "finished", 1);
            begin(&mut host, "peer", "live", [0.1, 0.2, 0.8, 1.0]);
            chunk(&mut host, "peer", "live", 0, vec![StrokePoint::new(20.0, 20.0, 0.5)]);
            let mut guest = session(depth);
            guest.collaboration_install_checkpoint(host.collaboration_checkpoint().unwrap()).unwrap();
            host.execute("collab.undo", json!({})).unwrap();
            guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
            chunk(&mut host, "peer", "live", 1, vec![StrokePoint::new(22.0, 20.0, 0.8)]);
            guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
            assert_eq!(pixels(&host), pixels(&guest), "depth {depth}");
        }
    }

    #[test]
    fn sparse_undo_redo_preserves_growing_earlier_peer_and_checkpoint_hidden_prefixes() {
        for depth in [8, 16, 32] {
            let mut host = session(depth);
            begin(&mut host, "early", "early", [0.1, 0.2, 0.8, 1.0]);
            chunk(&mut host, "early", "early", 0, vec![StrokePoint::new(4.0, 20.0, 0.4)]);
            begin(&mut host, "host", "mine", [0.8, 0.2, 0.1, 1.0]);
            chunk(&mut host, "host", "mine", 0, vec![StrokePoint::new(20.0, 20.0, 0.7)]);
            end(&mut host, "host", "mine", 1);
            begin(&mut host, "late", "late", [0.1, 0.8, 0.2, 1.0]);
            chunk(&mut host, "late", "late", 0, vec![StrokePoint::new(18.0, 20.0, 0.6), StrokePoint::new(54.0, 20.0, 0.6)]);
            end(&mut host, "late", "late", 1);
            chunk(&mut host, "early", "early", 1, vec![StrokePoint::new(50.0, 20.0, 0.4)]);
            let before = pixels(&host);
            let seen = host.active().unwrap().revision;
            host.execute("collab.undo", json!({})).unwrap();
            assert!(host.collaboration.canonical_outbox.last().unwrap().delta.is_none());
            let state = host.active().unwrap();
            let damage = host.collaboration.damage_since(state.doc.id, seen, state.revision).unwrap();
            assert!(!damage.is_empty());
            assert!(damage.x1 < 40, "undo damage must not overwrite an unrelated peer region");
            let after = pixels(&host);
            assert_eq!(&before[(20 * 64 + 50) * 4..(20 * 64 + 51) * 4], &after[(20 * 64 + 50) * 4..(20 * 64 + 51) * 4]);
            let mut oracle = session(depth);
            oracle.collaboration_install_checkpoint(host.collaboration_checkpoint().unwrap()).unwrap();
            assert_eq!(pixels(&host), pixels(&oracle), "undo depth {depth}");
            let mut guest = session(depth);
            guest.collaboration_install_checkpoint(host.collaboration_checkpoint().unwrap()).unwrap();
            chunk(&mut host, "early", "early", 2, vec![StrokePoint::new(20.0, 20.0, 1.0)]);
            guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
            host.execute("collab.redo", json!({})).unwrap();
            guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
            assert!(host.collaboration.canonical_outbox.last().unwrap().delta.is_none());
            assert_eq!(pixels(&host), pixels(&guest), "checkpoint redo depth {depth}");
            oracle.collaboration_install_checkpoint(host.collaboration_checkpoint().unwrap()).unwrap();
            assert_eq!(pixels(&host), pixels(&oracle), "redo depth {depth}");
        }
    }

    #[test]
    fn pending_predicted_transform_or_gradient_requires_full_damage() {
        for (id, params) in
            [("edit.transform", json!({"matrix":[1,0,0,1,3,4],"interpolation":"nearest"})), ("paint.gradient", json!({"from":[0,0],"to":[63,0]}))]
        {
            let mut host = session(8);
            begin(&mut host, "host", "ink", [1.0, 0.1, 0.3, 1.0]);
            chunk(&mut host, "host", "ink", 0, vec![StrokePoint::new(12.0, 10.0, 0.7)]);
            end(&mut host, "host", "ink", 1);
            begin(&mut host, "peer", "next", [0.1, 0.3, 1.0, 1.0]);
            let edit = crate::collab_resources::prepare_command(&host, id, &params).unwrap();
            host.collaboration.pending_commands.push(edit);
            let seen = host.active().unwrap().revision;
            chunk(&mut host, "peer", "next", 0, vec![StrokePoint::new(20.0, 10.0, 0.7)]);
            let state = host.active().unwrap();
            assert_eq!(state.last_damage, None, "{id}");
            assert_eq!(host.collaboration.damage_since(state.doc.id, seen, state.revision), None, "{id}");
        }
    }

    #[test]
    fn damage_ledger_unions_contiguous_revisions_and_rejects_gaps() {
        let mut room = Collaboration::default();
        let id = photocraft_doc::DocId(42);
        room.document_id = Some(id);
        room.record_damage(id, 2, Some(Rect::new(1, 2, 3, 4)));
        room.record_damage(id, 3, Some(Rect::EMPTY));
        room.record_damage(id, 4, Some(Rect::new(8, 9, 12, 14)));
        assert_eq!(room.damage_since(id, 1, 4), Some(Rect::new(1, 2, 12, 14)));
        assert_eq!(room.damage_since(id, 0, 4), None);
        assert_eq!(room.damage_since(id, 4, 4), Some(Rect::EMPTY));
        room.record_damage(id, 5, None);
        assert_eq!(room.damage_since(id, 1, 5), None);
        room.record_damage(id, 7, Some(Rect::EMPTY));
        assert_eq!(room.damage_since(id, 5, 7), None);
        for revision in 8..300 {
            room.record_damage(id, revision, Some(Rect::EMPTY));
        }
        assert_eq!(room.damage_ledger.len(), 256);
        assert_eq!(room.damage_since(id, 7, 299), None);
    }

    #[test]
    #[ignore = "aged active stroke with 100 later overlapping strokes; percentile evidence"]
    fn collaboration_aged_overlap_latency_benchmark() {
        fn stats(samples: &mut [f64]) -> serde_json::Value {
            samples.sort_by(f64::total_cmp);
            let at = |p: usize| samples.get(samples.len().saturating_sub(1) * p / 100).copied().unwrap_or(0.0);
            json!({"p50_ms":at(50),"p95_ms":at(95),"p99_ms":at(99),"max_ms":samples.last().copied().unwrap_or(0.0),"samples":samples.len()})
        }
        for depth in [8, 16, 32] {
            let mut host = session(depth);
            host.execute("layer.new.layer", json!({"name":"Aged overlap"})).unwrap();
            begin(&mut host, "slow", "aged", [0.9, 0.2, 0.1, 1.0]);
            chunk(&mut host, "slow", "aged", 0, vec![StrokePoint::new(18.0, 22.0, 0.4)]);
            for n in 0..100 {
                let id = format!("newer-{n}");
                let layer = host.active().unwrap().active_layer.unwrap();
                let brush = BrushSettings {
                    size: 28.0,
                    opacity: 0.5,
                    erase: n % 3 == 0,
                    pressure_opacity: true,
                    noise: true,
                    seed: 234 + n as u64,
                    color: [0.1, 0.3, 0.8, 1.0],
                    ..Default::default()
                };
                emit(
                    &mut host,
                    "rapid",
                    Operation::StrokeBegin {
                        stroke: Box::new(StrokeStart { id: id.clone(), layer, brush, zoom: 0.75, selection: Vec::new(), lock_transparency: false }),
                    },
                );
                chunk(&mut host, "rapid", &id, 0, vec![StrokePoint::new(20.0 + f64::from(n % 11), 22.0, 0.4 + 0.01 * (n % 30) as f32)]);
                end(&mut host, "rapid", &id, 1);
            }
            let mut guest = session(depth);
            guest.collaboration_install_checkpoint(host.collaboration_checkpoint().unwrap()).unwrap();
            host.collaboration.canonical_outbox.clear();
            let mut host_ms = Vec::new();
            let mut guest_ms = Vec::new();
            for sequence in 1..65 {
                let start = std::time::Instant::now();
                chunk(&mut host, "slow", "aged", sequence, vec![StrokePoint::new(14.0 + f64::from(sequence % 24), 22.0, 0.3 + 0.02 * (sequence % 24) as f32)]);
                host_ms.push(start.elapsed().as_secs_f64() * 1000.0);
                let start = std::time::Instant::now();
                guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                guest_ms.push(start.elapsed().as_secs_f64() * 1000.0);
            }
            end(&mut host, "slow", "aged", 65);
            guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
            assert_eq!(pixels(&host), pixels(&guest));
            let mut oracle = session(depth);
            oracle.collaboration_install_checkpoint(host.collaboration_checkpoint().unwrap()).unwrap();
            assert_eq!(pixels(&host), pixels(&oracle));
            let original = pixels(&host);
            emit(&mut host, "slow", Operation::Undo);
            guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
            assert_eq!(pixels(&host), pixels(&guest));
            assert!(host.collaboration.strokes.iter().filter(|stroke| stroke.owner == "rapid").all(|stroke| stroke.visible));
            emit(&mut host, "slow", Operation::Redo);
            guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
            assert_eq!(pixels(&host), original);
            assert_eq!(pixels(&host), pixels(&guest));
            let result =
                json!({"depth":depth,"later_completed_overlapping_strokes":100,"host":stats(&mut host_ms),"guest":stats(&mut guest_ms),"converged":true});
            println!("AGED_OVERLAP_BENCH {result}");
        }
    }
    #[test]
    #[ignore = "same-source overlap latency benchmark; prints percentile evidence"]
    fn collaboration_overlap_latency_benchmark() {
        fn percentiles(samples: &mut [f64]) -> serde_json::Value {
            samples.sort_by(f64::total_cmp);
            let at = |p: usize| samples.get(samples.len().saturating_sub(1) * p / 100).copied().unwrap_or(0.0);
            json!({"p50_ms":at(50),"p95_ms":at(95),"p99_ms":at(99),"samples":samples.len()})
        }
        for depth in [8, 16, 32] {
            let mut host = session(depth);
            host.execute("layer.new.layer", json!({"name":"Overlap layer"})).unwrap();
            for n in 0..96 {
                let id = format!("history-{n}");
                begin(&mut host, "old", &id, [0.1, 0.3, 0.8, 1.0]);
                chunk(&mut host, "old", &id, 0, vec![StrokePoint::new(f64::from(n % 32), f64::from(n % 24), 0.8)]);
                end(&mut host, "old", &id, 1);
            }
            let mut guest = session(depth);
            guest.collaboration.document_id = guest.active().map(|state| state.doc.id);
            guest.collaboration_install_checkpoint(host.collaboration_checkpoint().unwrap()).unwrap();
            host.collaboration.canonical_outbox.clear();
            let mut host_ms = Vec::new();
            let mut guest_ms = Vec::new();
            for round in 0..8 {
                for peer in ["draw", "erase"] {
                    let id = format!("{peer}-{round}");
                    let layer = host.active().unwrap().active_layer.unwrap();
                    emit(
                        &mut host,
                        peer,
                        Operation::StrokeBegin {
                            stroke: Box::new(StrokeStart {
                                id,
                                layer,
                                zoom: 0.75,
                                brush: BrushSettings {
                                    size: 28.0,
                                    opacity: 0.6,
                                    erase: peer == "erase",
                                    seed: 234,
                                    pressure_opacity: true,
                                    color: [0.9, 0.2, 0.1, 1.0],
                                    ..Default::default()
                                },
                                selection: Vec::new(),
                                lock_transparency: false,
                            }),
                        },
                    );
                    guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                }
                for sequence in 0..16 {
                    for peer in ["draw", "erase"] {
                        let id = format!("{peer}-{round}");
                        let instant = std::time::Instant::now();
                        chunk(
                            &mut host,
                            peer,
                            &id,
                            sequence,
                            vec![StrokePoint::new(12.0 + f64::from(sequence), 18.0 + f64::from(round % 3), 0.25 + 0.04 * sequence as f32)],
                        );
                        host_ms.push(instant.elapsed().as_secs_f64() * 1000.0);
                        let instant = std::time::Instant::now();
                        guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                        guest_ms.push(instant.elapsed().as_secs_f64() * 1000.0);
                    }
                }
                for peer in ["draw", "erase"] {
                    end(&mut host, peer, &format!("{peer}-{round}"), 16);
                    guest.collaboration_receive(host.collaboration.canonical_outbox.last().unwrap()).unwrap();
                }
            }
            assert_eq!(pixels(&host), pixels(&guest));
            let result = json!({"depth":depth,"history_strokes":96,"active_peers":2,"host":percentiles(&mut host_ms),"guest":percentiles(&mut guest_ms),"converged":true});
            println!("OVERLAP_BENCH {result}");
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
