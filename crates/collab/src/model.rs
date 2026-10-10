use photocraft_doc::LayerId;
use photocraft_paint::{BrushSettings, StrokePoint};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

pub const MAX_MEMBERS: usize = 64;
pub const MAX_CHUNK_POINTS: usize = 256;
pub const MAX_STROKE_POINTS: usize = 65_536;
pub const MAX_WIRE_BYTES: usize = 4 * 1024 * 1024;
pub const PROTOCOL_VERSION: u32 = 1;
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ProtocolError(pub String);
pub type Result<T> = std::result::Result<T, ProtocolError>;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Profile {
    pub name: String,
    pub color: [f32; 4],
    pub icon: Option<String>,
    pub show_name: bool,
    pub visible: bool,
}
impl Default for Profile {
    fn default() -> Self {
        Self { name: "Artist".into(), color: [0.2, 0.6, 1.0, 1.0], icon: None, show_name: true, visible: true }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Presence {
    pub profile: Profile,
    pub position: Option<[f64; 2]>,
    pub layer: Option<LayerId>,
    pub tool: String,
    pub speaking: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StickyNote {
    pub id: String,
    pub author: String,
    pub position: [f64; 2],
    pub text: String,
    pub collapsed: bool,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum VisibilityMode {
    Shared,
    #[default]
    Personal,
}
/// Private selection expressed as sparse horizontal coverage runs, never another user's selection.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SelectionRun {
    pub x: i32,
    pub y: i32,
    pub coverage: Vec<f32>,
}
fn default_zoom() -> f32 {
    1.0
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StrokeStart {
    #[serde(default = "default_zoom")]
    pub zoom: f32,
    pub id: String,
    pub layer: LayerId,
    pub brush: BrushSettings,
    pub selection: Vec<SelectionRun>,
    pub lock_transparency: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommandEdit {
    pub key: String,
    pub id: String,
    pub params: serde_json::Value,
    pub context: serde_json::Value,
    pub outcome: Option<crate::delta::DocumentDelta>,
    pub base_revision: u64,
    pub dependencies: Vec<LayerId>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Operation {
    Command { edit: Box<CommandEdit> },
    StrokeBegin { stroke: Box<StrokeStart> },
    StrokeChunk { id: String, sequence: u32, points: Vec<StrokePoint> },
    StrokeEnd { id: String, sequence: u32 },
    Undo,
    Redo,
    PurgeHistory,
    Presence { presence: Presence },
    Chat { text: String },
    Note { note: StickyNote },
    RemoveNote { id: String },
    Visibility { mode: VisibilityMode, layers: BTreeMap<LayerId, bool> },
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ClientMessage {
    pub version: u32,
    pub peer: String,
    pub nonce: u64,
    pub operation: Operation,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct HostMessage {
    pub revision: u64,
    pub message: ClientMessage,
    #[serde(default)]
    pub delta: Option<crate::delta::DocumentDelta>,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(default)]
    pub mapped_ids: BTreeMap<u64, u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoomState {
    pub code: String,
    pub host: String,
    pub local_peer: String,
    pub members: BTreeMap<String, Presence>,
    pub chat: VecDeque<(String, String)>,
    pub notes: BTreeMap<String, StickyNote>,
    pub visibility_mode: VisibilityMode,
    pub layer_visibility: BTreeMap<LayerId, bool>,
    pub show_cursors: bool,
    pub show_names: bool,
    pub show_notes: bool,
}
impl RoomState {
    pub fn new(code: String, host: String, local_peer: String) -> Self {
        Self {
            code,
            host,
            local_peer,
            members: BTreeMap::new(),
            chat: VecDeque::new(),
            notes: BTreeMap::new(),
            visibility_mode: VisibilityMode::Personal,
            layer_visibility: BTreeMap::new(),
            show_cursors: true,
            show_names: true,
            show_notes: true,
        }
    }
}
#[derive(Clone, Debug)]
struct Active {
    owner: String,
    sequence: u32,
    points: usize,
    last: Option<StrokePoint>,
    budget: ChunkBudget,
}
#[derive(Clone, Debug, Default)]
pub struct HostSequencer {
    pub revision: u64,
    nonces: BTreeMap<String, u64>,
    strokes: BTreeMap<String, Active>,
    pub log: VecDeque<HostMessage>,
    log_bytes: usize,
}
/// Measure a typed CBOR envelope without allocating its byte resources again.
pub fn serialized_size<T: Serialize>(value: &T) -> Result<usize> {
    #[derive(Default)]
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.checked_add(bytes.len()).ok_or_else(|| std::io::Error::other("serialized size overflow"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter::default();
    ciborium::ser::into_writer(value, &mut counter).map_err(|error| ProtocolError(error.to_string()))?;
    Ok(counter.0)
}
fn encoded_size(event: &HostMessage) -> usize {
    serialized_size(event).unwrap_or(MAX_WIRE_BYTES)
}
impl HostSequencer {
    /// Forget an authenticated departed connection without removing its drawing journal.
    /// The signaling service must revoke that connection's admission before calling this.
    pub fn forget_peer(&mut self, peer: &str) {
        self.nonces.remove(peer);
        self.strokes.retain(|_, stroke| stroke.owner != peer);
    }
    /// Transactional metadata copy; deliberately does not copy the potentially large replay log.
    pub fn candidate(&self) -> Self {
        Self { revision: self.revision, nonces: self.nonces.clone(), strokes: self.strokes.clone(), log: VecDeque::new(), log_bytes: 0 }
    }
    pub fn commit_candidate(&mut self, mut candidate: Self) {
        let events = std::mem::take(&mut candidate.log);
        candidate.log = std::mem::take(&mut self.log);
        candidate.log_bytes = self.log_bytes;
        for event in events {
            candidate.append_log(event);
        }
        *self = candidate;
    }
    fn append_log(&mut self, event: HostMessage) {
        self.log_bytes = self.log_bytes.saturating_add(encoded_size(&event));
        self.log.push_back(event);
        while self.log.len() > 4096 || self.log_bytes > 64 * 1024 * 1024 {
            if let Some(old) = self.log.pop_front() {
                self.log_bytes = self.log_bytes.saturating_sub(encoded_size(&old));
            } else {
                break;
            }
        }
    }

    /// Admission identity is supplied by the authenticated transport, never trusted from payload.
    pub fn accept(&mut self, identity: &str, message: ClientMessage) -> Result<Option<HostMessage>> {
        if message.version != PROTOCOL_VERSION || message.peer != identity || identity.is_empty() || identity.len() > 128 {
            return Err(ProtocolError("invalid protocol or peer identity".into()));
        }
        if self.nonces.get(identity).is_some_and(|n| message.nonce <= *n) {
            return Ok(None);
        }
        if self.nonces.len() >= MAX_MEMBERS && !self.nonces.contains_key(identity) {
            return Err(ProtocolError("room is full".into()));
        }
        validate(&message.operation)?;
        match &message.operation {
            Operation::StrokeBegin { stroke } => {
                if self.strokes.contains_key(&stroke.id) || self.strokes.len() >= MAX_MEMBERS {
                    return Err(ProtocolError("stroke id already active or stroke limit reached".into()));
                }
                self.strokes
                    .insert(stroke.id.clone(), Active { owner: identity.into(), sequence: 0, points: 0, last: None, budget: ChunkBudget::new(&stroke.brush) });
            }
            Operation::StrokeChunk { id, sequence, points } => {
                let s = self.strokes.get_mut(id).ok_or_else(|| ProtocolError("unknown stroke".into()))?;
                if s.owner != identity || *sequence != s.sequence {
                    return Err(ProtocolError("stroke ownership or sequence mismatch".into()));
                }
                if s.points.saturating_add(points.len()) > MAX_STROKE_POINTS {
                    return Err(ProtocolError("stroke point limit exceeded".into()));
                }
                s.budget.validate(s.last, points)?;
                let mut previous = s.last;
                for point in points {
                    if let Some(last) = previous
                        && (point.time < last.time || point.time - last.time > 1000.0 || (point.x - last.x).hypot(point.y - last.y) > 4096.0)
                    {
                        return Err(ProtocolError("stroke sample gap exceeds budget".into()));
                    }
                    previous = Some(*point);
                }
                s.last = previous;
                s.points += points.len();
                s.sequence = s.sequence.checked_add(1).ok_or_else(|| ProtocolError("sequence exhausted".into()))?;
            }
            Operation::StrokeEnd { id, sequence } => {
                let s = self.strokes.get(id).ok_or_else(|| ProtocolError("unknown stroke".into()))?;
                if s.owner != identity || *sequence != s.sequence {
                    return Err(ProtocolError("stroke ownership or sequence mismatch".into()));
                }
                self.strokes.remove(id);
            }

            _ => {}
        }
        self.revision = self.revision.checked_add(1).ok_or_else(|| ProtocolError("revision exhausted".into()))?;
        self.nonces.insert(identity.into(), message.nonce);
        let accepted = HostMessage { revision: self.revision, message, delta: None, result: None, mapped_ids: BTreeMap::new() };
        self.append_log(accepted.clone());
        Ok(Some(accepted))
    }
    pub fn replay_after(&self, revision: u64) -> Result<Vec<HostMessage>> {
        if self.log.front().is_some_and(|m| revision.saturating_add(1) < m.revision) {
            return Err(ProtocolError("checkpoint required: reconnect log expired".into()));
        }
        Ok(self.log.iter().filter(|m| m.revision > revision).cloned().collect())
    }
}
pub fn decode_client(bytes: &[u8]) -> Result<ClientMessage> {
    if bytes.len() > MAX_WIRE_BYTES {
        return Err(ProtocolError("message too large".into()));
    }
    let m: ClientMessage = serde_json::from_slice(bytes).map_err(|e| ProtocolError(e.to_string()))?;
    validate(&m.operation)?;
    Ok(m)
}
/// Conservative work bound before an input chunk enters the dab renderer.
#[derive(Clone, Copy, Debug)]
pub struct ChunkBudget {
    dab_count: f64,
    pixel_cost: f64,
    build_rate: f64,
}
impl ChunkBudget {
    pub fn new(b: &BrushSettings) -> Self {
        let count = if b.scattering.enabled { f64::from(b.scattering.count.clamp(1, 16)) } else { 1.0 };
        let diameter = f64::from(b.size);
        let dual = if b.dual_brush.enabled { f64::from(b.dual_brush.size).powi(2) * f64::from(b.dual_brush.count.clamp(1, 16)) } else { 0.0 };
        Self { dab_count: count, pixel_cost: diameter.powi(2) * count + dual, build_rate: if b.build_up { f64::from(b.build_up_rate) } else { 0.0 } }
    }
    pub fn validate(self, last: Option<StrokePoint>, points: &[StrokePoint]) -> Result<()> {
        let mut distance = 0.0;
        let mut previous = last;
        for point in points {
            if let Some(p) = previous {
                distance += (point.x - p.x).hypot(point.y - p.y);
            }
            previous = Some(*point);
        }
        let elapsed = last.or_else(|| points.first().copied()).zip(points.last().copied()).map_or(0.0, |(first, last)| (last.time - first.time).max(0.0));
        let steps = 1.0 + distance / 0.5 + elapsed * self.build_rate / 1000.0;
        if steps * self.dab_count > 32_768.0 || steps * self.pixel_cost > 128.0 * 1024.0 * 1024.0 {
            return Err(ProtocolError("stroke chunk exceeds rendering work budget; send shorter chunks".into()));
        }
        Ok(())
    }
}
fn valid_brush(b: &BrushSettings) -> bool {
    fn finite_json(v: &serde_json::Value) -> bool {
        match v {
            serde_json::Value::Null => false,
            serde_json::Value::Number(n) => n.is_u64() || n.is_i64() || n.as_f64().is_some_and(|f| f.abs() <= 1_000_000.0),
            serde_json::Value::Array(a) => a.iter().all(finite_json),
            serde_json::Value::Object(o) => o.values().all(finite_json),
            _ => true,
        }
    }
    let tile = |g: &photocraft_paint::GrayTile| g.width <= 1024 && g.height <= 1024 && g.is_valid();
    let tip = |t: &photocraft_paint::TipShape| match t {
        photocraft_paint::TipShape::Sampled(g) => tile(g),
        _ => true,
    };
    let pattern = match &b.texture.pattern {
        photocraft_paint::Pattern::Tile(g) => tile(g),
        photocraft_paint::Pattern::Procedural { size, .. } => *size <= 1024,
    };
    tip(&b.tip)
        && tip(&b.dual_brush.tip)
        && pattern
        && serde_json::to_value(b).is_ok_and(|v| finite_json(&v))
        && b.size <= 1024.0
        && b.build_up_rate <= 120.0
        && b.dual_brush.size <= 1024.0
        && b.dual_brush.scatter <= 10.0
        && b.scattering.scatter.jitter <= 10.0
}
pub fn validate(op: &Operation) -> Result<()> {
    let text = |s: &str, n: usize| !s.is_empty() && s.len() <= n;
    let finite = |p: [f64; 2]| p.iter().all(|v| v.is_finite() && v.abs() <= 1_000_000.0);
    let ok = match op {
        Operation::StrokeBegin { stroke: s } => {
            text(&s.id, 128)
                && s.zoom.is_finite()
                && (0.01..=64.0).contains(&s.zoom)
                && valid_brush(&s.brush)
                && s.brush.size.is_finite()
                && (0.1..=4096.0).contains(&s.brush.size)
                && s.brush.spacing.is_finite()
                && s.brush.spacing >= 0.001
                && s.selection.len() <= 65_536
                && s.selection.iter().map(|r| r.coverage.len()).sum::<usize>() <= 1_048_576
                && s.selection.iter().all(|r| r.coverage.iter().all(|c| c.is_finite() && (0.0..=1.0).contains(c)))
        }
        Operation::StrokeChunk { id, points, .. } => {
            text(id, 128)
                && !points.is_empty()
                && points.len() <= MAX_CHUNK_POINTS
                && points.iter().all(|p| {
                    finite([p.x, p.y])
                        && p.pressure.is_finite()
                        && (0.0..=1.0).contains(&p.pressure)
                        && p.tilt_x.is_finite()
                        && (-90.0..=90.0).contains(&p.tilt_x)
                        && p.tilt_y.is_finite()
                        && (-90.0..=90.0).contains(&p.tilt_y)
                        && p.rotation.is_finite()
                        && (0.0..=360.0).contains(&p.rotation)
                        && p.wheel.is_finite()
                        && (0.0..=1.0).contains(&p.wheel)
                        && p.time.is_finite()
                })
        }
        Operation::StrokeEnd { id, .. } | Operation::RemoveNote { id } => text(id, 128),
        Operation::Presence { presence: p } => {
            text(&p.profile.name, 64)
                && p.profile.icon.as_ref().is_none_or(|s| s.len() <= 64)
                && p.profile.color.iter().all(|c| c.is_finite() && (0.0..=1.0).contains(c))
                && p.position.is_none_or(finite)
                && p.tool.len() <= 64
        }
        Operation::Chat { text: s } => text(s, 4096),
        Operation::Note { note: n } => text(&n.id, 128) && text(&n.text, 4096) && finite(n.position),
        Operation::Visibility { layers, .. } => layers.len() <= 4096,
        Operation::Undo | Operation::Redo | Operation::PurgeHistory => true,
        Operation::Command { edit } => {
            text(&edit.id, 128)
                && edit.key.len() <= 256
                && edit.dependencies.len() <= 4096
                && serde_json::to_vec(&edit.params).is_ok_and(|b| b.len() <= MAX_WIRE_BYTES)
                && serde_json::to_vec(&edit.context).is_ok_and(|b| b.len() <= 64 * 1024 * 1024)
        }
    };
    if ok { Ok(()) } else { Err(ProtocolError("invalid or oversized collaboration operation".into())) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authentication_dedup_and_sequences() {
        let mut host = HostSequencer::default();
        let message = ClientMessage { version: 1, peer: "a".into(), nonce: 1, operation: Operation::Chat { text: "Hello".into() } };
        assert!(host.accept("b", message.clone()).is_err());
        assert!(host.accept("a", message.clone()).unwrap().is_some());
        assert!(host.accept("a", message).unwrap().is_none());
        assert_eq!(host.revision, 1);
        let message = ClientMessage {
            version: 1,
            peer: "a".into(),
            nonce: 2,
            operation: Operation::StrokeChunk { id: "missing".into(), sequence: 0, points: vec![StrokePoint::new(0.0, 0.0, 1.0)] },
        };
        assert!(host.accept("a", message).is_err());
        assert_eq!(host.revision, 1);
    }
    #[test]
    fn oversize_invalid_pressure_and_reconnect_gap() {
        assert!(decode_client(&vec![0; MAX_WIRE_BYTES + 1]).is_err());
        assert!(validate(&Operation::StrokeChunk { id: "s".into(), sequence: 0, points: vec![StrokePoint::new(0.0, 0.0, -1.0)] }).is_err());
        let mut host = HostSequencer::default();
        host.log.push_back(HostMessage {
            revision: 50,
            message: ClientMessage { version: 1, peer: "a".into(), nonce: 1, operation: Operation::Undo },
            delta: None,
            result: None,
            mapped_ids: BTreeMap::new(),
        });
        assert!(host.replay_after(0).is_err());
    }
}

/// Complete per-author history retained across a current-document checkpoint.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JournalCheckpoint {
    #[serde(with = "serde_bytes")]
    pub baseline: Vec<u8>,
    pub revision: u64,
    pub entries: Vec<ArchivedEdit>,
    pub redo: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub undo_floor: BTreeMap<String, usize>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ArchivedEdit {
    Stroke { start: Box<StrokeStart>, owner: String, chunks: Vec<Vec<StrokePoint>>, ended: bool, visible: bool },
    Command { edit: Box<CommandEdit>, owner: String, visible: bool },
}
