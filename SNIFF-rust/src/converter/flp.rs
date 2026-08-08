// flp.rs
// Everything that touches raw FLP binary bytes.
//
// Responsibilities:
//   - FLP header and event-stream framing (open_flp, EventReader, FlEvent)
//   - Raw note record parsing (parse_notes, FlNote)
//   - Full pattern+note extraction pass (parse_flp, Project)
//   - UTF-16LE text decoding used by pattern names (decode_fl_text)
//   - Low-level byte helpers (take, take_u16, take_u32, take_varint)
//
// Nothing in here knows about chart sections, JSON output, or the UI.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};

use super::types::{FlpInfo, PatternInfo, Progress, PROGRESS_BATCH};

// ---------------------------------------------------------------------------
// FLP event-id constants
// ---------------------------------------------------------------------------

pub(crate) const EVENT_NEW_PATTERN: u8 = 65;
// Variable-length ("text") events start at 192; pattern name is text-event 1.
pub(crate) const EVENT_PATTERN_NAME: u8 = 193;
pub(crate) const EVENT_FINE_TEMPO: u8 = 0x9c;
pub(crate) const EVENT_PATTERN_NOTES: u8 = 224;

pub(crate) const NOTE_RECORD_SIZE: usize = 24;

// ---------------------------------------------------------------------------
// Marker pitch constants
// ---------------------------------------------------------------------------
// These match the original SNIFF's fixed marker assignments and are not
// user-configurable (unlike alt_marker_pitch, which lives in ConversionPreset).

/// A note at this pitch marks a BPM change for its section:
/// `changeBPM`/`bpm` fields are included in that section's JSON output.
pub(crate) const PITCH_BPM_CHANGE: u8 = 56;

/// A note at this pitch sets the section's `altAnim` JSON field to true.
/// Distinct from `alt_marker_pitch`, which marks individual *notes*
/// (not whole sections) as "Alt Animation".
pub(crate) const PITCH_ALT_ANIM: u8 = 57;

/// A note at this pitch sets `mustHitSection` to true, and that value
/// carries forward until a PITCH_MUST_HIT_FALSE note (or the chart ends).
pub(crate) const PITCH_MUST_HIT_TRUE: u8 = 53;

/// A note at this pitch sets `mustHitSection` to false (see above).
pub(crate) const PITCH_MUST_HIT_FALSE: u8 = 54;

// ---------------------------------------------------------------------------
// Raw note record from the FLP
// ---------------------------------------------------------------------------

/// A single piano-roll note exactly as it appears in the FLP binary.
/// Field names and offsets verified against the original SNIFF's FLNote
/// struct in FL.cs.
#[derive(Clone, Copy)]
pub(crate) struct FlNote {
    pub position: u32,
    pub length: u32,
    /// MIDI pitch (low byte of the FLP pitch uint at offset 12).
    pub key: u8,
    /// Raw velocity (0–127, FL Studio default 100). Values below 64 (< 50%)
    /// are treated as forced sustains in the chart builder.
    pub velocity: u8,
    /// True when flags byte (record[19]) has bit 0x08 set — the piano-roll
    /// "portamento" toggle, repurposed by SNIFF as a per-note Alt Animation flag.
    pub portamento: bool,
}

// ---------------------------------------------------------------------------
// Full parse result
// ---------------------------------------------------------------------------

/// Everything extracted from one FLP file in a single event-loop pass:
/// global PPQ, optional tempo, the selected pattern's notes, and the full
/// pattern list (same data `inspect` returns, collected for free).
pub(crate) struct Project {
    pub ppq: u16,
    pub bpm: Option<f64>,
    pub notes: Vec<FlNote>,
    /// All patterns found in this FLP, in FL Studio creation order, with
    /// note counts and names. Collected in the same pass as note extraction
    /// so callers do not need a separate `inspect()` call.
    pub patterns: Vec<PatternInfo>,
}

// ---------------------------------------------------------------------------
// FLP event types
// ---------------------------------------------------------------------------

/// A single decoded FLP event.
/// Event IDs 0–63 carry a byte payload, 64–127 a word (u16), 128–191 a dword
/// (u32), and 192–255 a variable-length (varint-prefixed) byte slice.
pub(crate) enum FlEvent<'a> {
    #[allow(dead_code)]
    Byte(u8, u8),
    Word(u8, u16),
    Dword(u8, u32),
    Data(u8, &'a [u8]),
}

// ---------------------------------------------------------------------------
// Event reader
// ---------------------------------------------------------------------------

pub(crate) struct EventReader<'a> {
    pub(crate) bytes: &'a [u8],
    pub(crate) at: usize,
    pub(crate) end: usize,
}

impl<'a> EventReader<'a> {
    pub fn next(&mut self) -> Result<Option<FlEvent<'a>>> {
        if self.at >= self.end {
            return Ok(None);
        }
        let id = *take(self.bytes, &mut self.at, 1)?.first().unwrap();
        let event = match id {
            0..=63   => FlEvent::Byte(id, take(self.bytes, &mut self.at, 1)?[0]),
            64..=127  => FlEvent::Word(id, take_u16(self.bytes, &mut self.at)?),
            128..=191 => FlEvent::Dword(id, take_u32(self.bytes, &mut self.at)?),
            _ => {
                let len = take_varint(self.bytes, &mut self.at)?;
                FlEvent::Data(id, take(self.bytes, &mut self.at, len)?)
            }
        };
        Ok(Some(event))
    }
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Parses the FLhd header and returns the PPQ plus a reader positioned at
/// the start of the FLdt event stream. Shared by `parse_flp` and `inspect`
/// so the event-framing rules live in exactly one place.
pub(crate) fn open_flp(bytes: &[u8]) -> Result<(u16, EventReader<'_>)> {
    if bytes.get(..4) != Some(b"FLhd") {
        bail!("not an FL Studio project (missing FLhd)");
    }
    let mut at = 4;
    let header_len = take_u32(bytes, &mut at)? as usize;
    if header_len != 6 {
        bail!("unsupported FLP header length {header_len}");
    }
    let _type     = take_u16(bytes, &mut at)?;
    let _channels = take_u16(bytes, &mut at)?;
    let ppq       = take_u16(bytes, &mut at)?;
    // FLP may contain chunks before FLdt. Skip them without reading payloads.
    let data_end = loop {
        let id  = take(bytes, &mut at, 4)?;
        let len = take_u32(bytes, &mut at)? as usize;
        if id == b"FLdt" {
            break at.checked_add(len).context("FLdt length overflow")?;
        }
        take(bytes, &mut at, len)?;
    }
    .min(bytes.len());
    Ok((ppq, EventReader { bytes, at, end: data_end }))
}

/// Parses an FLP from an already-read byte slice.
///
/// Always collects the full pattern list (stored in `project.patterns`) in the
/// same event-loop pass as note extraction — no separate `inspect()` call needed.
///
/// `selected_pattern`:
///   - `Some(id)` — collect notes for that pattern; error if not found.
///   - `None` — patterns-only mode; no notes collected, no "not found" error.
///     Used by the Merge arm to resolve a name→ID mapping without a second
///     file read.
pub(crate) fn parse_flp(
    bytes: &[u8],
    selected_pattern: Option<u16>,
    progress: &mut dyn FnMut(Progress),
) -> Result<Project> {
    let (ppq, mut events) = open_flp(bytes)?;
    let mut project = Project {
        ppq,
        bpm: None,
        notes: Vec::new(),
        patterns: Vec::new(),
    };

    let mut order: Vec<u16> = Vec::new();
    let mut names: HashMap<u16, String> = HashMap::new();
    let mut note_counts: HashMap<u16, usize> = HashMap::new();
    let mut bpm_change_counts: HashMap<u16, usize> = HashMap::new();

    // Track the last-seen pattern ID rather than a boolean "active" flag.
    // FL Studio can emit EVENT_NEW_PATTERN multiple times between a pattern
    // header and its notes payload (e.g. for channel/instrument assignments),
    // which would clear a simple boolean flag before the notes arrive.
    // Tracking the ID and comparing at collection time matches inspect()'s
    // approach and handles this correctly.
    let mut current_pattern: Option<u16> = None;
    let mut pattern_found = false;

    while let Some(event) = events.next()? {
        match event {
            FlEvent::Word(EVENT_NEW_PATTERN, value) => {
                current_pattern = Some(value);
                if !order.contains(&value) {
                    order.push(value);
                }
                if selected_pattern == Some(value) {
                    pattern_found = true;
                }
            }
            FlEvent::Data(EVENT_PATTERN_NAME, payload) => {
                if let Some(id) = current_pattern {
                    let name = decode_fl_text(payload);
                    if !name.trim().is_empty() {
                        names.insert(id, name);
                    }
                }
            }
            FlEvent::Dword(EVENT_FINE_TEMPO, value) => {
                project.bpm = Some(value as f64 / 1000.0);
            }
            FlEvent::Data(EVENT_PATTERN_NOTES, payload) => {
                if let Some(id) = current_pattern {
                    let total = payload.len() / NOTE_RECORD_SIZE;
                    *note_counts.entry(id).or_insert(0) += total;
                    let bpm_markers = payload
                        .chunks_exact(NOTE_RECORD_SIZE)
                        .filter(|record| record[12] == PITCH_BPM_CHANGE)
                        .count();
                    if bpm_markers > 0 {
                        *bpm_change_counts.entry(id).or_insert(0) += bpm_markers;
                    }
                    if selected_pattern == Some(id) {
                        parse_notes(payload, &mut project.notes, progress)?;
                    }
                }
            }
            _ => {}
        }
    }

    project.patterns = order
        .into_iter()
        .map(|id| PatternInfo {
            name: names.remove(&id).unwrap_or_else(|| format!("Pattern {id}")),
            id,
            note_count: note_counts.remove(&id).unwrap_or(0),
            bpm_change_count: bpm_change_counts.remove(&id).unwrap_or(0),
        })
        .collect();

    if let Some(id) = selected_pattern {
        if !pattern_found {
            bail!("pattern {id} does not exist in this FLP — pick one from the pattern list");
        }
    }
    Ok(project)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Parses raw note records from an EVENT_PATTERN_NOTES payload into `destination`.
pub(crate) fn parse_notes(
    payload: &[u8],
    destination: &mut Vec<FlNote>,
    progress: &mut dyn FnMut(Progress),
) -> Result<()> {
    if payload.len() % NOTE_RECORD_SIZE != 0 {
        bail!("pattern-note payload has invalid length {}", payload.len());
    }
    let total = payload.len() / NOTE_RECORD_SIZE;
    destination.reserve(total);
    for (index, record) in payload.chunks_exact(NOTE_RECORD_SIZE).enumerate() {
        // FLP piano-roll note record layout (24 bytes, all little-endian),
        // verified against the original SNIFF's FLNote struct (FL.cs):
        //  0– 3: Time / position (uint)
        //  4– 5: TBD (ushort)
        //  6– 7: ChannelNo (ushort)
        //  8–11: Duration / length (uint)
        // 12–15: Pitch (uint) — low byte is the MIDI key; upper bytes unused here
        // 16–17: FinePitch (ushort)
        // 18:    Release (byte)
        // 19:    Flags (byte) — bit 0x08 = portamento
        // 20:    Panning (byte)
        // 21:    Velocity (byte, 0–127, default 100)
        // 22:    ModX (byte)
        // 23:    ModY (byte)
        destination.push(FlNote {
            position:  u32::from_le_bytes(record[0..4].try_into().unwrap()),
            length:    u32::from_le_bytes(record[8..12].try_into().unwrap()),
            key:       record[12],
            portamento: record[19] & 0x08 != 0,
            velocity:  record[21],
        });
        // Emit progress in batches (see PROGRESS_BATCH in types.rs) so the
        // UI bar stays responsive without flooding the channel.
        if total > 0 && (index % PROGRESS_BATCH == 0 || index + 1 == total) {
            progress(Progress::Notes { done: index + 1, total });
        }
    }
    Ok(())
}

/// FL Studio 12+ stores text events as null-terminated UTF-16LE.
pub(crate) fn decode_fl_text(payload: &[u8]) -> String {
    let units: Vec<u16> = payload
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .take_while(|&unit| unit != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

// ---------------------------------------------------------------------------
// Byte-level primitives
// ---------------------------------------------------------------------------

pub(crate) fn take<'a>(data: &'a [u8], at: &mut usize, count: usize) -> Result<&'a [u8]> {
    let end = at.checked_add(count).context("FLP offset overflow")?;
    let slice = data.get(*at..end).context("truncated FLP")?;
    *at = end;
    Ok(slice)
}

pub(crate) fn take_u16(data: &[u8], at: &mut usize) -> Result<u16> {
    Ok(u16::from_le_bytes(take(data, at, 2)?.try_into().unwrap()))
}

pub(crate) fn take_u32(data: &[u8], at: &mut usize) -> Result<u32> {
    Ok(u32::from_le_bytes(take(data, at, 4)?.try_into().unwrap()))
}

pub(crate) fn take_varint(data: &[u8], at: &mut usize) -> Result<usize> {
    let mut result = 0usize;
    // SNIFF accepts as many continuation bytes as the integer representation
    // permits. FL Studio normally uses at most five, but newer/third-party
    // writers may emit a wider encoding.
    for shift in (0..usize::BITS).step_by(7) {
        let byte = take(data, at, 1)?[0];
        result |= ((byte & 0x7f) as usize)
            .checked_shl(shift)
            .context("FLP variable-length integer overflow")?;
        if byte & 0x80 == 0 {
            return Ok(result);
        }
    }
    bail!("invalid FLP variable-length integer")
}

// ---------------------------------------------------------------------------
// Public inspection API (used by inspect.rs)
// ---------------------------------------------------------------------------

/// Scans an FLP file's event stream and returns pattern metadata and BPM.
/// Does not extract note data beyond counting — much cheaper than parse_flp
/// when only metadata is needed.
pub fn inspect_bytes(bytes: &[u8]) -> Result<FlpInfo> {
    let (_ppq, mut events) = open_flp(bytes)?;

    let mut order: Vec<u16> = Vec::new();
    let mut names: HashMap<u16, String> = HashMap::new();
    let mut current: Option<u16> = None;
    let mut bpm: Option<f64> = None;
    let mut counts: HashMap<u16, usize> = HashMap::new();
    let mut bpm_change_counts: HashMap<u16, usize> = HashMap::new();

    while let Some(event) = events.next()? {
        match event {
            FlEvent::Word(EVENT_NEW_PATTERN, value) => {
                if !order.contains(&value) {
                    order.push(value);
                }
                current = Some(value);
            }
            FlEvent::Data(EVENT_PATTERN_NAME, payload) => {
                if let Some(id) = current {
                    let name = decode_fl_text(payload);
                    if !name.trim().is_empty() {
                        names.insert(id, name);
                    }
                }
            }
            FlEvent::Data(EVENT_PATTERN_NOTES, payload) => {
                if let Some(id) = current {
                    let notes = payload.len() / NOTE_RECORD_SIZE;
                    *counts.entry(id).or_insert(0) += notes;
                    let bpm_markers = payload
                        .chunks_exact(NOTE_RECORD_SIZE)
                        .filter(|record| record[12] == PITCH_BPM_CHANGE)
                        .count();
                    if bpm_markers > 0 {
                        *bpm_change_counts.entry(id).or_insert(0) += bpm_markers;
                    }
                }
            }
            FlEvent::Dword(EVENT_FINE_TEMPO, value) => {
                bpm = Some(value as f64 / 1000.0);
            }
            _ => {}
        }
    }

    let patterns = order
        .into_iter()
        .map(|id| PatternInfo {
            name: names.remove(&id).unwrap_or_else(|| format!("Pattern {id}")),
            id,
            note_count: counts.remove(&id).unwrap_or(0),
            bpm_change_count: bpm_change_counts.remove(&id).unwrap_or(0),
        })
        .collect();
    Ok(FlpInfo { bpm, patterns })
}
// ---------------------------------------------------------------------------
// FSC support
// ---------------------------------------------------------------------------

/// Returns true if `bytes` looks like an FL Studio Score file rather than
/// a full FLP project. Both share the `FLhd` magic and 6-byte header, but
/// an FSC has a non-zero format field (offset 8, u16 LE) whereas an FLP
/// always has format = 0. We use this to transparently route `.fsc` inputs
/// without requiring a separate file-extension check in every call site.
pub fn is_fsc(bytes: &[u8]) -> bool {
    // Bytes 0-3: "FLhd", 4-7: header size (6), 8-9: format field.
    // FLP = 0x0000, FSC = anything else (observed 0x0010 in the wild).
    if bytes.len() < 10 {
        return false;
    }
    let format = u16::from_le_bytes([bytes[8], bytes[9]]);
    format != 0
}

/// Parses an FL Studio Score file (.fsc) from an already-read byte slice.
/// An FSC has the same FLhd/FLdt framing as an FLP, but FLdt's payload is
/// raw packed 24-byte note records with no event-type wrapping — no pattern
/// events, no tempo event. PPQ is read from the header. BPM must come from
/// the caller (i.e. `base_bpm_override` in the preset).
pub(crate) fn parse_fsc(
    bytes: &[u8],
    progress: &mut dyn FnMut(Progress),
) -> Result<Project> {
    // open_flp reads FLhd and positions the EventReader at the start of FLdt.
    // We don't run the event loop — instead we grab the raw FLdt bytes and
    // feed them straight to parse_notes, bypassing the event framing entirely.
    let (ppq, reader) = open_flp(bytes)?;

    // The EventReader's `at` field points to the first byte of FLdt payload
    // and `end` points one past the last. Extract the raw slice directly.
    let payload = &reader.bytes[reader.at..reader.end];

    let mut notes = Vec::new();
    parse_notes(payload, &mut notes, progress)?;

    // FSC has no pattern list and no tempo event — return a synthetic Project
    // with a single unnamed pattern so the rest of the pipeline can treat it
    // uniformly. BPM stays None; the caller must supply base_bpm_override.
    let note_count = notes.len();
    Ok(Project {
        ppq,
        bpm: None,
        notes,
        patterns: vec![PatternInfo {
            id: 1,
            name: "Score".to_owned(),
            note_count,
            bpm_change_count: 0,
        }],
    })
}
