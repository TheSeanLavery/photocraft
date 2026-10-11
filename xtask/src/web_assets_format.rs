//! Strict, bounded extraction of active static assets from a wasm-bindgen executable.
const MAX_MODULE_BYTES: usize = 64 << 20;
const PAGE_BYTES: u64 = 65_536;
const MARKER: &str = "photocraft.static-assets";
pub const PACK_VERSION: &str = "photocraft-static-assets-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub offset: usize,
    pub bytes: Vec<u8>,
}

pub struct Packed {
    pub executable: Vec<u8>,
    pub assets: Vec<u8>,
    pub segments: Vec<Segment>,
    pub memory_bytes: u64,
}

struct Section<'a> {
    id: u8,
    payload: &'a [u8],
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }
    fn byte(&mut self) -> Result<u8, String> {
        let byte = *self.bytes.get(self.pos).ok_or("Truncated WebAssembly input")?;
        self.pos = self.pos.checked_add(1).ok_or("WebAssembly input overflow")?;
        Ok(byte)
    }
    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(count).ok_or("WebAssembly length overflow")?;
        let bytes = self.bytes.get(self.pos..end).ok_or("Truncated WebAssembly payload")?;
        self.pos = end;
        Ok(bytes)
    }
    fn unsigned(&mut self) -> Result<u32, String> {
        let mut value = 0u32;
        for shift in (0..35).step_by(7) {
            let byte = self.byte()?;
            let payload = u32::from(byte & 0x7f);
            if shift == 28 && payload > 15 {
                return Err("WebAssembly u32 LEB overflow".into());
            }
            value |= payload << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err("WebAssembly u32 LEB too long".into())
    }
    fn signed(&mut self) -> Result<i32, String> {
        let mut value = 0i64;
        for shift in (0..35).step_by(7) {
            let byte = self.byte()?;
            value |= i64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                if byte & 0x40 != 0 {
                    value -= 1i64 << (shift + 7);
                }
                return i32::try_from(value).map_err(|_| "WebAssembly i32 LEB overflow".into());
            }
        }
        Err("WebAssembly i32 LEB too long".into())
    }
    fn name(&mut self) -> Result<&'a str, String> {
        let len = self.unsigned()? as usize;
        std::str::from_utf8(self.take(len)?).map_err(|e| e.to_string())
    }
    fn finish(&self) -> Result<(), String> {
        if self.pos == self.bytes.len() { Ok(()) } else { Err("Unexpected trailing WebAssembly payload".into()) }
    }
}

fn unsigned(mut value: usize, out: &mut Vec<u8>) {
    while value >= 128 {
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}
fn name(value: &str, out: &mut Vec<u8>) {
    unsigned(value.len(), out);
    out.extend_from_slice(value.as_bytes());
}
fn section(id: u8, payload: &[u8], out: &mut Vec<u8>) {
    out.push(id);
    unsigned(payload.len(), out);
    out.extend_from_slice(payload);
}
fn marker(role: &str, bundle: &str, out: &mut Vec<u8>) {
    let mut payload = Vec::new();
    name(MARKER, &mut payload);
    payload.extend_from_slice(format!("{PACK_VERSION}\n{role}\n{bundle}\n").as_bytes());
    section(0, &payload, out);
}

fn sections(bytes: &[u8]) -> Result<Vec<Section<'_>>, String> {
    if bytes.len() > MAX_MODULE_BYTES {
        return Err("WebAssembly module exceeds 64 MiB packing limit".into());
    }
    let mut reader = Reader::new(bytes);
    if reader.take(8)? != b"\0asm\x01\0\0\0" {
        return Err("Unsupported WebAssembly header/version".into());
    }
    let mut result = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut last_rank = 0;
    while reader.pos < bytes.len() {
        let id = reader.byte()?;
        if id > 13 || (id != 0 && !seen.insert(id)) {
            return Err("Unsupported or duplicate WebAssembly section".into());
        }
        if id != 0 {
            let rank = match id {
                13 => 6,
                6..=9 => id + 1,
                12 => 11,
                10 => 12,
                11 => 13,
                _ => id,
            };
            if rank <= last_rank {
                return Err("WebAssembly sections are out of order".into());
            }
            last_rank = rank;
        }
        let len = reader.unsigned()? as usize;
        let payload = reader.take(len)?;
        if id == 0 && Reader::new(payload).name()? == MARKER {
            return Err("WebAssembly input is already asset-packed".into());
        }
        result.push(Section { id, payload });
    }
    Ok(result)
}

fn segments(payload: &[u8], memory_bytes: u64) -> Result<(Vec<u8>, Vec<Segment>), String> {
    let mut reader = Reader::new(payload);
    let count = reader.unsigned()? as usize;
    if count > 100_000 {
        return Err("Too many WebAssembly data segments".into());
    }
    let mut empty = Vec::new();
    unsigned(count, &mut empty);
    let mut segments = Vec::new();
    for _ in 0..count {
        let start = reader.pos;
        match reader.unsigned()? {
            0 => (),
            2 if reader.unsigned()? == 0 => (),
            _ => return Err("Only active data segments in the default memory can be packed".into()),
        }
        if reader.byte()? != 0x41 {
            return Err("Static asset offset must be an i32.const".into());
        }
        let offset = u32::try_from(reader.signed()?).map_err(|_| "Negative static asset memory offset")?;
        if reader.byte()? != 0x0b {
            return Err("Unsupported static asset offset expression".into());
        }
        let header_end = reader.pos;
        let len = reader.unsigned()? as usize;
        if u64::from(offset).checked_add(len as u64).is_none_or(|end| end > memory_bytes) {
            return Err("Static asset extends outside initial memory".into());
        }
        let bytes = reader.take(len)?.to_vec();
        empty.extend_from_slice(payload.get(start..header_end).ok_or("Invalid data segment header")?);
        unsigned(0, &mut empty);
        segments.push(Segment { offset: offset as usize, bytes });
    }
    reader.finish()?;
    Ok((empty, segments))
}

/// Preserve executable sections and data slots, moving each exact active payload to an asset module.
/// A common digest of the original executable binds both modules without circular output hashes.
pub fn pack(bytes: &[u8], bundle: &str) -> Result<Packed, String> {
    if bundle.len() != 64 || !bundle.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("Invalid static asset bundle digest".into());
    }
    let sections = sections(bytes)?;
    if sections.iter().any(|s| s.id == 8) {
        return Err("A WebAssembly start section cannot run before static assets are loaded".into());
    }
    if let Some(imports) = sections.iter().find(|s| s.id == 2) {
        let mut reader = Reader::new(imports.payload);
        let count = reader.unsigned()?;
        if count > 100_000 {
            return Err("Too many WebAssembly imports".into());
        }
        for _ in 0..count {
            let _module = reader.name()?;
            let _name = reader.name()?;
            if reader.byte()? != 0 {
                return Err("Static asset packing requires function-only imports; imported memory/table/global/tag is unsupported".into());
            }
            let _function_type = reader.unsigned()?;
        }
        reader.finish()?;
    }
    let memory = sections.iter().find(|s| s.id == 5).ok_or("Executable has no defined memory")?;
    let mut reader = Reader::new(memory.payload);
    if reader.unsigned()? != 1 {
        return Err("Asset packing requires exactly one memory".into());
    }
    let limits_start = reader.pos;
    let flags = reader.unsigned()?;
    if flags > 1 {
        return Err("Shared/memory64 WebAssembly memories are not supported by asset packing".into());
    }
    let initial = reader.unsigned()?;
    if initial > 65_536 {
        return Err("Initial memory exceeds wasm32 limit".into());
    }
    if flags == 1 {
        let maximum = reader.unsigned()?;
        if maximum < initial || maximum > 65_536 {
            return Err("Invalid WebAssembly memory maximum".into());
        }
    }
    reader.finish()?;
    let limits = memory.payload.get(limits_start..).ok_or("Missing memory limits")?;
    let memory_bytes = u64::from(initial) * PAGE_BYTES;
    let exports = sections.iter().find(|s| s.id == 7).ok_or("Executable has no exports")?;
    let mut reader = Reader::new(exports.payload);
    let mut memory_export = false;
    let mut explicit_start = false;
    for _ in 0..reader.unsigned()? {
        let name = reader.name()?;
        let kind = reader.byte()?;
        let index = reader.unsigned()?;
        if name == "memory" && kind == 2 && index == 0 {
            memory_export = true;
        }
        if name == "__wbindgen_start" && kind == 0 {
            explicit_start = true;
        }
    }
    reader.finish()?;
    if !memory_export || !explicit_start {
        return Err("Expected wasm-bindgen memory and explicit start exports".into());
    }
    let data = sections.iter().find(|s| s.id == 11).ok_or("Executable has no static data")?;
    let (empty, segments) = segments(data.payload, memory_bytes)?;
    if let Some(count) = sections.iter().find(|s| s.id == 12) {
        let mut reader = Reader::new(count.payload);
        if reader.unsigned()? as usize != segments.len() {
            return Err("WebAssembly DataCount does not match segment slots".into());
        }
        reader.finish()?;
    }
    let mut executable = b"\0asm\x01\0\0\0".to_vec();
    for s in &sections {
        section(s.id, if s.id == 11 { &empty } else { s.payload }, &mut executable);
    }
    marker("executable", bundle, &mut executable);
    let mut imports = Vec::new();
    unsigned(1, &mut imports);
    name("env", &mut imports);
    name("memory", &mut imports);
    imports.push(2);
    imports.extend_from_slice(limits);
    let mut assets = b"\0asm\x01\0\0\0".to_vec();
    section(2, &imports, &mut assets);
    section(11, data.payload, &mut assets);
    marker("assets", bundle, &mut assets);
    Ok(Packed { executable, assets, segments, memory_bytes })
}

/// Validate the common original-module binding on compiled output bytes (including repeat runs).
pub fn validate_marker(bytes: &[u8], role: &str, bundle: &str) -> Result<(), String> {
    if bytes.len() > MAX_MODULE_BYTES {
        return Err("Packed module exceeds processing limit".into());
    }
    let mut reader = Reader::new(bytes);
    if reader.take(8)? != b"\0asm\x01\0\0\0" {
        return Err("Invalid packed WebAssembly header".into());
    }
    let expected = format!("{PACK_VERSION}\n{role}\n{bundle}\n");
    let mut matches = 0;
    while reader.pos < bytes.len() {
        let id = reader.byte()?;
        let len = reader.unsigned()? as usize;
        let payload = reader.take(len)?;
        if id == 0 {
            let mut custom = Reader::new(payload);
            if custom.name()? == MARKER {
                if custom.take(payload.len().saturating_sub(custom.pos))? != expected.as_bytes() {
                    return Err("Packed module belongs to a different static asset bundle".into());
                }
                matches += 1;
            }
        }
    }
    if matches == 1 { Ok(()) } else { Err("Missing or duplicate packed module binding".into()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(offset: &[u8], bytes: &[u8]) -> Vec<u8> {
        let mut module = b"\0asm\x01\0\0\0".to_vec();
        section(5, &[1, 0, 1], &mut module);
        let mut exports = vec![2];
        name("memory", &mut exports);
        exports.extend([2, 0]);
        name("__wbindgen_start", &mut exports);
        exports.extend([0, 0]);
        section(7, &exports, &mut module);
        let mut data = vec![1, 0, 0x41];
        data.extend(offset);
        data.push(0x0b);
        unsigned(bytes.len(), &mut data);
        data.extend(bytes);
        section(12, &[1], &mut module);
        section(11, &data, &mut module);
        module
    }
    #[test]
    fn signed_offsets_and_full_memory_are_preserved() {
        let module = fixture(&[0xc0, 0], b"\0font\xffshader locale"); // +64: sign bit requires second LEB byte.
        let packed = pack(&module, &"a".repeat(64)).unwrap();
        assert_eq!(packed.memory_bytes, 65_536);
        assert_eq!(packed.segments, vec![Segment { offset: 64, bytes: b"\0font\xffshader locale".to_vec() }]);
        let mut baseline = vec![0; packed.memory_bytes as usize];
        baseline[64..64 + b"\0font\xffshader locale".len()].copy_from_slice(b"\0font\xffshader locale");
        fn output_segments(module: &[u8], memory: u64) -> Vec<Segment> {
            let mut reader = Reader::new(module);
            reader.take(8).unwrap();
            while reader.pos < module.len() {
                let id = reader.byte().unwrap();
                let len = reader.unsigned().unwrap() as usize;
                let payload = reader.take(len).unwrap();
                if id == 11 {
                    return segments(payload, memory).unwrap().1;
                }
            }
            panic!("output has no data section");
        }
        let empty_slots = output_segments(&packed.executable, packed.memory_bytes);
        assert_eq!(empty_slots.len(), 1);
        assert_eq!(empty_slots[0].offset, 64);
        assert!(empty_slots[0].bytes.is_empty());
        let mut reconstructed = vec![0; baseline.len()];
        for segment in output_segments(&packed.assets, packed.memory_bytes) {
            reconstructed[segment.offset..segment.offset + segment.bytes.len()].copy_from_slice(&segment.bytes);
        }
        assert_eq!(baseline, reconstructed);
        validate_marker(&packed.executable, "executable", &"a".repeat(64)).unwrap();
        validate_marker(&packed.assets, "assets", &"a".repeat(64)).unwrap();
        assert!(validate_marker(&packed.assets, "assets", &"b".repeat(64)).is_err());
        assert!(validate_marker(&packed.executable, "assets", &"a".repeat(64)).is_err());
        // DataCount remains unchanged and every original active data slot remains with an empty payload.
        assert!(packed.executable.windows(9).any(|w| w == [11, 7, 1, 0, 0x41, 0xc0, 0, 11, 0]));
        assert!(packed.assets.windows(b"\0font\xffshader locale".len()).any(|w| w == b"\0font\xffshader locale"));
    }
    #[test]
    fn hostile_lengths_and_offsets_return_errors() {
        for offset in [&[0x7f][..], &[0xff, 0xff, 0xff, 0xff, 0x0f], &[0x80, 0x80, 0x80, 0x80, 0x80, 0], &[0x80, 0x80, 0x04]] {
            assert!(pack(&fixture(offset, b"payload"), &"a".repeat(64)).is_err());
        }
        let module = fixture(&[0], b"data");
        for end in 0..module.len() {
            assert!(pack(&module[..end], &"a".repeat(64)).is_err());
        }
        assert!(sections(b"\0asm\x01\0\0\0\x0b\xff\xff\xff\xff\x7f").is_err());
    }
    #[test]
    fn start_passive_datacount_and_repacking_are_rejected() {
        let mut module = fixture(&[0], b"data");
        section(8, &[0], &mut module);
        assert!(pack(&module, &"a".repeat(64)).is_err());
        let mut passive = fixture(&[0], b"data");
        let at = passive.windows(3).position(|x| x == [1, 0, 0x41]).unwrap();
        passive[at + 1] = 1;
        assert!(pack(&passive, &"a".repeat(64)).is_err());
        let mut wrong_count = fixture(&[0], b"data");
        let at = wrong_count.windows(3).position(|x| x == [12, 1, 1]).unwrap();
        wrong_count[at + 2] = 2;
        assert!(pack(&wrong_count, &"a".repeat(64)).is_err());
        let packed = pack(&fixture(&[0], b"data"), &"a".repeat(64)).unwrap();
        assert!(pack(&packed.executable, &"a".repeat(64)).is_err());
    }
    #[test]
    fn imported_memory_and_out_of_order_sections_are_rejected() {
        let original = fixture(&[0], b"data");
        let mut imported = b"\0asm\x01\0\0\0".to_vec();
        let mut imports = vec![1];
        name("env", &mut imports);
        name("memory", &mut imports);
        imports.extend([2, 0, 1]);
        section(2, &imports, &mut imported);
        imported.extend_from_slice(&original[8..]);
        assert!(pack(&imported, &"a".repeat(64)).err().unwrap().contains("function-only"));
        let mut unordered = original.clone();
        section(3, &[0], &mut unordered);
        assert!(pack(&unordered, &"a".repeat(64)).is_err());
    }
}
