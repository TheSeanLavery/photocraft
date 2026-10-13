//! Undo/redo history.
//!
//! Because pixel tiles are `Arc`-shared copy-on-write (see `photocraft-raster`), a full
//! [`Document`] clone costs O(layers + tiles) pointer copies, not pixel copies. History therefore
//! stores whole-document snapshots per transaction, which is simple, obviously correct, and the
//! same approach Photoshop's History panel exposes to users (one state per step).
//!
//! Memory is bounded by `max_states` plus an approximate byte budget. Accounting deduplicates shared tiles; archived snapshots do not retain resident pixels.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use photocraft_doc::{DocId, Document, LayerId};
use serde::{Deserialize, Serialize};

/// The layers a state targeted when it was created: the active ("key") layer and every selected
/// layer. Undo and redo bring them back with the state's document, as the reference app does;
/// selecting layers is not a step of its own, so it doesn't change a state's target.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerTarget {
    pub active: Option<LayerId>,
    pub selected: Vec<LayerId>,
}

#[derive(Clone, Debug)]
pub struct HistoryState {
    pub label: String,
    /// The layers targeted when this state was created.
    pub layers: LayerTarget,
    pub identity: StateIdentity,
    document: StoredDocument,
    has_selection: bool,
}

/// Stable navigation identity; branch colors follow `branch`, not row positions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateIdentity {
    pub id: u64,
    pub parent: Option<u64>,
    pub branch: u64,
    #[serde(default)]
    pub revision: u64,
}

#[derive(Clone, Debug)]
pub struct HistoryEntry {
    pub identity: StateIdentity,
    pub label: String,
    pub current: bool,
}

impl HistoryState {
    /// Build a resident recovery state without copying pixel data.
    pub fn from_document(label: impl Into<String>, document: Arc<Document>) -> Self {
        Self {
            label: label.into(),
            layers: LayerTarget::default(),
            identity: StateIdentity::default(),
            has_selection: document.selection.is_some(),
            document: StoredDocument::Resident(document),
        }
    }

    /// Build a lazily restored recovery state. `has_selection` is metadata for
    /// selection menus, which must not read storage merely to draw themselves.
    pub fn from_archive(label: impl Into<String>, has_selection: bool, archive: Arc<dyn ArchivedDocument>) -> Self {
        Self {
            label: label.into(),
            layers: LayerTarget::default(),
            identity: StateIdentity::default(),
            has_selection,
            document: StoredDocument::Archived(archive),
        }
    }

    /// May read storage for a cold state; recovery writers call this on a worker.
    pub fn load_document(&self) -> Result<Arc<Document>, String> {
        self.document.load()
    }

    pub fn has_selection(&self) -> bool {
        self.has_selection
    }

    pub fn with_layers(mut self, layers: LayerTarget) -> Self {
        self.layers = layers;
        self
    }
}

/// Immutable handles and navigation metadata captured alongside the current
/// document. Undo is oldest first; redo's last entry is the next redo state.
/// Capturing this value never hydrates archived documents.
#[derive(Clone, Debug)]
pub struct HistoryCheckpoint {
    pub undo: Vec<HistoryState>,
    pub redo: Vec<HistoryState>,
    pub branches: Vec<HistoryState>,
    pub current_identity: StateIdentity,
    pub current_label: String,
    pub current_layers: LayerTarget,
    pub max_states: usize,
    pub max_bytes: usize,
}

/// Engine-owned immutable scratch archive. Loading must preserve the complete document.
pub trait ArchivedDocument: std::fmt::Debug + Send + Sync {
    fn load(&self) -> Result<Arc<Document>, String>;
    fn metadata_bytes(&self) -> usize {
        0
    }
    /// Resident backing for compact replay states; counted once, never mistaken for disk.
    fn resident(&self) -> Option<&Arc<Document>> {
        None
    }
}

#[derive(Debug)]
struct RemappedDocument {
    source: Arc<dyn ArchivedDocument>,
    id: DocId,
}
impl ArchivedDocument for RemappedDocument {
    fn load(&self) -> Result<Arc<Document>, String> {
        let mut doc = self.source.load()?;
        Arc::make_mut(&mut doc).id = self.id;
        Ok(doc)
    }
    fn metadata_bytes(&self) -> usize {
        std::mem::size_of::<Self>().saturating_add(self.source.metadata_bytes())
    }
    fn resident(&self) -> Option<&Arc<Document>> {
        self.source.resident()
    }
}

#[derive(Clone, Debug)]
enum StoredDocument {
    Resident(Arc<Document>),
    Archived(Arc<dyn ArchivedDocument>),
}

impl StoredDocument {
    fn load(&self) -> Result<Arc<Document>, String> {
        match self {
            Self::Resident(doc) => Ok(doc.clone()),
            Self::Archived(archive) => archive.load(),
        }
    }
    fn resident(&self) -> Option<&Arc<Document>> {
        match self {
            Self::Resident(doc) => Some(doc),
            Self::Archived(archive) => archive.resident(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct History {
    /// Past states; the last one is the state *before* the current document.
    undo: VecDeque<HistoryState>,
    redo: Vec<HistoryState>,
    branches: Vec<HistoryState>,
    current_identity: StateIdentity,
    next_id: u64,
    pub max_states: usize,
    /// Pixel memory budget in bytes for the current document plus the tiles only history holds
    /// (0 = unlimited). [`History::trim`] drops the oldest states beyond it.
    pub max_bytes: usize,
    /// Label of the step that produced the current document.
    current_label: String,
    /// The layers targeted when the current document was created (opened or edited).
    current_layers: LayerTarget,
    current_archive: Option<Arc<dyn ArchivedDocument>>,
    current_archive_document: Option<std::sync::Weak<Document>>,
}

impl Default for History {
    fn default() -> Self {
        Self::new(0)
    }
}

impl History {
    pub fn checkpoint(&self) -> HistoryCheckpoint {
        HistoryCheckpoint {
            undo: self.undo.iter().cloned().collect(),
            redo: self.redo.clone(),
            branches: self.branches.clone(),
            current_identity: self.current_identity,
            current_label: self.current_label.clone(),
            current_layers: self.current_layers.clone(),
            max_states: self.max_states,
            max_bytes: self.max_bytes,
        }
    }

    /// Restore the exact navigation cursor without reading document storage.
    /// Recovery metadata has independent admission limits; preference limits
    /// may subsequently be applied explicitly through `enforce_state_limit`.
    pub fn from_checkpoint(mut checkpoint: HistoryCheckpoint) -> Result<Self, String> {
        const MAX_LABEL_BYTES: usize = 4096;
        if checkpoint.current_label.len() > MAX_LABEL_BYTES
            || checkpoint.undo.iter().chain(checkpoint.redo.iter()).chain(checkpoint.branches.iter()).any(|state| state.label.len() > MAX_LABEL_BYTES)
        {
            return Err("Recovery history label exceeds 4096 bytes".into());
        }
        // Legacy checkpoints have no identities. Assign their linear path once.
        if checkpoint.current_identity == StateIdentity::default()
            && checkpoint.undo.iter().chain(checkpoint.redo.iter()).all(|state| state.identity == StateIdentity::default())
            && checkpoint.branches.is_empty()
        {
            let mut id = 0u64;
            for state in &mut checkpoint.undo {
                state.identity = StateIdentity { id, parent: id.checked_sub(1), branch: 0, revision: 0 };
                id = id.saturating_add(1);
            }
            checkpoint.current_identity = StateIdentity { id, parent: id.checked_sub(1), branch: 0, revision: 0 };
            for state in checkpoint.redo.iter_mut().rev() {
                id = id.saturating_add(1);
                state.identity = StateIdentity { id, parent: id.checked_sub(1), branch: 0, revision: 0 };
            }
        }
        if checkpoint.current_identity.id > u64::MAX / 2
            || checkpoint.current_identity.revision > u64::MAX / 2
            || checkpoint.current_identity.branch > checkpoint.current_identity.id
            || checkpoint.current_identity.parent.is_some_and(|parent| parent >= checkpoint.current_identity.id)
        {
            return Err("Invalid current history identity".into());
        }
        let mut ids = HashSet::from([checkpoint.current_identity.id]);
        let mut next_id = checkpoint.current_identity.id.checked_add(1).ok_or("History identity exhausted")?;
        for state in checkpoint.undo.iter().chain(checkpoint.redo.iter()).chain(checkpoint.branches.iter()) {
            if !ids.insert(state.identity.id)
                || state.identity.id > u64::MAX / 2
                || state.identity.revision > u64::MAX / 2
                || state.identity.branch > state.identity.id
                || state.identity.parent.is_some_and(|parent| parent >= state.identity.id)
            {
                return Err("Invalid recovery history identity".into());
            }
            next_id = next_id.max(state.identity.id.checked_add(1).ok_or("History identity exhausted")?);
        }
        Ok(Self {
            undo: checkpoint.undo.into(),
            redo: checkpoint.redo,
            branches: checkpoint.branches,
            current_identity: checkpoint.current_identity,
            next_id,
            current_label: checkpoint.current_label,
            current_layers: checkpoint.current_layers,
            current_archive: None,
            current_archive_document: None,
            max_states: checkpoint.max_states,
            max_bytes: checkpoint.max_bytes,
        })
    }

    pub fn new(max_states: usize) -> Self {
        Self {
            undo: VecDeque::new(),
            redo: Vec::new(),
            branches: Vec::new(),
            current_identity: StateIdentity::default(),
            next_id: 1,
            max_states,
            max_bytes: 0,
            current_label: "Open".into(),
            current_layers: LayerTarget::default(),
            current_archive: None,
            current_archive_document: None,
        }
    }

    /// Metadata-only stamp for logical history changes (including Purge).
    pub fn checkpoint_signature(&self) -> (usize, usize, &str) {
        (self.undo.len(), self.redo.len(), &self.current_label)
    }

    /// A duplicate recovered tab can be admitted under a fresh identity.
    /// Cold states are remapped only when restored, without reading disk here.
    pub fn remap_document_id(&mut self, id: DocId) {
        if let Some(source) = self.current_archive.take() {
            self.current_archive = Some(Arc::new(RemappedDocument { source, id }));
        }
        for state in self.undo.iter_mut().chain(self.redo.iter_mut()).chain(self.branches.iter_mut()) {
            match &mut state.document {
                StoredDocument::Resident(doc) => {
                    if doc.id != id {
                        Arc::make_mut(doc).id = id;
                    }
                }
                StoredDocument::Archived(source) => {
                    *source = Arc::new(RemappedDocument { source: source.clone(), id });
                }
            }
        }
    }

    /// Record that `before` was replaced by a new current document via step `label`, which left
    /// `layers` targeted.
    pub fn record(&mut self, label: impl Into<String>, before: Arc<Document>, layers: LayerTarget) {
        self.clear_current_archive();
        let prev_label = std::mem::replace(&mut self.current_label, label.into());
        let prev_layers = std::mem::replace(&mut self.current_layers, layers);
        self.undo.push_back(HistoryState {
            label: prev_label,
            layers: prev_layers,
            identity: self.current_identity,
            has_selection: before.selection.is_some(),
            document: StoredDocument::Resident(before),
        });
        let branch = if self.redo.is_empty() { self.current_identity.branch } else { self.next_id };
        self.current_identity = StateIdentity { id: self.next_id, parent: Some(self.current_identity.id), branch, revision: 0 };
        self.next_id = self.next_id.saturating_add(1);
        self.branches.append(&mut self.redo);
        self.enforce_state_limit();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn undo_label(&self) -> Option<&str> {
        self.can_undo().then_some(self.current_label.as_str())
    }
    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|s| s.label.as_str())
    }
    /// Labels of the redo states, the next redo first (the History panel's greyed rows).
    pub fn redo_labels(&self) -> impl Iterator<Item = &str> {
        self.redo.iter().rev().map(|s| s.label.as_str())
    }

    /// Set the layers the current document targets from its creation on: when it is first
    /// opened, or when its step coalesces or changes the selection after recording.
    pub fn set_current_layers(&mut self, layers: LayerTarget) {
        self.current_layers = layers;
        self.current_identity.revision = self.current_identity.revision.saturating_add(1);
    }

    pub fn current_layers(&self) -> &LayerTarget {
        &self.current_layers
    }

    /// Restore first: a failed archive read never moves either history stack.
    pub fn try_undo(&mut self, current: Arc<Document>) -> Result<Option<Arc<Document>>, String> {
        let Some(prev) = self.undo.back() else { return Ok(None) };
        let restored = prev.document.load()?;
        let Some(prev) = self.undo.pop_back() else { return Ok(None) };
        let label = std::mem::replace(&mut self.current_label, prev.label);
        let layers = std::mem::replace(&mut self.current_layers, prev.layers.clone());
        let has_selection = current.selection.is_some();
        let document = self.take_current_document(current);
        self.redo.push(HistoryState { label, has_selection, document, layers, identity: self.current_identity });
        self.current_identity = prev.identity;
        self.current_archive = match prev.document {
            StoredDocument::Archived(archive) => Some(archive),
            _ => None,
        };
        self.current_archive_document = self.current_archive.as_ref().map(|_| Arc::downgrade(&restored));
        Ok(Some(restored))
    }

    pub fn try_redo(&mut self, current: Arc<Document>) -> Result<Option<Arc<Document>>, String> {
        let Some(next) = self.redo.last() else { return Ok(None) };
        let restored = next.document.load()?;
        let Some(next) = self.redo.pop() else { return Ok(None) };
        let label = std::mem::replace(&mut self.current_label, next.label);
        let layers = std::mem::replace(&mut self.current_layers, next.layers.clone());
        let has_selection = current.selection.is_some();
        let document = self.take_current_document(current);
        self.undo.push_back(HistoryState { label, has_selection, document, layers, identity: self.current_identity });
        self.current_identity = next.identity;
        self.current_archive = match next.document {
            StoredDocument::Archived(archive) => Some(archive),
            _ => None,
        };
        self.current_archive_document = self.current_archive.as_ref().map(|_| Arc::downgrade(&restored));
        Ok(Some(restored))
    }

    /// Compatibility wrapper. Interactive callers should use [`Self::try_undo`] to report errors.
    pub fn undo(&mut self, current: Arc<Document>) -> Option<(Arc<Document>, LayerTarget)> {
        self.try_undo(current).ok().flatten().map(|doc| (doc, self.current_layers.clone()))
    }
    /// Compatibility wrapper. Interactive callers should use [`Self::try_redo`] to report errors.
    pub fn redo(&mut self, current: Arc<Document>) -> Option<(Arc<Document>, LayerTarget)> {
        self.try_redo(current).ok().flatten().map(|doc| (doc, self.current_layers.clone()))
    }

    /// Entries for a History panel: past labels oldest→newest, then the current label.
    pub fn entries(&self) -> Vec<String> {
        self.undo.iter().map(|s| s.label.clone()).chain(std::iter::once(self.current_label.clone())).collect()
    }

    /// Metadata only: drawing history must never decode a document from disk.
    pub fn timeline(&self) -> Vec<HistoryEntry> {
        let mut entries: Vec<_> = self
            .undo
            .iter()
            .chain(self.redo.iter())
            .chain(self.branches.iter())
            .map(|state| HistoryEntry { identity: state.identity, label: state.label.clone(), current: false })
            .collect();
        entries.push(HistoryEntry { identity: self.current_identity, label: self.current_label.clone(), current: true });
        entries.sort_by_key(|entry| entry.identity.id);
        entries
    }

    /// Delete one branch state and all of its descendants, preserving independent branches.
    /// The caller first moves the current document to the deleted state's predecessor.
    pub fn prune_descendants(&mut self, id: u64) {
        let timeline = self.timeline();
        let mut removed = HashSet::from([id]);
        loop {
            let before = removed.len();
            for entry in &timeline {
                if entry.identity.parent.is_some_and(|parent| removed.contains(&parent)) {
                    removed.insert(entry.identity.id);
                }
            }
            if removed.len() == before {
                break;
            }
        }
        self.undo.retain(|state| !removed.contains(&state.identity.id));
        self.redo.retain(|state| !removed.contains(&state.identity.id));
        self.branches.retain(|state| !removed.contains(&state.identity.id));
    }

    pub fn current_id(&self) -> u64 {
        self.current_identity.id
    }

    pub fn timeline_stamp(&self) -> (u64, u64, u64, usize, usize, usize) {
        (self.current_identity.id, self.current_identity.revision, self.next_id, self.undo.len(), self.redo.len(), self.branches.len())
    }

    pub fn adjacent_id(&self, redo: bool) -> Option<u64> {
        (if redo { self.redo.last() } else { self.undo.back() }).map(|state| state.identity.id)
    }

    /// Immutable destination handle, safe to restore on a worker.
    pub fn state_by_id(&self, current: Arc<Document>, id: u64) -> Option<HistoryState> {
        if id == self.current_identity.id {
            let mut state = HistoryState::from_document(self.current_label.clone(), current).with_layers(self.current_layers.clone());
            state.identity = self.current_identity;
            return Some(state);
        }
        self.undo.iter().chain(self.redo.iter()).chain(self.branches.iter()).find(|state| state.identity.id == id).cloned()
    }

    /// Commit one already-loaded destination. Intervening snapshots stay cold.
    /// Build/validate the path before mutating navigation, so any error is atomic.
    pub fn jump_loaded(&mut self, current: Arc<Document>, id: u64, restored: Arc<Document>) -> Result<(), String> {
        self.jump_loaded_on_path(current, id, restored, None)
    }

    pub fn navigation_branch(&self) -> u64 {
        self.redo.first().map_or(self.current_identity.branch, |state| state.identity.branch)
    }

    pub fn jump_loaded_on_path(&mut self, current: Arc<Document>, id: u64, restored: Arc<Document>, preferred_branch: Option<u64>) -> Result<(), String> {
        if id == self.current_identity.id {
            return Ok(());
        }
        let mut nodes: std::collections::BTreeMap<_, _> =
            self.undo.iter().chain(self.redo.iter()).chain(self.branches.iter()).map(|state| (state.identity.id, state.clone())).collect();
        let target = nodes.remove(&id).ok_or("History state is no longer available")?;
        let mut current_state = HistoryState::from_document(self.current_label.clone(), current.clone()).with_layers(self.current_layers.clone());
        current_state.identity = self.current_identity;
        nodes.insert(self.current_identity.id, current_state);
        let mut ancestors = Vec::new();
        let mut parent = target.identity.parent;
        let mut seen = HashSet::from([id]);
        while let Some(key) = parent {
            if !seen.insert(key) {
                return Err("Cyclic history path".into());
            }
            let Some(state) = nodes.remove(&key) else { break };
            parent = state.identity.parent;
            ancestors.push(state);
        }
        ancestors.reverse();
        let branch = preferred_branch.unwrap_or_else(|| if target.identity.parent.is_none() { self.navigation_branch() } else { target.identity.branch });
        let mut descendants = Vec::new();
        let mut children: std::collections::BTreeMap<u64, Vec<u64>> = std::collections::BTreeMap::new();
        for state in nodes.values() {
            if let Some(parent) = state.identity.parent {
                children.entry(parent).or_default().push(state.identity.id);
            }
        }
        let mut cursor = id;
        loop {
            let next = children
                .get(&cursor)
                .into_iter()
                .flatten()
                .filter_map(|key| nodes.get(key))
                .max_by_key(|state| (state.identity.branch == branch, state.identity.id))
                .map(|state| state.identity.id);
            let Some(next) = next else { break };
            if !seen.insert(next) {
                return Err("Cyclic history path".into());
            }
            let Some(state) = nodes.remove(&next) else { break };
            cursor = next;
            descendants.push(state);
        }
        // Preserve an archived handle for the previous current document when available.
        let stored_current = self.take_current_document(current);
        for state in ancestors.iter_mut().chain(descendants.iter_mut()).chain(nodes.values_mut()) {
            if state.identity.id == self.current_identity.id {
                state.document = stored_current.clone();
            }
        }
        descendants.reverse();
        self.undo = ancestors.into();
        self.redo = descendants;
        self.branches = nodes.into_values().collect();
        self.current_identity = target.identity;
        self.current_label = target.label;
        self.current_layers = target.layers;
        self.current_archive = match target.document {
            StoredDocument::Archived(archive) => Some(archive),
            _ => None,
        };
        self.current_archive_document = self.current_archive.as_ref().map(|_| Arc::downgrade(&restored));
        Ok(())
    }

    /// Document of past entry `i`, indexed like [`History::entries`] (0 = oldest). The last entry is
    /// the current document, which the history does not hold, so it (and any index past it) is `None`.
    pub fn state(&self, i: usize) -> Option<Arc<Document>> {
        self.try_state(i).ok().flatten()
    }

    /// Metadata-only query for menus; never hydrates a cold snapshot.
    pub fn has_past_selection(&self) -> bool {
        self.undo.iter().any(|state| state.has_selection)
    }

    /// Most recent selection-bearing state, found without reading scratch storage.
    pub fn latest_selection_state(&self) -> Option<usize> {
        self.undo.iter().enumerate().rev().find_map(|(index, state)| state.has_selection.then_some(index))
    }

    pub fn resident_state(&self, i: usize) -> Option<Arc<Document>> {
        self.undo.get(i).and_then(|state| match &state.document {
            StoredDocument::Resident(doc) => Some(doc.clone()),
            _ => None,
        })
    }

    pub fn try_state(&self, i: usize) -> Result<Option<Arc<Document>>, String> {
        self.undo.get(i).map(|state| state.document.load()).transpose()
    }

    /// Resident snapshots only; this never reads scratch storage.
    pub fn resident_states(&self) -> Vec<Arc<Document>> {
        let mut seen = HashSet::new();
        self.undo
            .iter()
            .chain(self.redo.iter())
            .chain(self.branches.iter())
            .filter_map(|state| state.document.resident().cloned())
            .chain(self.current_archive.iter().filter_map(|archive| archive.resident().cloned()))
            .filter(|doc| seen.insert(Arc::as_ptr(doc) as usize))
            .collect()
    }

    /// Concrete snapshots eligible for scratch serialization. Compact replay backing stays resident.
    pub fn spillable_states(&self) -> Vec<Arc<Document>> {
        self.undo
            .iter()
            .chain(self.redo.iter())
            .chain(self.branches.iter())
            .filter_map(|state| match &state.document {
                StoredDocument::Resident(doc) => Some(doc.clone()),
                _ => None,
            })
            .collect()
    }

    /// State descriptors and compact replay records, separate from pixel payload accounting.
    pub fn metadata_bytes(&self) -> usize {
        let mut total = self.current_label.capacity().saturating_add(self.current_layers.selected.capacity().saturating_mul(std::mem::size_of::<LayerId>()));
        for state in self.undo.iter().chain(self.redo.iter()).chain(self.branches.iter()) {
            total = total
                .saturating_add(std::mem::size_of::<HistoryState>())
                .saturating_add(state.label.capacity())
                .saturating_add(state.layers.selected.capacity().saturating_mul(std::mem::size_of::<LayerId>()));
            if let StoredDocument::Archived(archive) = &state.document {
                total = total.saturating_add(archive.metadata_bytes());
            }
        }
        if let Some(archive) = &self.current_archive {
            total = total.saturating_add(archive.metadata_bytes());
        }
        total
    }

    pub fn archived_states(&self) -> usize {
        self.undo
            .iter()
            .chain(self.redo.iter())
            .chain(self.branches.iter())
            .filter(|state| matches!(&state.document, StoredDocument::Archived(archive) if archive.resident().is_none()))
            .count()
    }

    /// Adjacent resident snapshots pinned for immediate undo/redo.
    pub fn hot_states(&self) -> Vec<Arc<Document>> {
        self.undo
            .back()
            .into_iter()
            .chain(self.redo.last())
            .filter_map(|state| state.document.resident().cloned())
            .chain(self.current_archive.iter().filter_map(|archive| archive.resident().cloned()))
            .collect()
    }

    /// Whether this allocation is the immediate undo or redo state.
    /// Used to revalidate asynchronous spill results after intervening navigation.
    pub fn is_hot(&self, doc: &Arc<Document>) -> bool {
        self.undo.back().and_then(|state| state.document.resident()).is_some_and(|hot| Arc::ptr_eq(doc, hot))
            || self.redo.last().and_then(|state| state.document.resident()).is_some_and(|hot| Arc::ptr_eq(doc, hot))
    }

    /// Oldest eligible snapshot, keeping the immediate undo and redo states hot.
    pub fn spill_candidate(&self) -> Option<Arc<Document>> {
        self.undo
            .iter()
            .take(self.undo.len().saturating_sub(1))
            .chain(self.redo.iter().take(self.redo.len().saturating_sub(1)))
            .filter_map(|state| state.document.resident())
            .find(|doc| {
                !self.undo.back().and_then(|state| state.document.resident()).is_some_and(|hot| Arc::ptr_eq(doc, hot))
                    && !self.redo.last().and_then(|state| state.document.resident()).is_some_and(|hot| Arc::ptr_eq(doc, hot))
            })
            .cloned()
    }

    /// Replace only the allocation actually archived; stale worker results are harmless.
    pub fn replace_resident(&mut self, doc: &Arc<Document>, archive: Arc<dyn ArchivedDocument>) -> bool {
        let mut replaced = false;
        for state in self.undo.iter_mut().chain(self.redo.iter_mut()).chain(self.branches.iter_mut()) {
            if matches!(&state.document, StoredDocument::Resident(resident) if Arc::ptr_eq(resident, doc)) {
                state.document = StoredDocument::Archived(archive.clone());
                replaced = true;
            }
        }
        replaced
    }

    /// Budget fallback, preserving the immediate undo and redo steps.
    pub fn drop_oldest(&mut self) -> bool {
        if !self.branches.is_empty() {
            self.branches.remove(0);
            return true;
        }
        if self.undo.len() > 1 {
            self.undo.pop_front();
            true
        } else if self.redo.len() > 1 {
            self.redo.remove(0);
            true
        } else {
            false
        }
    }

    /// Explicit preference reduction may retire even the final undo/redo state.
    /// The current document is never owned by these stacks.
    pub fn discard_oldest(&mut self) -> bool {
        if !self.branches.is_empty() {
            self.branches.remove(0);
            return true;
        }
        if self.undo.pop_front().is_some() {
            true
        } else if !self.redo.is_empty() {
            self.redo.remove(0);
            true
        } else {
            false
        }
    }

    /// Enforce a lowered history-count preference without reading scratch storage.
    pub fn enforce_state_limit(&mut self) -> usize {
        if self.max_states == 0 {
            return 0;
        }
        let limit = self.max_states.max(1);
        let mut removed = 0;
        while self.undo.len().saturating_add(self.redo.len()).saturating_add(self.branches.len()) > limit {
            if !self.discard_oldest() {
                break;
            }
            removed += 1;
        }
        removed
    }

    /// Number of past states (entries before the current one).
    pub fn past_len(&self) -> usize {
        self.undo.len()
    }

    /// Rename the current step (e.g. after folding several steps into one with [`Self::purge_last`]).
    pub fn set_current_label(&mut self, label: impl Into<String>) {
        self.current_label = label.into();
    }

    /// Forget the most recent undo state and every redo state (Edit › Purge › Undo): the
    /// last step can no longer be undone and its pixels are released.
    pub fn purge_last(&mut self) -> bool {
        self.redo.clear();
        self.undo.pop_back().is_some()
    }

    /// Forget the redo states, e.g. after undoing half of a compound step that failed.
    pub fn clear_redo(&mut self) {
        self.redo.clear();
    }

    /// Invalidate replay provenance after a successful edit that changes the current document.
    pub fn clear_current_archive(&mut self) {
        self.current_archive = None;
        self.current_archive_document = None;
    }

    pub fn set_current_archive(&mut self, current: &Arc<Document>, archive: Arc<dyn ArchivedDocument>) {
        self.current_archive_document = Some(Arc::downgrade(current));
        self.current_archive = Some(archive);
    }

    fn take_current_document(&mut self, current: Arc<Document>) -> StoredDocument {
        let matches = self.current_archive_document.take().and_then(|bound| bound.upgrade()).is_some_and(|bound| Arc::ptr_eq(&bound, &current));
        match self.current_archive.take() {
            Some(archive) if matches => StoredDocument::Archived(archive),
            _ => StoredDocument::Resident(current),
        }
    }

    pub fn clear(&mut self) {
        self.clear_current_archive();
        self.undo.clear();
        self.redo.clear();
        self.branches.clear();
        self.current_identity.parent = None;
    }

    /// Approximate unique tile and shared binary-payload bytes held by history.
    pub fn unique_bytes(&self, current: &Document) -> usize {
        let mut seen = HashSet::new();
        document_bytes(current, &mut seen);
        self.resident_states().iter().fold(0usize, |n, doc| n.saturating_add(document_bytes(doc, &mut seen)))
    }

    /// Managed bytes of `current` plus the allocations only history holds (what [`History::trim`]
    /// bounds by [`History::max_bytes`]).
    pub fn pixel_bytes(&self, current: &Document) -> usize {
        let mut seen = HashSet::new();
        let own = document_bytes(current, &mut seen);
        self.resident_states().iter().fold(own, |n, doc| n.saturating_add(document_bytes(doc, &mut seen)))
    }

    /// Keep pixel memory within [`History::max_bytes`]: the current document's tiles plus the
    /// tiles only history holds (newest states first). The oldest undo states that don't fit are
    /// dropped; the most recent one is always kept so the last step can be undone. Returns how
    /// many states were dropped.
    pub fn trim(&mut self, current: &Document) -> usize {
        if self.max_bytes == 0 || self.undo.len() <= 1 {
            return 0;
        }
        let mut seen = HashSet::new();
        let mut total = document_bytes(current, &mut seen);
        for s in self.redo.iter().rev() {
            if let Some(doc) = s.document.resident() {
                total = total.saturating_add(document_bytes(doc, &mut seen));
            }
        }
        let mut keep = 0;
        for s in self.undo.iter().rev() {
            if let Some(doc) = s.document.resident() {
                total = total.saturating_add(document_bytes(doc, &mut seen));
            }
            if total > self.max_bytes && keep >= 1 {
                break;
            }
            keep += 1;
        }
        let drop = self.undo.len() - keep;
        self.undo.drain(..drop);
        drop
    }
}

/// Managed tile and shared binary-payload bytes not already in `seen`.
/// Metadata containers, strings, renderer caches and allocator overhead are additional memory.
pub fn document_bytes(doc: &Document, seen: &mut HashSet<usize>) -> usize {
    let mut add = |surface: &photocraft_doc::Surface| {
        surface.tiles().filter(|(_, tile)| seen.insert(Arc::as_ptr(tile) as usize)).fold(0usize, |n, (_, tile)| n.saturating_add(tile.bytes().len()))
    };
    let mut total = 0usize;
    let layers = doc.walk();
    for (_, _, layer) in &layers {
        if let Some(surface) = layer.surface() {
            total = total.saturating_add(add(surface));
        }
        if let Some(mask) = &layer.mask {
            total = total.saturating_add(add(&mask.surface));
        }
        if let Some(cache) = &layer.fill_cache {
            total = total.saturating_add(add(&cache.surface));
        }
        if let photocraft_doc::LayerContent::Smart(smart) = &layer.content
            && let Some(mask) = &smart.filter_mask
        {
            total = total.saturating_add(add(&mask.surface));
        }
        if let Some(video) = &layer.video {
            for frame in video.frames.iter().chain(video.original_frames.iter().flatten()) {
                total = total.saturating_add(add(frame));
            }
        }
    }
    for channel in doc.channels.iter().chain(&doc.quick_mask) {
        total = total.saturating_add(add(&channel.surface));
    }
    if let Some(selection) = &doc.selection {
        total = total.saturating_add(add(selection));
    }
    for pattern in &doc.patterns {
        total = total.saturating_add(add(&pattern.surface));
    }
    // Distinct live Arc allocations cannot have the same address, including across
    // payload types. A caller keeps all traversed documents alive while deduplicating.
    let mut blob = |bytes: &Arc<Vec<u8>>| {
        if seen.insert(Arc::as_ptr(bytes) as usize) { bytes.capacity() } else { 0 }
    };
    for bytes in doc.icc_profile.iter().chain(doc.metadata.exif.iter()) {
        total = total.saturating_add(blob(bytes));
    }
    for (_, _, bytes) in &doc.metadata.psd_resources {
        total = total.saturating_add(blob(bytes));
    }
    for (_, _, bytes) in &doc.metadata.psd_global_blocks {
        total = total.saturating_add(blob(bytes));
    }
    for path in &doc.paths {
        if let Some(bytes) = &path.psd_raw {
            total = total.saturating_add(blob(bytes));
        }
    }
    for (_, _, layer) in &layers {
        for (_, bytes) in &layer.psd_blocks {
            total = total.saturating_add(blob(bytes));
        }
        if let Some(bytes) = &layer.effects.psd_raw {
            total = total.saturating_add(blob(bytes));
        }
        match &layer.content {
            photocraft_doc::LayerContent::Text(text) => {
                if let Some(bytes) = &text.psd_raw {
                    total = total.saturating_add(blob(bytes));
                }
            }
            photocraft_doc::LayerContent::Shape(shape) => {
                if let Some(bytes) = &shape.psd_raw {
                    total = total.saturating_add(blob(bytes));
                }
            }
            photocraft_doc::LayerContent::Smart(smart) => {
                if let Some(bytes) = &smart.psd_raw {
                    total = total.saturating_add(blob(bytes));
                }
                if let photocraft_doc::SmartSource::Embedded { bytes, .. } = &smart.source {
                    total = total.saturating_add(blob(bytes));
                }
            }
            _ => {}
        }
    }
    for (_, _, layer) in &layers {
        if let photocraft_doc::LayerContent::Adjustment(photocraft_doc::Adjustment::ColorLookup { lut: Some(lut), .. }) = &layer.content
            && seen.insert(Arc::as_ptr(lut) as usize)
        {
            total = total.saturating_add(lut.capacity().saturating_mul(std::mem::size_of::<f32>()));
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_doc::{Color, ColorMode, Layer, SampleType, Size};

    fn base() -> Document {
        Document::with_background("h", Size::new(64, 64), ColorMode::Rgb, SampleType::U8, Color::WHITE)
    }

    /// Apply an edit the way the engine does: snapshot, mutate a clone, record.
    fn edit(h: &mut History, cur: &mut Arc<Document>, label: &str, f: impl FnOnce(&mut Document)) {
        let before = cur.clone();
        let mut d = (**cur).clone();
        f(&mut d);
        *cur = Arc::new(d);
        h.record(label, before, LayerTarget::default());
    }

    #[test]
    fn undo_redo_cycle() {
        let mut h = History::default();
        let mut cur = Arc::new(base());
        edit(&mut h, &mut cur, "New Layer", |d| {
            d.insert_above(None, Layer::raster("L", d.pixel_format()));
        });
        assert_eq!(cur.layers.len(), 2);
        assert_eq!(h.undo_label(), Some("New Layer"));
        cur = h.undo(cur).unwrap().0;
        assert_eq!(cur.layers.len(), 1);
        assert_eq!(h.redo_label(), Some("New Layer"));
        cur = h.redo(cur).unwrap().0;
        assert_eq!(cur.layers.len(), 2);
        assert!(h.redo(cur.clone()).is_none());
    }

    #[test]
    fn undo_and_redo_return_the_layers_each_state_targeted_when_created() {
        let mut h = History::default();
        let open = Arc::new(base());
        let bg = open.layers[0].id;
        let target = |id: LayerId| LayerTarget { active: Some(id), selected: vec![id] };
        h.set_current_layers(target(bg));
        // New Layer targets the new layer; the background is then selected (not a step) and filled.
        let mut d = (*open).clone();
        let new = d.insert_above(None, Layer::raster("L", d.pixel_format()));
        let layered = Arc::new(d);
        h.record("New Layer", open, target(new));
        let mut d = (*layered).clone();
        d.name = "filled".into();
        h.record("Fill", layered, target(bg));
        let (d, layers) = h.undo(Arc::new(d)).unwrap();
        assert_eq!(layers, target(new), "New Layer's state targeted the new layer when created");
        let (d, layers) = h.undo(d).unwrap();
        assert_eq!(layers, target(bg), "the opened document's target");
        let (d, layers) = h.redo(d).unwrap();
        assert_eq!(layers, target(new));
        let (_, layers) = h.redo(d).unwrap();
        assert_eq!(layers, target(bg), "Fill's state targeted the background");
    }

    #[test]
    fn state_accessor_matches_entries() {
        let mut h = History::default();
        let mut cur = Arc::new(base());
        let open = cur.clone();
        edit(&mut h, &mut cur, "A", |d| d.name = "a".into());
        edit(&mut h, &mut cur, "B", |d| d.name = "b".into());
        assert_eq!(h.entries().len(), h.past_len() + 1);
        assert!(Arc::ptr_eq(&h.state(0).unwrap(), &open));
        assert_eq!(h.state(1).unwrap().name, "a");
        assert!(h.state(2).is_none(), "the current document is not held");
        cur = h.undo(cur).unwrap().0;
        h.undo(cur).unwrap();
        assert_eq!(h.redo_labels().collect::<Vec<_>>(), ["A", "B"], "next redo first");
    }

    #[test]
    fn new_edit_clears_redo() {
        let mut h = History::default();
        let mut cur = Arc::new(base());
        edit(&mut h, &mut cur, "A", |d| d.name = "a".into());
        cur = h.undo(cur).unwrap().0;
        edit(&mut h, &mut cur, "B", |d| d.name = "b".into());
        assert!(!h.can_redo());
        assert_eq!(h.entries(), vec!["Open".to_string(), "B".to_string()]);
    }

    #[test]
    fn max_states_bound() {
        let mut h = History::new(3);
        let mut cur = Arc::new(base());
        for i in 0..10 {
            edit(&mut h, &mut cur, &format!("step {i}"), |d| d.name = format!("{i}"));
        }
        let mut n = 0;
        while let Some((d, _)) = h.undo(cur.clone()) {
            cur = d;
            n += 1;
        }
        assert_eq!(n, 3);
        assert_eq!(cur.name, "6");
    }

    #[test]
    fn byte_budget_drops_oldest_states_but_keeps_one() {
        let mut h = History::new(50);
        let mut cur = Arc::new(base());
        let tile = 256 * 256 * 4;
        // Each step repaints the background's tile: one unique tile per state.
        for i in 0..6 {
            edit(&mut h, &mut cur, &format!("paint {i}"), |d| {
                let id = d.layers[0].id;
                d.layer_mut(id).unwrap().surface_mut().unwrap().write_pixel(1, 1, &[i as f32 / 8.0, 0.0, 0.0, 1.0]);
            });
        }
        assert_eq!(h.trim(&cur), 0, "unlimited by default");
        assert_eq!(h.past_len(), 6);
        // The current tile plus three history tiles fit.
        h.max_bytes = 4 * tile;
        assert_eq!(h.trim(&cur), 3);
        assert_eq!(h.past_len(), 3);
        assert_eq!(h.unique_bytes(&cur), 3 * tile);
        assert_eq!(h.pixel_bytes(&cur), 4 * tile, "the current tile plus history's");
        assert_eq!(h.entries()[0], "paint 2");
        // A budget smaller than one state still keeps the last step undoable.
        h.max_bytes = 1;
        h.trim(&cur);
        assert_eq!(h.past_len(), 1);
        assert!(h.undo(cur.clone()).is_some());
    }

    #[test]
    fn history_shares_untouched_tiles() {
        let mut h = History::default();
        let mut cur = Arc::new(base());
        assert_eq!(h.unique_bytes(&cur), 0);
        // rename only: no pixel changes, history holds no unique tiles
        edit(&mut h, &mut cur, "Rename", |d| d.name = "x".into());
        assert_eq!(h.unique_bytes(&cur), 0);
        // paint one pixel: exactly one tile becomes unique to history
        edit(&mut h, &mut cur, "Paint", |d| {
            let id = d.layers[0].id;
            d.layer_mut(id).unwrap().surface_mut().unwrap().write_pixel(1, 1, &[0.0, 0.0, 0.0, 1.0]);
        });
        assert_eq!(h.unique_bytes(&cur), 256 * 256 * 4);
    }
    #[derive(Debug)]
    struct Archive(Result<Arc<Document>, String>);
    impl ArchivedDocument for Archive {
        fn load(&self) -> Result<Arc<Document>, String> {
            self.0.clone()
        }
    }

    #[test]
    fn selection_tiles_count_current_and_history_without_double_counting() {
        use photocraft_doc::{PixelFormat, Surface};
        for depth in SampleType::ALL {
            let mut doc = Document::new("selection", Size::new(300, 64), ColorMode::Rgb, depth);
            let mut selection = Surface::new(PixelFormat::new(ColorMode::Grayscale, depth, false));
            selection.write_pixel(1, 1, &[1.0]);
            selection.write_pixel(257, 1, &[1.0]);
            doc.selection = Some(selection);
            let mut cur = Arc::new(doc);
            let tile = match depth {
                SampleType::U8 => 65_536,
                SampleType::U16 => 131_072,
                SampleType::F32 => 262_144,
            };
            let mut h = History { max_bytes: usize::MAX, ..History::default() };
            assert_eq!(h.pixel_bytes(&cur), 2 * tile, "current selection counts without any history");
            assert_eq!(h.unique_bytes(&cur), 0);
            edit(&mut h, &mut cur, "Rename", |doc| doc.name = "renamed".into());
            assert_eq!(h.pixel_bytes(&cur), 2 * tile, "the shared selection tiles count once");
            edit(&mut h, &mut cur, "Selection", |doc| {
                doc.selection.as_mut().unwrap().write_pixel(1, 1, &[0.5]);
            });
            assert_eq!(h.pixel_bytes(&cur), 3 * tile, "only the changed selection tile is copied");
            assert_eq!(h.unique_bytes(&cur), tile);
            h.trim(&cur);
            assert_counter(&h, &cur);
            let selected_value = cur.selection.as_ref().unwrap().sample_channel(1, 1, 0);
            edit(&mut h, &mut cur, "Deselect", |doc| doc.selection = None);
            assert_eq!(h.pixel_bytes(&cur), 3 * tile, "deselecting leaves the previous masks in history");
            assert_eq!(h.unique_bytes(&cur), 3 * tile);
            assert_counter(&h, &cur);
            cur = h.undo(cur).unwrap().0;
            assert_eq!(cur.selection.as_ref().unwrap().sample_channel(1, 1, 0), selected_value);
            assert_eq!(h.pixel_bytes(&cur), 3 * tile);
            assert_eq!(h.unique_bytes(&cur), tile);
            assert_counter(&h, &cur);
            // Coalescing can change the current mask without recording another state.
            Arc::make_mut(&mut cur).selection.as_mut().unwrap().write_pixel(257, 1, &[0.25]);
            assert_eq!(h.pixel_bytes(&cur), 4 * tile);
            assert_eq!(h.trim(&cur), 0);
            assert_counter(&h, &cur);
            cur = h.redo(cur).unwrap().0;
            assert!(cur.selection.is_none());
            assert_eq!(h.pixel_bytes(&cur), 4 * tile);
            assert_counter(&h, &cur);
        }
    }

    #[test]
    fn selection_byte_budget_drops_old_masks_and_keeps_the_last_undo() {
        use photocraft_doc::{PixelFormat, Surface};
        for depth in SampleType::ALL {
            let mut doc = Document::new("selection", Size::new(64, 64), ColorMode::Rgb, depth);
            doc.selection = Some(Surface::new(PixelFormat::new(ColorMode::Grayscale, depth, false)));
            let mut cur = Arc::new(doc);
            let tile = 256 * 256 * depth.bytes();
            let mut h = History::default();
            for i in 1..=6 {
                edit(&mut h, &mut cur, "Selection", |doc| {
                    doc.selection.as_mut().unwrap().write_pixel(1, 1, &[i as f32 / 8.0]);
                });
            }
            assert_eq!(h.pixel_bytes(&cur), 6 * tile);
            h.max_bytes = 3 * tile;
            assert_eq!(h.trim(&cur), 4);
            assert_eq!(h.past_len(), 2);
            assert_eq!(h.pixel_bytes(&cur), 3 * tile);
            assert_eq!(h.unique_bytes(&cur), 2 * tile);
            assert_counter(&h, &cur);
            h.max_bytes = 1;
            assert_eq!(h.trim(&cur), 1);
            assert_eq!(h.past_len(), 1, "the most recent undo survives even below one mask's size");
            assert_eq!(h.pixel_bytes(&cur), 2 * tile);
            cur = h.undo(cur).unwrap().0;
            assert_counter(&h, &cur);
        }
    }

    fn assert_counter(h: &History, current: &Document) {
        let own = document_bytes(current, &mut HashSet::new());
        let total = own.saturating_add(h.unique_bytes(current));
        let mut seen = HashSet::new();
        let mut union = document_bytes(current, &mut seen);
        for state in h.resident_states() {
            union = union.saturating_add(document_bytes(&state, &mut seen));
        }
        assert_eq!(h.pixel_bytes(current), total);
        assert_eq!(h.pixel_bytes(current), union);
    }

    fn video_document(depth: SampleType, frames: usize) -> Document {
        let mut doc = Document::new("video", Size::new(64, 64), ColorMode::Rgb, depth);
        let mut layer = Layer::raster("Video", doc.pixel_format());
        let footage: Vec<_> = (0..frames)
            .map(|i| {
                let mut surface = photocraft_doc::Surface::new(doc.pixel_format());
                surface.write_pixel(1, 1, &[i as f32 / frames as f32, 0.0, 0.0, 1.0]);
                surface
            })
            .collect();
        layer.content = photocraft_doc::LayerContent::Raster(footage[0].clone());
        layer.video = Some(photocraft_doc::VideoData::new(footage, 24.0));
        doc.layers.push(layer);
        doc
    }

    #[test]
    fn video_memory_counts_originals_and_deduplicates_displayed_frames() {
        for depth in [SampleType::U8, SampleType::U16, SampleType::F32] {
            let mut doc = video_document(depth, 3);
            let tile = doc.layers[0].surface().unwrap().tiles().next().unwrap().1.bytes().len();
            let video = doc.layers[0].video.as_mut().unwrap();
            video.original_frames = Some(video.frames.clone());
            // Original footage and the displayed frame alias the stack, except this painted frame.
            video.frames[2].write_pixel(1, 1, &[0.0, 1.0, 0.0, 1.0]);
            let mut cur = Arc::new(doc);
            let mut h = History { max_bytes: usize::MAX, ..History::default() };
            assert_eq!(h.pixel_bytes(&cur), 4 * tile);
            for name in ["A", "B"] {
                edit(&mut h, &mut cur, "Rename", |doc| doc.name = name.into());
                h.trim(&cur);
            }
            assert_counter(&h, &cur);
            assert_eq!(h.pixel_bytes(&cur), 4 * tile);
            assert_eq!(h.unique_bytes(&cur), 0);
            // Rasterizing keeps the displayed frame; the rest of the clip survives only in undo.
            edit(&mut h, &mut cur, "Rasterize", |doc| doc.layers[0].video = None);
            assert_eq!(h.unique_bytes(&cur), 3 * tile);
            assert_eq!(h.pixel_bytes(&cur), 4 * tile);
            assert_counter(&h, &cur);
            cur = h.undo(cur).unwrap().0;
            assert_eq!(h.unique_bytes(&cur), 0);
            assert_counter(&h, &cur);
            cur = h.redo(cur).unwrap().0;
            assert_eq!(h.unique_bytes(&cur), 3 * tile);
            assert_counter(&h, &cur);
            h.clear();
            assert_eq!(h.pixel_bytes(&cur), tile);
        }
    }

    #[test]
    fn video_memory_budget_evicts_deleted_off_playhead_frames() {
        for depth in [SampleType::U8, SampleType::U16, SampleType::F32] {
            let mut cur = Arc::new(video_document(depth, 6));
            let tile = cur.layers[0].surface().unwrap().tiles().next().unwrap().1.bytes().len();
            let mut h = History::default();
            for _ in 0..4 {
                edit(&mut h, &mut cur, "Delete Frame", |doc| {
                    doc.layers[0].video.as_mut().unwrap().frames.remove(1);
                });
            }
            assert_eq!(h.pixel_bytes(&cur), 6 * tile);
            assert_eq!(h.unique_bytes(&cur), 4 * tile);
            h.max_bytes = 3 * tile;
            assert_eq!(h.trim(&cur), 3);
            assert_eq!(h.past_len(), 1);
            assert_eq!(h.pixel_bytes(&cur), 3 * tile);
            assert_eq!(h.unique_bytes(&cur), tile);
            assert_counter(&h, &cur);
            cur = h.undo(cur).unwrap().0;
            assert_eq!(cur.layers[0].video.as_ref().unwrap().frames.len(), 3);
            assert_counter(&h, &cur);
            cur = h.redo(cur).unwrap().0;
            assert_eq!(cur.layers[0].video.as_ref().unwrap().frames.len(), 2);
            assert_counter(&h, &cur);
            // A budget below the remaining clip still keeps the latest step undoable.
            h.max_bytes = 1;
            assert_eq!(h.trim(&cur), 0);
            assert!(h.can_undo());
        }
    }

    #[test]
    fn counter_tracks_eviction_undo_redo_branching_purge_and_clones() {
        for depth in [SampleType::U8, SampleType::U16, SampleType::F32] {
            let mut cur = Arc::new(Document::with_background("h", Size::new(300, 64), ColorMode::Rgb, depth, Color::WHITE));
            Arc::make_mut(&mut cur).selection = Some(photocraft_doc::Surface::new(photocraft_doc::PixelFormat::GRAY8));
            let mut h = History::new(4);
            h.max_bytes = usize::MAX;
            for i in 0..8 {
                edit(&mut h, &mut cur, "Paint", |doc| {
                    doc.layers[0].surface_mut().unwrap().write_pixel(1, 1, &[i as f32 / 10.0, 0.0, 0.0, 1.0]);
                    doc.selection.as_mut().unwrap().write_pixel(1, 1, &[i as f32 / 10.0]);
                });
                h.trim(&cur);
                if i >= 1 {
                    assert_counter(&h, &cur);
                }
            }
            cur = h.undo(cur).unwrap().0;
            assert_counter(&h, &cur);
            cur = h.undo(cur).unwrap().0;
            assert_counter(&h, &cur);
            cur = h.redo(cur).unwrap().0;
            assert_counter(&h, &cur);
            let copy = h.clone();
            edit(&mut h, &mut cur, "Branch", |doc| doc.name = "branch".into());
            assert!(!h.can_redo());
            assert_counter(&h, &cur);
            assert_counter(&copy, &cur);
            cur = h.undo(cur).unwrap().0;
            h.clear_redo();
            assert_counter(&h, &cur);
            h.purge_last();
            assert_counter(&h, &cur);
            h.clear();
            assert!(h.resident_states().is_empty());
            assert_eq!(h.pixel_bytes(&cur), document_bytes(&cur, &mut HashSet::new()));
        }
    }

    #[test]
    fn recovery_checkpoint_preserves_cursor_labels_and_both_stack_orders() {
        let mut history = History::new(37);
        history.max_bytes = 1234;
        let mut current = Arc::new(base());
        for name in ["A", "B", "C", "D"] {
            edit(&mut history, &mut current, name, |doc| doc.name = name.into());
        }
        current = history.try_undo(current).unwrap().unwrap();
        current = history.try_undo(current).unwrap().unwrap();
        assert_eq!(current.name, "B");
        let checkpoint = history.checkpoint();
        assert_eq!(checkpoint.current_label, "B");
        assert_eq!(checkpoint.undo.iter().map(|s| s.label.as_str()).collect::<Vec<_>>(), ["Open", "A"]);
        assert_eq!(checkpoint.redo.iter().map(|s| s.label.as_str()).collect::<Vec<_>>(), ["D", "C"]);
        let mut recovered = History::from_checkpoint(checkpoint).unwrap();
        assert_eq!(recovered.max_states, 37);
        assert_eq!(recovered.max_bytes, 1234);
        for expected in ["C", "D"] {
            current = recovered.try_redo(current).unwrap().unwrap();
            assert_eq!(current.name, expected);
        }
        for expected in ["C", "B", "A"] {
            current = recovered.try_undo(current).unwrap().unwrap();
            assert_eq!(current.name, expected);
        }
        assert_eq!(recovered.redo_label(), Some("B"));
    }

    #[test]
    fn recovery_checkpoint_keeps_cold_states_lazy_and_failed_navigation_atomic() {
        let mut checkpoint = History::new(50).checkpoint();
        checkpoint.current_label = "Current".into();
        checkpoint.undo.push(HistoryState::from_archive("Before", true, Arc::new(Archive(Err("unreadable recovery".into())))));
        checkpoint.redo.push(HistoryState::from_document("After", Arc::new(base())));
        let mut history = History::from_checkpoint(checkpoint).unwrap();
        let captured = history.checkpoint();
        assert!(captured.undo.first().unwrap().has_selection());
        assert!(captured.undo.first().unwrap().load_document().is_err());
        assert_eq!(history.archived_states(), 1);
        assert!(history.has_past_selection());
        assert!(history.try_undo(Arc::new(base())).is_err());
        assert_eq!(history.entries(), ["Before", "Current"]);
        assert_eq!(history.redo_label(), Some("After"));
        assert_eq!(history.past_len(), 1);
    }

    #[test]
    fn recovered_identity_remaps_resident_and_cold_states_without_changing_sources() {
        let original = Arc::new(base());
        let old_id = original.id;
        let new_id = DocId::fresh();
        let mut checkpoint = History::default().checkpoint();
        checkpoint.undo.push(HistoryState::from_archive("Cold", false, Arc::new(Archive(Ok(original.clone())))));
        checkpoint.redo.push(HistoryState::from_document("Resident", original.clone()));
        let mut history = History::from_checkpoint(checkpoint).unwrap();
        history.remap_document_id(new_id);
        assert_eq!(history.archived_states(), 1, "remapping does not hydrate cold history");
        let checkpoint = history.checkpoint();
        assert_eq!(checkpoint.undo[0].load_document().unwrap().id, new_id);
        assert_eq!(checkpoint.redo[0].load_document().unwrap().id, new_id);
        assert_eq!(original.id, old_id, "other tabs retain their identities");
    }

    #[test]
    fn remapped_current_replay_preserves_identity_through_undo_redo() {
        let original = Arc::new(base());
        let new_id = DocId::fresh();
        let mut history = History::default();
        history.record("Move", original.clone(), LayerTarget::default());
        history.set_current_archive(&original, Arc::new(Archive(Ok(original.clone()))));
        history.remap_document_id(new_id);
        let mut current = (*original).clone();
        current.id = new_id;
        let previous = history.try_undo(Arc::new(current)).unwrap().unwrap();
        assert_eq!(previous.id, new_id);
        let restored = history.try_redo(previous).unwrap().unwrap();
        assert_eq!(restored.id, new_id);
        assert_ne!(original.id, new_id);
    }

    #[test]
    fn recovery_checkpoint_rejects_oversized_metadata() {
        let mut checkpoint = History::new(50).checkpoint();
        checkpoint.current_label = "x".repeat(4097);
        assert!(History::from_checkpoint(checkpoint).is_err());
        let mut checkpoint = History::new(50).checkpoint();
        let state = HistoryState::from_document("State", Arc::new(base()));
        checkpoint.undo = vec![state];
        checkpoint.current_identity = StateIdentity { id: 1, parent: Some(1), branch: 0, revision: 0 };
        assert!(History::from_checkpoint(checkpoint).is_err());
        let mut checkpoint = History::new(50).checkpoint();
        checkpoint.max_states = 0;
        assert!(History::from_checkpoint(checkpoint).is_ok());
        let mut checkpoint = History::default().checkpoint();
        checkpoint.current_identity.id = u64::MAX - 1;
        assert!(History::from_checkpoint(checkpoint).is_err(), "hostile identities must leave room for future edits");
    }

    #[test]
    fn cold_restore_and_failed_read_preserve_stacks() {
        let mut history = History::new(50);
        let mut current = Arc::new(base());
        let original = current.clone();
        edit(&mut history, &mut current, "A", |doc| doc.name = "a".into());
        let labels = history.entries();
        assert!(history.replace_resident(&original, Arc::new(Archive(Err("corrupt scratch blob".into())))));
        assert_eq!(history.archived_states(), 1);
        assert!(history.try_state(0).is_err());
        assert!(history.try_undo(current.clone()).is_err());
        assert_eq!(history.entries(), labels);
        assert!(!history.can_redo());
        history.clear();
        history.record("A", original.clone(), LayerTarget::default());
        assert!(history.replace_resident(&original, Arc::new(Archive(Ok(original.clone())))));
        let restored = history.try_undo(current.clone()).unwrap().unwrap();
        assert!(Arc::ptr_eq(&restored, &original));
        assert!(Arc::ptr_eq(&history.try_redo(restored).unwrap().unwrap(), &current));
    }

    #[test]
    fn replacement_deduplicates_and_stale_results_are_ignored() {
        let mut history = History::new(50);
        let original = Arc::new(base());
        history.record("A", original.clone(), LayerTarget::default());
        history.record("B", original.clone(), LayerTarget::default());
        assert!(history.spill_candidate().is_none(), "shared immediate undo allocation stays hot");
        assert!(history.replace_resident(&original, Arc::new(Archive(Ok(original.clone())))));
        assert_eq!(history.archived_states(), 2);
        assert!(history.resident_states().is_empty());
        assert!(!history.replace_resident(&original, Arc::new(Archive(Ok(original.clone())))));
        assert!(history.drop_oldest());
        assert!(!history.drop_oldest());
        history.clear();
        assert_eq!(history.archived_states(), 0);
    }

    #[test]
    fn selection_patterns_and_video_pixels_are_accounted_once() {
        let mut doc = base();
        let format = doc.pixel_format();
        let mut surface = photocraft_doc::Surface::new(format);
        surface.write_pixel(0, 0, &[0.25, 0.5, 0.75, 1.0]);
        doc.selection = Some(surface.clone());
        doc.patterns.push(photocraft_doc::Pattern::new("P", surface.clone(), 1, 1));
        doc.layers[0].video = Some(photocraft_doc::VideoData::new(vec![surface], 24.0));
        let mut seen = HashSet::new();
        assert_eq!(document_bytes(&doc, &mut seen), 2 * 256 * 256 * 4);
        assert_eq!(document_bytes(&doc, &mut seen), 0);
        // Original video frames are shared until an edit replaces one. The new allocation
        // must enter the same resident history budget without counting aliases twice.
        let mut original = doc.selection.clone().unwrap();
        original.write_pixel(0, 0, &[0.1, 0.2, 0.3, 1.0]);
        let video = doc.layers[0].video.as_mut().unwrap();
        video.frames.push(video.frames[0].clone());
        video.original_frames = Some(vec![video.frames[0].clone(), original]);
        seen.clear();
        assert_eq!(document_bytes(&doc, &mut seen), 3 * 256 * 256 * 4);
        assert_eq!(document_bytes(&doc, &mut seen), 0);
    }

    #[test]
    fn cold_redo_failure_keeps_the_next_step() {
        let mut history = History::new(50);
        let mut current = Arc::new(base());
        edit(&mut history, &mut current, "A", |doc| doc.name = "a".into());
        let future = current.clone();
        current = history.try_undo(current).unwrap().unwrap();
        assert!(history.replace_resident(&future, Arc::new(Archive(Err("read failed".into())))));
        let label = history.redo_label().map(str::to_owned);
        assert!(history.try_redo(current).is_err());
        assert_eq!(history.redo_label(), label.as_deref());
        assert!(!history.can_undo());
    }
    #[test]
    fn menu_queries_never_load_a_cold_selection() {
        let mut history = History::new(50);
        let mut doc = base();
        doc.selection = Some(photocraft_doc::Surface::new(doc.pixel_format()));
        let doc = Arc::new(doc);
        history.record("Select", doc.clone(), LayerTarget::default());
        assert!(history.replace_resident(&doc, Arc::new(Archive(Err("must not read".into())))));
        assert!(history.has_past_selection());
        assert_eq!(history.latest_selection_state(), Some(0));
        history.record("Clear selection", Arc::new(base()), LayerTarget::default());
        assert_eq!(history.latest_selection_state(), Some(0), "skip newer no-selection states without loading archives");
        assert!(history.resident_state(0).is_none());
        assert!(history.try_state(0).is_err());
    }

    #[test]
    fn spill_and_drop_keep_adjacent_steps_hot_and_contiguous() {
        let mut history = History::new(50);
        let mut current = Arc::new(base());
        let oldest = current.clone();
        for label in ["A", "B", "C"] {
            edit(&mut history, &mut current, label, |doc| doc.name = label.into());
        }
        assert!(Arc::ptr_eq(&history.spill_candidate().unwrap(), &oldest));
        assert!(history.drop_oldest());
        assert_eq!(history.entries(), ["A", "B", "C"]);
        current = history.try_undo(current).unwrap().unwrap();
        current = history.try_undo(current).unwrap().unwrap();
        assert_eq!(current.name, "A");
        assert!(history.drop_oldest(), "discard farthest redo");
        assert_eq!(history.redo_label(), Some("B"));
        assert!(!history.drop_oldest());
        assert_eq!(history.try_redo(current).unwrap().unwrap().name, "B");
    }
    #[test]
    fn explicit_budget_reduction_can_discard_final_cold_state() {
        let mut history = History::new(50);
        let doc = Arc::new(base());
        history.record("A", doc.clone(), LayerTarget::default());
        history.replace_resident(&doc, Arc::new(Archive(Err("must not load".into()))));
        assert!(!history.drop_oldest());
        assert!(history.discard_oldest());
        assert!(!history.can_undo());
        assert!(!history.discard_oldest());
    }

    #[test]
    fn lowered_count_bounds_both_stacks_without_loading() {
        let mut history = History::new(50);
        let mut current = Arc::new(base());
        for label in ["A", "B", "C"] {
            edit(&mut history, &mut current, label, |doc| doc.name = label.into());
        }
        current = history.undo(current).unwrap().0;
        history.max_states = 1;
        assert_eq!(history.enforce_state_limit(), 2);
        assert_eq!(history.past_len(), 0);
        assert_eq!(history.redo_label(), Some("C"));
        assert_eq!(history.redo(current).unwrap().0.name, "C");
    }
    #[test]
    fn asynchronous_candidate_can_become_hot_after_navigation() {
        let mut history = History::new(50);
        let mut current = Arc::new(base());
        let oldest = current.clone();
        for label in ["A", "B", "C"] {
            edit(&mut history, &mut current, label, |doc| doc.name = label.into());
        }
        assert!(!history.is_hot(&oldest));
        current = history.undo(current).unwrap().0;
        let _current = history.undo(current).unwrap().0;
        assert!(history.is_hot(&oldest));
        assert!(history.spill_candidate().is_none_or(|candidate| !Arc::ptr_eq(&candidate, &oldest)));
    }
    #[test]
    fn large_shared_binary_payloads_are_counted_once() {
        let mut doc = base();
        let bytes = Arc::new(vec![17; 4096]);
        doc.icc_profile = Some(bytes.clone());
        doc.metadata.exif = Some(bytes.clone());
        doc.metadata.psd_resources.push((1, "resource".into(), bytes.clone()));
        doc.metadata.psd_global_blocks.push((*b"8BIM", *b"lnk2", bytes.clone()));
        doc.layers[0].psd_blocks.push((*b"test", bytes));
        let baseline = 256 * 256 * 4;
        let mut seen = HashSet::new();
        assert_eq!(document_bytes(&doc, &mut seen), baseline + 4096);
        let mut clone = doc.clone();
        clone.metadata.exif = Some(Arc::new(vec![4; 1024]));
        assert_eq!(document_bytes(&clone, &mut seen), 1024);
    }
}

#[cfg(test)]
mod direct_history_tests {
    use super::*;
    use photocraft_doc::{ColorMode, SampleType, Size};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn doc() -> Arc<Document> {
        Arc::new(Document::new("Original", Size::new(2, 2), ColorMode::Rgb, SampleType::U8))
    }
    fn edit(history: &mut History, current: &mut Arc<Document>, label: &str) {
        history.record(label, current.clone(), LayerTarget::default());
        let mut next = (**current).clone();
        next.name = label.into();
        *current = Arc::new(next);
    }
    #[test]
    fn unlimited_retains_original_and_more_than_ten_thousand_states() {
        let mut history = History::default();
        let mut current = doc();
        for i in 0..10_050 {
            edit(&mut history, &mut current, &format!("Brush {i}"));
        }
        assert_eq!(history.past_len(), 10_050);
        let state = history.state_by_id(current.clone(), 0).unwrap();
        let original = state.load_document().unwrap();
        history.jump_loaded(current, 0, original.clone()).unwrap();
        assert_eq!(original.name, "Original");
        assert_eq!(history.timeline().len(), 10_051);
        assert!(History::from_checkpoint(history.checkpoint()).is_ok());
    }
    #[test]
    fn alternative_branches_and_cursor_survive_checkpoint_and_direct_jumps() {
        let mut history = History::default();
        let mut current = doc();
        edit(&mut history, &mut current, "Paint");
        edit(&mut history, &mut current, "Erase");
        let erased_id = history.current_id();
        current = history.try_undo(current).unwrap().unwrap();
        edit(&mut history, &mut current, "Move layer");
        let moved_id = history.current_id();
        assert!(!history.can_redo());
        assert_eq!(history.timeline().len(), 4);
        assert_ne!(history.timeline()[2].identity.branch, history.timeline()[3].identity.branch);
        let mut restored = History::from_checkpoint(history.checkpoint()).unwrap();
        let erased = restored.state_by_id(current.clone(), erased_id).unwrap().load_document().unwrap();
        restored.jump_loaded(current, erased_id, erased.clone()).unwrap();
        assert_eq!(erased.name, "Erase");
        let moved = restored.state_by_id(erased.clone(), moved_id).unwrap().load_document().unwrap();
        restored.jump_loaded(erased, moved_id, moved.clone()).unwrap();
        assert_eq!(moved.name, "Move layer");
        assert_eq!(restored.try_undo(moved).unwrap().unwrap().name, "Paint");
    }
    #[derive(Debug)]
    struct Counted {
        document: Arc<Document>,
        reads: Arc<AtomicUsize>,
        fail: bool,
    }
    impl ArchivedDocument for Counted {
        fn load(&self) -> Result<Arc<Document>, String> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            if self.fail { Err("corrupt archive".into()) } else { Ok(self.document.clone()) }
        }
    }
    #[test]
    fn jumping_over_cold_states_reads_only_the_destination_and_failures_are_atomic() {
        let mut history = History::default();
        let mut current = doc();
        for i in 0..80 {
            edit(&mut history, &mut current, &format!("Edit {i}"));
        }
        let reads = Arc::new(AtomicUsize::new(0));
        for state in &mut history.undo {
            state.document = StoredDocument::Archived(Arc::new(Counted { document: state.load_document().unwrap(), reads: reads.clone(), fail: false }));
        }
        let target = history.state_by_id(current.clone(), 0).unwrap().load_document().unwrap();
        history.jump_loaded(current.clone(), 0, target).unwrap();
        assert_eq!(reads.load(Ordering::Relaxed), 1);
        assert_eq!(history.timeline().len(), 81);
        let before = history.timeline_stamp();
        let bad = HistoryState::from_archive("Bad", false, Arc::new(Counted { document: current.clone(), reads: reads.clone(), fail: true }));
        assert!(bad.load_document().is_err());
        assert!(history.jump_loaded(current.clone(), u64::MAX, current).is_err());
        assert_eq!(history.timeline_stamp(), before);
    }
}
