//! Off-session checkpoint restoration. Each step advances bounded brush/sample/tile work.
//! Baseline decoding and a semantic command remain indivisible operations.
use super::*;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct CheckpointProgress {
    pub completed: usize,
    pub total: usize,
    pub done: bool,
    pub work_units: usize,
}
struct ArchiveStroke {
    start: Box<StrokeStart>,
    owner: String,
    chunks: Vec<Vec<photocraft_paint::StrokePoint>>,
    chunk: usize,
    last: Option<photocraft_paint::StrokePoint>,
    bounds: Rect,
    ended: bool,
    visible: bool,
}
struct RenderStroke {
    index: usize,
    chunk: usize,
    point: usize,
    selection_run: usize,
    selection_offset: usize,
    finished: bool,
    tiles: Vec<(i32, i32)>,
    tile: usize,
}
/// Owns all temporary state; cancellation is simply dropping this value.
/// The Session and its currently open documents are never touched until commit.
pub struct CheckpointRestore {
    document: Document,
    restored: Collaboration,
    entries: VecDeque<ArchivedEdit>,
    total_entries: usize,
    archived: usize,
    stroke_ids: BTreeSet<String>,
    command_ids: BTreeSet<String>,
    nonce: BTreeMap<String, u64>,
    archive_stroke: Option<ArchiveStroke>,
    render_stroke: Option<RenderStroke>,
    replay: usize,
    revision: u64,
    work_units: usize,
    failed: bool,
}
impl CheckpointRestore {
    pub fn new(checkpoint: JournalCheckpoint) -> crate::Result<Self> {
        let bytes = serialized_size(&checkpoint).map_err(error)?;
        if checkpoint.entries.len() > 4096
            || checkpoint.undo_floor.len() > 4096
            || checkpoint.undo_floor.values().any(|floor| *floor > checkpoint.entries.len())
            || bytes > 480 * 1024 * 1024
        {
            return Err(EngineError::Other("checkpoint exceeds history budget".into()));
        }
        let mut document = photocraft_format::load_from_bytes_with(
            &checkpoint.baseline,
            &photocraft_format::LoadOptions {
                max_manifest_bytes: 8 << 20,
                max_blob_bytes: 64 << 20,
                max_total_bytes: 512 << 20,
                preserve_ids: true,
                ..Default::default()
            },
        )
        .map_err(|e| EngineError::Other(e.to_string()))?;
        document.selection = None;
        document.quick_mask = None;
        let total_entries = checkpoint.entries.len();
        let restored = Collaboration {
            baseline: Some(document.clone()),
            baseline_bytes: Some(checkpoint.baseline),
            history_bytes: bytes,
            redo: checkpoint.redo,
            undo_floor: checkpoint.undo_floor,
            ..Default::default()
        };
        Ok(Self {
            document,
            restored,
            entries: checkpoint.entries.into(),
            total_entries,
            archived: 0,
            stroke_ids: BTreeSet::new(),
            command_ids: BTreeSet::new(),
            nonce: BTreeMap::new(),
            archive_stroke: None,
            render_stroke: None,
            replay: 0,
            revision: checkpoint.revision,
            work_units: 0,
            failed: false,
        })
    }
    pub fn progress(&self) -> CheckpointProgress {
        CheckpointProgress {
            completed: self.archived.saturating_add(self.replay),
            total: self.total_entries.saturating_mul(2),
            done: !self.failed && self.archived == self.total_entries && self.replay == self.total_entries,
            work_units: self.work_units,
        }
    }
    /// One unit validates an archive chunk, renders at most eight points, writes at
    /// most 256 selection samples, or composites one 64×64 coverage tile.
    /// Callers must also enforce a wall-clock budget between units. One baseline
    /// decode or semantic command may exceed that budget and is reported separately.
    pub fn step(&mut self, work_units: usize) -> crate::Result<CheckpointProgress> {
        if self.failed {
            return Err(EngineError::Other("checkpoint restoration previously failed".into()));
        }
        for _ in 0..work_units.clamp(1, 64) {
            if self.progress().done {
                break;
            }
            if let Err(e) = self.advance() {
                self.failed = true;
                return Err(e);
            }
            self.work_units = self.work_units.saturating_add(1);
        }
        Ok(self.progress())
    }
    fn accept(&mut self, owner: &str, operation: Operation) -> crate::Result<()> {
        let next = self.nonce.entry(owner.into()).or_default();
        *next = next.saturating_add(1);
        let mut candidate = self.restored.sequencer.candidate();
        candidate.accept(owner, ClientMessage { version: PROTOCOL_VERSION, peer: owner.into(), nonce: *next, operation }).map_err(error)?;
        self.restored.sequencer = candidate.candidate();
        Ok(())
    }
    fn advance(&mut self) -> crate::Result<()> {
        if let Some(mut stroke) = self.archive_stroke.take() {
            if let Some(points) = stroke.chunks.get(stroke.chunk) {
                self.accept(
                    &stroke.owner,
                    Operation::StrokeChunk {
                        id: stroke.start.id.clone(),
                        sequence: u32::try_from(stroke.chunk).map_err(|e| EngineError::Other(e.to_string()))?,
                        points: points.clone(),
                    },
                )?;
                ChunkBudget::new(&stroke.start.brush).validate(stroke.last, points).map_err(error)?;
                let radius = f64::from(stroke.start.brush.size) * 2.0;
                for point in points {
                    if let Some(last) = stroke.last
                        && (point.time < last.time || point.time - last.time > 1000.0 || (point.x - last.x).hypot(point.y - last.y) > 4096.0)
                    {
                        return Err(EngineError::Other("invalid checkpoint sample gap".into()));
                    }
                    stroke.bounds = stroke.bounds.union(&Rect::new(
                        (point.x - radius).floor() as i32,
                        (point.y - radius).floor() as i32,
                        (point.x + radius).ceil() as i32,
                        (point.y + radius).ceil() as i32,
                    ));
                    stroke.last = Some(*point);
                }
                if i64::from(stroke.bounds.width()).saturating_mul(i64::from(stroke.bounds.height())) > 16_777_216 {
                    return Err(EngineError::Other("checkpoint stroke exceeds rendering budget".into()));
                }
                stroke.chunk = stroke.chunk.saturating_add(1);
                self.archive_stroke = Some(stroke);
                return Ok(());
            }
            if stroke.ended {
                self.accept(
                    &stroke.owner,
                    Operation::StrokeEnd {
                        id: stroke.start.id.clone(),
                        sequence: u32::try_from(stroke.chunks.len()).map_err(|e| EngineError::Other(e.to_string()))?,
                    },
                )?;
                if !self.restored.strokes.iter().any(|s| s.owner == stroke.owner && !s.ended) {
                    self.restored.sequencer.forget_peer(&stroke.owner);
                }
            }
            let format = self.document.pixel_format();
            self.restored.strokes.push(ReplayStroke {
                renderer: StrokeRenderer::new(&stroke.start.brush, Some(format), stroke.start.zoom),
                selection: (!stroke.start.selection.is_empty()).then(|| {
                    Surface::new(photocraft_color::PixelFormat::new(photocraft_color::ColorMode::Grayscale, photocraft_color::SampleType::F32, false))
                }),
                start: *stroke.start,
                owner: stroke.owner,
                pre: Surface::new(format),
                last: stroke.last,
                before_document: None,
                chunks: stroke.chunks,
                ended: stroke.ended,
                visible: stroke.visible,
            });
            self.restored.journal.push(JournalEntry::Stroke(self.restored.strokes.len().saturating_sub(1)));
            self.archived = self.archived.saturating_add(1);
            return Ok(());
        }
        if let Some(entry) = self.entries.pop_front() {
            match entry {
                ArchivedEdit::Command { edit, owner, visible } => {
                    validate(&Operation::Command { edit: edit.clone() }).map_err(error)?;
                    if owner.is_empty() || owner.len() > 128 || !self.command_ids.insert(edit.key.clone()) {
                        return Err(EngineError::Other("invalid checkpoint command identity".into()));
                    }
                    self.restored.commands.push(ReplayCommand {
                        independent_strokes: independent_layer_creation(&edit.id, edit.outcome.as_ref()),
                        edit: *edit,
                        owner,
                        visible,
                        created: Vec::new(),
                    });
                    self.restored.journal.push(JournalEntry::Command(self.restored.commands.len().saturating_sub(1)));
                    self.archived = self.archived.saturating_add(1);
                }
                ArchivedEdit::Stroke { start, owner, chunks, ended, visible } => {
                    if !self.stroke_ids.insert(start.id.clone()) {
                        return Err(EngineError::Other("duplicate checkpoint stroke".into()));
                    }
                    self.accept(&owner, Operation::StrokeBegin { stroke: start.clone() })?;
                    self.archive_stroke = Some(ArchiveStroke { start, owner, chunks, chunk: 0, last: None, bounds: Rect::EMPTY, ended, visible });
                }
            }
            return Ok(());
        }
        if let Some(mut render) = self.render_stroke.take() {
            let stroke = self.restored.strokes.get_mut(render.index).ok_or_else(|| EngineError::Other("missing checkpoint stroke".into()))?;
            if let Some(selection) = stroke.selection.as_mut() {
                let mut budget = 256;
                while budget > 0 {
                    let Some(run) = stroke.start.selection.get(render.selection_run) else { break };
                    if let Some(coverage) = run.coverage.get(render.selection_offset) {
                        selection.write_pixel(run.x.saturating_add(i32::try_from(render.selection_offset).unwrap_or(i32::MAX)), run.y, &[*coverage]);
                        render.selection_offset = render.selection_offset.saturating_add(1);
                        budget -= 1;
                    } else {
                        render.selection_run = render.selection_run.saturating_add(1);
                        render.selection_offset = 0;
                        budget -= 1;
                    }
                }
                if render.selection_run < stroke.start.selection.len() {
                    self.render_stroke = Some(render);
                    return Ok(());
                }
            }
            if let Some(points) = stroke.chunks.get(render.chunk) {
                let stop = render.point.saturating_add(8).min(points.len());
                if let Some(batch) = points.get(render.point..stop) {
                    stroke.renderer.push(batch);
                }
                render.point = stop;
                if stop == points.len() {
                    render.chunk = render.chunk.saturating_add(1);
                    render.point = 0;
                }
                self.render_stroke = Some(render);
                return Ok(());
            }
            if !render.finished {
                if stroke.ended {
                    stroke.renderer.finish();
                }
                render.tiles = stroke.renderer.coverage_tiles();
                render.finished = true;
                self.render_stroke = Some(render);
                return Ok(());
            }
            if stroke.visible
                && let Some(tile) = render.tiles.get(render.tile)
            {
                let target = self.document.layer_mut(stroke.start.layer).and_then(|l| l.surface_mut()).ok_or(EngineError::NoLayer(stroke.start.layer))?;
                stroke.renderer.composite_tiles(&stroke.pre, target, stroke.selection.as_ref(), stroke.start.lock_transparency, std::slice::from_ref(tile));
                render.tile = render.tile.saturating_add(1);
                self.render_stroke = Some(render);
                return Ok(());
            }
            self.replay = self.replay.saturating_add(1);
            return Ok(());
        }
        match self.restored.journal.get(self.replay).copied().ok_or_else(|| EngineError::Other("checkpoint replay cursor invalid".into()))? {
            JournalEntry::Command(index) => {
                self.restored.rebuild_command(index, &mut self.document)?;
                self.replay = self.replay.saturating_add(1);
            }
            JournalEntry::Stroke(index) => {
                let stroke = self.restored.strokes.get_mut(index).ok_or_else(|| EngineError::Other("missing checkpoint stroke".into()))?;
                let target = self.document.layer(stroke.start.layer).and_then(|l| l.surface());
                if target.is_none() && stroke.visible {
                    return Err(EngineError::NoLayer(stroke.start.layer));
                }
                if let Some(target) = target {
                    self.restored.bases.entry(stroke.start.layer).or_insert_with(|| target.clone());
                }
                // Hidden missing-layer ink still needs its renderer for a later layer
                // restoration/redo. Its absent prefix is replaced by the normal rebuild.
                stroke.pre = target.cloned().unwrap_or_else(|| Surface::new(self.document.pixel_format()));
                stroke.renderer = StrokeRenderer::new(&stroke.start.brush, Some(stroke.pre.format()), stroke.start.zoom);
                self.render_stroke =
                    Some(RenderStroke { index, chunk: 0, point: 0, selection_run: 0, selection_offset: 0, finished: false, tiles: Vec::new(), tile: 0 });
            }
        }
        Ok(())
    }
    fn finish(mut self, id: photocraft_doc::DocId, room: Option<RoomState>) -> crate::Result<(Document, Collaboration)> {
        if !self.progress().done {
            return Err(EngineError::Other("checkpoint restoration is not complete".into()));
        }
        self.document.id = id;
        self.restored.document_id = Some(id);
        self.restored.room = room;
        if let Some(base) = self.restored.baseline.as_mut() {
            base.id = id;
        }
        self.restored.applied_revision = self.revision;
        self.restored.sequencer.revision = self.revision;
        self.restored.next_nonce = self.revision;
        self.restored.canonical_document = Some(self.document.clone());
        Ok((self.document, self.restored))
    }
    pub(super) fn install_existing(self, session: &mut Session, id: photocraft_doc::DocId) -> crate::Result<()> {
        if !session.docs.iter().any(|s| s.doc.id == id) {
            return Err(EngineError::NoDocument);
        }
        let (document, restored) = self.finish(id, session.collaboration.room.clone())?;
        let state = session.docs.iter_mut().find(|s| s.doc.id == id).ok_or(EngineError::NoDocument)?;
        state.active_layer = document.top_layer();
        state.selected_layers = state.active_layer.into_iter().collect();
        state.doc = std::sync::Arc::new(document);
        state.revision = state.revision.saturating_add(1);
        state.last_damage = None;
        session.collaboration = restored;
        Ok(())
    }
    pub(super) fn install_new(self, session: &mut Session, room: RoomState) -> crate::Result<photocraft_doc::DocId> {
        let mut id = photocraft_doc::DocId::fresh();
        while session.docs.iter().any(|s| s.doc.id == id) {
            id = photocraft_doc::DocId::fresh();
        }
        let (document, restored) = self.finish(id, Some(room))?;
        session.add_document(document, None);
        session.collaboration = restored;
        Ok(id)
    }
}
