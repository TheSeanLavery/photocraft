//! Shared native/bot wire format with bounded reliable transfer fragmentation.
use crate::{ClientMessage, HostMessage, JournalCheckpoint, Presence, ProtocolError, Result, RoomState};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
pub const MAX_TRANSFER: usize = 512 * 1024 * 1024;
pub const FRAGMENT_BYTES: usize = 16 * 1024;
pub const MAX_PACKET: usize = 48 * 1024;
const MAX_BUFFERED: usize = 512 * 1024 * 1024;
const HEADER: usize = 24;
const PACKET_MAGIC: &[u8; 4] = b"PCF2";
const MESSAGE_MAGIC: &[u8; 4] = b"PCW2";
// Public typed protocol envelopes remain value-based; outgoing queues retain encoded shared buffers.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Wire {
    Bootstrap {
        #[serde(with = "serde_bytes")]
        document: Vec<u8>,
        events: Vec<HostMessage>,
        room: RoomState,
    },
    Checkpoint {
        #[serde(with = "serde_bytes")]
        document: Vec<u8>,
        room: RoomState,
        checkpoint: JournalCheckpoint,
    },
    Client(ClientMessage),
    Host(HostMessage),
    Presence {
        peer: String,
        presence: Presence,
    },
    Voice {
        peer: String,
        samples: Vec<u8>,
    },
    Reject(String),
    Left(String),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Fragment {
    pub id: u64,
    pub index: usize,
    pub total: usize,
    #[serde(with = "serde_bytes")]
    pub bytes: Vec<u8>,
}
struct Transfer {
    id: u64,
    next: usize,
    total: usize,
    bytes: Vec<u8>,
}
#[derive(Default)]
pub struct Decoder {
    transfers: HashMap<String, Transfer>,
}
impl Decoder {
    /// Reliable ordered packets, keyed by authenticated transport peer identity.
    pub fn push(&mut self, peer: &str, bytes: &[u8]) -> Result<Option<Wire>> {
        let result = self.push_inner(peer, bytes);
        if result.is_err() {
            self.transfers.remove(peer);
        }
        result
    }
    pub fn remove(&mut self, peer: &str) {
        self.transfers.remove(peer);
    }
    fn push_inner(&mut self, peer: &str, bytes: &[u8]) -> Result<Option<Wire>> {
        if peer.len() > 128 || bytes.len() > MAX_PACKET {
            return Err(ProtocolError("fragment peer or packet exceeds limit".into()));
        }
        let fragment = decode_fragment(bytes)?;
        if fragment.bytes.len() > FRAGMENT_BYTES
            || fragment.total == 0
            || fragment.total > MAX_TRANSFER.div_ceil(FRAGMENT_BYTES)
            || fragment.index >= fragment.total
        {
            return Err(ProtocolError("invalid transfer fragment".into()));
        }
        if fragment.index == 0 {
            if self.transfers.contains_key(peer) {
                return Err(ProtocolError("previous transfer interrupted".into()));
            }
            if self.transfers.len() >= 64 {
                return Err(ProtocolError("too many concurrent transfers".into()));
            }
            self.transfers.insert(peer.into(), Transfer { id: fragment.id, next: 0, total: fragment.total, bytes: Vec::new() });
        }
        let buffered = self.transfers.values().map(|t| t.bytes.len()).fold(0usize, usize::saturating_add);
        if buffered.saturating_add(fragment.bytes.len()) > MAX_BUFFERED {
            return Err(ProtocolError("checkpoint memory budget exceeded".into()));
        }
        let transfer = self.transfers.get_mut(peer).ok_or_else(|| ProtocolError("missing transfer start".into()))?;
        if transfer.id != fragment.id || transfer.next != fragment.index || transfer.total != fragment.total {
            return Err(ProtocolError("transfer sequence mismatch".into()));
        }
        if transfer.bytes.len().saturating_add(fragment.bytes.len()) > MAX_TRANSFER {
            return Err(ProtocolError("transfer exceeds memory limit".into()));
        }
        transfer.bytes.try_reserve(fragment.bytes.len()).map_err(|_| ProtocolError("cannot allocate checkpoint".into()))?;
        transfer.bytes.extend(fragment.bytes);
        transfer.next = transfer.next.saturating_add(1);
        if transfer.next != transfer.total {
            return Ok(None);
        }
        let transfer = self.transfers.remove(peer).ok_or_else(|| ProtocolError("transfer disappeared".into()))?;
        decode_message(&transfer.bytes).map(Some)
    }
}
/// Bounded serialization; byte resources stay byte strings rather than numeric JSON arrays.
pub fn encode_message(wire: &Wire) -> Result<Vec<u8>> {
    let mut writer = LimitedWriter { bytes: MESSAGE_MAGIC.to_vec(), limit: MAX_TRANSFER };
    ciborium::ser::into_writer(wire, &mut writer).map_err(|e| ProtocolError(e.to_string()))?;
    Ok(writer.bytes)
}
pub fn decode_message(bytes: &[u8]) -> Result<Wire> {
    if bytes.len() > MAX_TRANSFER {
        return Err(ProtocolError("transfer exceeds 512 MiB".into()));
    }
    if let Some(body) = bytes.strip_prefix(MESSAGE_MAGIC) {
        preflight_cbor(body)?;
        let mut reader = body;
        let wire = ciborium::de::from_reader_with_recursion_limit(&mut reader, 128).map_err(|e| ProtocolError(e.to_string()))?;
        if !reader.is_empty() {
            return Err(ProtocolError("trailing bytes after CBOR message".into()));
        }
        Ok(wire)
    } else {
        serde_json::from_slice(bytes).map_err(|e| ProtocolError(e.to_string()))
    }
}
struct LimitedWriter {
    bytes: Vec<u8>,
    limit: usize,
}
impl std::io::Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.bytes.len().saturating_add(bytes.len()) > self.limit {
            return Err(std::io::Error::other("transfer exceeds 512 MiB"));
        }
        self.bytes.try_reserve(bytes.len()).map_err(|_| std::io::Error::other("cannot allocate transfer"))?;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn read_u32(bytes: &[u8], range: std::ops::Range<usize>) -> Result<u32> {
    let raw: [u8; 4] = bytes
        .get(range)
        .ok_or_else(|| ProtocolError("short fragment header".into()))?
        .try_into()
        .map_err(|_| ProtocolError("invalid fragment header".into()))?;
    Ok(u32::from_be_bytes(raw))
}
fn decode_fragment(bytes: &[u8]) -> Result<Fragment> {
    if !bytes.starts_with(PACKET_MAGIC) {
        return serde_json::from_slice(bytes).map_err(|e| ProtocolError(e.to_string()));
    }
    let raw: [u8; 8] = bytes
        .get(4..12)
        .ok_or_else(|| ProtocolError("short fragment header".into()))?
        .try_into()
        .map_err(|_| ProtocolError("invalid fragment header".into()))?;
    let length = usize::try_from(read_u32(bytes, 20..24)?).map_err(|_| ProtocolError("fragment length overflow".into()))?;
    let payload = bytes.get(HEADER..).filter(|payload| payload.len() == length).ok_or_else(|| ProtocolError("fragment payload length mismatch".into()))?;
    Ok(Fragment {
        id: u64::from_be_bytes(raw),
        index: usize::try_from(read_u32(bytes, 12..16)?).map_err(|_| ProtocolError("fragment index overflow".into()))?,
        total: usize::try_from(read_u32(bytes, 16..20)?).map_err(|_| ProtocolError("fragment count overflow".into()))?,
        bytes: payload.to_vec(),
    })
}
pub fn fragment_message(id: u64, bytes: &[u8], index: usize) -> Result<Vec<u8>> {
    if bytes.len() > MAX_TRANSFER || bytes.is_empty() {
        return Err(ProtocolError("invalid transfer size".into()));
    }
    let total = bytes.len().div_ceil(FRAGMENT_BYTES);
    let start = index.checked_mul(FRAGMENT_BYTES).ok_or_else(|| ProtocolError("fragment offset overflow".into()))?;
    let end = start.saturating_add(FRAGMENT_BYTES).min(bytes.len());
    let payload = bytes.get(start..end).filter(|_| index < total).ok_or_else(|| ProtocolError("fragment index out of range".into()))?;
    let mut packet = Vec::with_capacity(HEADER + payload.len());
    packet.extend_from_slice(PACKET_MAGIC);
    packet.extend_from_slice(&id.to_be_bytes());
    for value in [index, total, payload.len()] {
        packet.extend_from_slice(&u32::try_from(value).map_err(|_| ProtocolError("fragment header overflow".into()))?.to_be_bytes());
    }
    packet.extend_from_slice(payload);
    Ok(packet)
}
pub fn encode_reliable(id: u64, wire: &Wire) -> Result<Vec<Vec<u8>>> {
    let bytes = encode_message(wire)?;
    (0..bytes.len().div_ceil(FRAGMENT_BYTES)).map(|index| fragment_message(id, &bytes, index)).collect()
}
pub fn encode_unreliable(wire: &Wire) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(wire).map_err(|e| ProtocolError(e.to_string()))?;
    if bytes.len() > MAX_PACKET {
        return Err(ProtocolError("unreliable packet exceeds limit".into()));
    }
    Ok(bytes)
}
pub fn decode_unreliable(bytes: &[u8]) -> Result<Wire> {
    if bytes.len() > MAX_PACKET {
        return Err(ProtocolError("unreliable packet exceeds limit".into()));
    }
    serde_json::from_slice(bytes).map_err(|e| ProtocolError(e.to_string()))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragmented_native_wire_round_trip_and_malformed_recovery() {
        let message = "payload".repeat(4000);
        let packets = encode_reliable(12, &Wire::Reject(message.clone())).unwrap();
        assert!(packets.len() > 1);
        let mut decoder = Decoder::default();
        let mut completed = None;
        for packet in &packets {
            completed = decoder.push("host", packet).unwrap();
        }
        assert!(matches!(completed,Some(Wire::Reject(s)) if s==message));
        assert!(decoder.push("host", packets.get(1).unwrap()).is_err());
        for packet in packets {
            let _ = decoder.push("host", &packet).unwrap();
        }
    }
    #[test]
    fn binary_resource_bytes_are_compact_and_round_trip() {
        let document = vec![193; 1024 * 1024];
        let room = RoomState::new("TEST".into(), "host".into(), "guest".into());
        let encoded = encode_message(&Wire::Bootstrap { document: document.clone(), events: Vec::new(), room }).unwrap();
        assert!(encoded.len() < document.len() + 4096);
        assert!(matches!(decode_message(&encoded).unwrap(), Wire::Bootstrap { document: bytes, .. } if bytes == document));
    }
    #[test]
    fn generic_command_json_values_survive_binary_wire() {
        let params = serde_json::json!({"amount":0.125,"enabled":true,"nested":[null,{"value":"text"}],"ids":[1,2,3]});
        let edit = crate::CommandEdit {
            key: "command".into(),
            id: "image.adjustments.brightnessContrast".into(),
            params: params.clone(),
            context: serde_json::json!({}),
            outcome: None,
            base_revision: 0,
            dependencies: Vec::new(),
        };
        let original = Wire::Client(ClientMessage {
            version: crate::PROTOCOL_VERSION,
            peer: "guest".into(),
            nonce: 1,
            operation: crate::Operation::Command { edit: Box::new(edit) },
        });
        let bytes = encode_message(&original).unwrap();
        assert!(
            matches!(decode_message(&bytes).unwrap(),Wire::Client(ClientMessage { operation:crate::Operation::Command { edit },.. }) if edit.params == params)
        );
    }
    #[test]
    fn rejects_forged_cbor_lengths_depth_and_trailing_bytes() {
        for forged in [vec![0x5b, 255, 255, 255, 255, 255, 255, 255, 255], vec![0x9b, 255, 255, 255, 255, 255, 255, 255, 255], vec![0xbf, 0x61, b'a', 255]] {
            let mut message = MESSAGE_MAGIC.to_vec();
            message.extend(forged);
            assert!(decode_message(&message).is_err());
        }
        let mut deep = MESSAGE_MAGIC.to_vec();
        deep.extend(vec![0x81; 130]);
        deep.push(0);
        assert!(decode_message(&deep).is_err());
        let mut valid = encode_message(&Wire::Reject("valid".into())).unwrap();
        valid.push(0);
        assert!(decode_message(&valid).is_err());
    }
    #[test]
    fn rejects_oversize_and_missing_start() {
        let mut d = Decoder::default();
        assert!(d.push("host", &vec![0; MAX_PACKET + 1]).is_err());
        let packet = serde_json::to_vec(&Fragment { id: 1, index: 1, total: 2, bytes: vec![1] }).unwrap();
        assert!(d.push("host", &packet).is_err());
    }
}

/// Reject forged lengths and excessive nesting before any schema visitor can allocate.
fn preflight_cbor(bytes: &[u8]) -> Result<()> {
    fn invalid() -> ProtocolError {
        ProtocolError("invalid or excessive CBOR structure".into())
    }
    fn item(bytes: &[u8], offset: &mut usize, depth: usize, nodes: &mut usize) -> Result<()> {
        if depth > 128 || *nodes >= 8_000_000 {
            return Err(invalid());
        }
        *nodes += 1;
        let head = *bytes.get(*offset).ok_or_else(invalid)?;
        *offset = offset.checked_add(1).ok_or_else(invalid)?;
        let major = head >> 5;
        let info = head & 31;
        let argument = match info {
            0..=23 => Some(u64::from(info)),
            24..=27 => {
                let length = 1usize << (info - 24);
                let end = offset.checked_add(length).ok_or_else(invalid)?;
                let raw = bytes.get(*offset..end).ok_or_else(invalid)?;
                *offset = end;
                Some(raw.iter().fold(0u64, |value, byte| (value << 8) | u64::from(*byte)))
            }
            31 => None,
            _ => return Err(invalid()),
        };
        match major {
            0 | 1 => {
                if argument.is_none() {
                    return Err(invalid());
                }
            }
            2 | 3 => {
                if let Some(length) = argument {
                    let length = usize::try_from(length).map_err(|_| invalid())?;
                    let end = offset.checked_add(length).ok_or_else(invalid)?;
                    if end > bytes.len() {
                        return Err(invalid());
                    }
                    *offset = end;
                } else {
                    loop {
                        let next = *bytes.get(*offset).ok_or_else(invalid)?;
                        if next == 255 {
                            *offset += 1;
                            break;
                        }
                        if next >> 5 != major || next & 31 == 31 {
                            return Err(invalid());
                        }
                        item(bytes, offset, depth + 1, nodes)?;
                    }
                }
            }
            4 | 5 => {
                if let Some(count) = argument {
                    let count = usize::try_from(count).map_err(|_| invalid())?;
                    let count = if major == 5 { count.checked_mul(2).ok_or_else(invalid)? } else { count };
                    if count > bytes.len().saturating_sub(*offset) {
                        return Err(invalid());
                    }
                    for _ in 0..count {
                        item(bytes, offset, depth + 1, nodes)?;
                    }
                } else {
                    let mut count = 0usize;
                    loop {
                        if bytes.get(*offset) == Some(&255) {
                            *offset += 1;
                            break;
                        }
                        item(bytes, offset, depth + 1, nodes)?;
                        count += 1;
                    }
                    if major == 5 && !count.is_multiple_of(2) {
                        return Err(invalid());
                    }
                }
            }
            6 => {
                if argument.is_none() {
                    return Err(invalid());
                }
                item(bytes, offset, depth + 1, nodes)?;
            }
            7 => {
                if argument.is_none() {
                    return Err(invalid());
                }
            }
            _ => return Err(invalid()),
        }
        Ok(())
    }
    let mut offset = 0;
    item(bytes, &mut offset, 0, &mut 0)?;
    if offset != bytes.len() {
        return Err(invalid());
    }
    Ok(())
}
/// Resource blobs are encoded as CBOR byte strings while remaining JSON-compatible.
pub mod resource_bytes {
    use serde::{Deserialize, Deserializer, Serializer, ser::SerializeMap};
    use std::collections::BTreeMap;
    pub fn serialize<S: Serializer>(resources: &BTreeMap<String, Vec<u8>>, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(resources.len()))?;
        for (key, bytes) in resources {
            map.serialize_entry(key, serde_bytes::Bytes::new(bytes))?;
        }
        map.end()
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> std::result::Result<BTreeMap<String, Vec<u8>>, D::Error> {
        let bytes = BTreeMap::<String, serde_bytes::ByteBuf>::deserialize(deserializer)?;
        Ok(bytes.into_iter().map(|(key, value)| (key, value.into_vec())).collect())
    }
}
