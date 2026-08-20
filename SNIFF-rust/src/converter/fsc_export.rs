// fsc_export.rs
// Converts a Psych Engine chart JSON into an FL Studio Score (.fsc) file.
//
// Direction: Chart JSON -> FSC
//
// The FSC format is structurally identical to an FLP up through the header,
// but FLdt's payload is raw packed 24-byte note records with no event wrapper.
// PPQ is always written as 96 (FL Studio's default for score files).
//
// Pitch mapping is always SNIFF's default layout regardless of the preset:
//   data 0-3 (BF lanes)       -> pitches 48-51
//   data 4-7 (opponent lanes) -> pitches 60-63
// mustHitSection flips which data range is BF vs opponent, exactly as in the
// forward direction.
//
// BPM is read from song.bpm in the chart JSON — no user input needed.
// Sustain lengths in ms are converted back to ticks using that BPM and PPQ=96.

use std::{
    fs,
    io::{BufWriter, Write},
    path::Path,
    time::Instant,
};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use super::types::{Progress, Stats};

use num_format::{Locale, ToFormattedString};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// PPQ written into every FSC output. FL Studio's default for score files.
const FSC_PPQ: u16 = 96;

/// FSC format field (offset 8 in FLhd). Distinguishes FSC from FLP (0x0000).
/// 0x0010 is the value observed in real FSC files saved by FL Studio.
const FSC_FORMAT: u16 = 0x0010;

/// Default velocity for written notes (78% = 0x64, FL Studio's own default).
const DEFAULT_VELOCITY: u8 = 0x64;

/// Default release (50% = 0x40).
const DEFAULT_RELEASE: u8 = 0x40;

/// Default pan (centre = 0x40).
const DEFAULT_PAN: u8 = 0x40;

// SNIFF default pitch layout — always used for export regardless of preset.
// BF lanes: data 0-3 -> pitches 48-51 (C3..D#3 in FL Studio's C5=60 scheme)
// Opponent lanes: data 4-7 -> pitches 60-63 (C4..D#4)
const BF_PITCHES: [u8; 4]  = [48, 49, 50, 51];
const OPP_PITCHES: [u8; 4] = [60, 61, 62, 63];

// Marker pitches emitted back into the FSC, matching the original SNIFF's
// MIDINotes enum. These mirror the constants in flp.rs.
const PITCH_BF_CAM: u8  = 53; // mustHitSection = true
const PITCH_EN_CAM: u8  = 54; // mustHitSection = false
const PITCH_BPM_CH: u8  = 56; // BPM change
const PITCH_ALT_AN: u8  = 57; // altAnim section

// ---------------------------------------------------------------------------
// Input JSON schema (Psych Engine chart)
// ---------------------------------------------------------------------------
// We only deserialize the fields we actually need. simd_json respects
// #[serde(deny_unknown_fields)] only if requested — omitting it lets us
// ignore the many fields we don't care about.

#[derive(Deserialize)]
struct ChartRoot {
    song: SongData,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SongData {
    bpm: f64,
    notes: Vec<SectionData>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SectionData {
    section_notes: Vec<NoteEntry>,
    #[serde(default)]
    must_hit_section: bool,
    /// Present and true when this section carries a BPM change.
    #[serde(default)]
    change_bpm: bool,
    /// The new BPM value when change_bpm is true. Ignored otherwise.
    #[serde(default)]
    bpm: f64,
    /// Present and true when this section has the altAnim flag set.
    #[serde(default)]
    alt_anim: bool,
}

/// A note entry is a positional JSON array: [strum_time, data, sustain?].
/// Fields may be integers or floats — Psych Engine charts commonly write data
/// as a bare integer (e.g. 0, 2) with no decimal point, which simd-json's
/// strict f64 deserializer rejects. We deserialize as OwnedValue and coerce
/// each field to f64 manually so both representations are accepted.
#[derive(Deserialize)]
struct NoteEntry(Vec<simd_json::OwnedValue>);

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Reads a Psych Engine chart JSON from `input`, converts all notes back to
/// 24-byte FSC records, and writes an FL Studio Score file to `output`.
pub fn convert_json_to_fsc(
    input: &Path,
    output: &Path,
    progress: &mut dyn FnMut(Progress),
) -> Result<Stats> {
    let start = Instant::now();

    progress(Progress::Stage("Reading chart JSON"));
    let mut bytes = fs::read(input)
        .with_context(|| format!("reading {}", input.display()))?;

    progress(Progress::Stage("Parsing chart JSON"));
    // simd_json::from_slice requires a mutable slice (it does in-place
    // transformations as part of its parsing strategy).
    let chart: ChartRoot = simd_json::from_slice(&mut bytes)
        .with_context(|| format!("parsing {}", input.display()))?;

    let bpm = chart.song.bpm;
    if !bpm.is_finite() || bpm <= 0.0 {
        bail!("chart JSON has invalid BPM: {bpm}");
    }

    // ms -> ticks: ticks = ms * (BPM * PPQ) / 60_000
    let ms_to_ticks = |ms: f64| -> u32 {
        ((ms * bpm * FSC_PPQ as f64) / 60_000.0).round() as u32
    };

    progress(Progress::Stage("Converting notes to FSC records"));
    let sections = &chart.song.notes;
    let total_sections = sections.len();
    let mut records: Vec<[u8; 24]> = Vec::new();

    // Marker state carried forward across sections, matching the original
    // SNIFF JSONtoFL section loop exactly.
    let mut current_bpm = bpm;
    let mut must_hit_section = true; // Psych Engine default

    /// Builds a marker record using FL Studio's DefaultNote values:
    /// TBD=0x4000, ChannelNo=0, duration=ppqn*4, velocity=0x64,
    /// release=0x40, pan=0x40, ModX=0x80, ModY=0x80.
    fn marker_record(position: u32, pitch: u8) -> [u8; 24] {
        let duration: u32 = FSC_PPQ as u32 * 4;
        let mut rec = [0u8; 24];
        rec[0..4].copy_from_slice(&position.to_le_bytes());
        rec[4] = 0x00; rec[5] = 0x40; // TBD = 0x4000
        rec[8..12].copy_from_slice(&duration.to_le_bytes());
        rec[12] = pitch;
        rec[16] = 0x78; // FinePitch low
        rec[18] = 0x40; // Release
        rec[20] = 0x40; // Pan
        rec[21] = 0x64; // Velocity
        rec[22] = 0x80; // ModX
        rec[23] = 0x80; // ModY
        rec
    }

    for (si, section) in sections.iter().enumerate() {
        let section_tick = (si as u32) * FSC_PPQ as u32 * 4;
        let must_hit = section.must_hit_section;

        // BPM change: emit a BPM_CH marker at the tick where the BPM changed.
        // Matches the original: only fires when changeBPM is true AND the BPM
        // value actually differs from the current BPM (not just the flag being set).
        if section.change_bpm && section.bpm != 0.0 && section.bpm != current_bpm {
            current_bpm = section.bpm;
            records.push(marker_record(section_tick, PITCH_BPM_CH));
        }

        // mustHitSection flip: emit BF_CAM (53) or EN_CAM (54) only when the
        // value actually changes from the previous section. The original tracks
        // mustHitSection state across sections and only emits when it flips.
        if must_hit != must_hit_section {
            must_hit_section = must_hit;
            let pitch = if must_hit_section { PITCH_BF_CAM } else { PITCH_EN_CAM };
            records.push(marker_record(section_tick, pitch));
        }

        // altAnim: emit ALT_AN (57) whenever this section has the flag set.
        if section.alt_anim {
            records.push(marker_record(section_tick, PITCH_ALT_AN));
        }

        for note in &section.section_notes {
            let fields = &note.0;
            if fields.is_empty() { continue; }

            // Coerce each positional field to f64, accepting both JSON
            // integers and floats (Psych Engine uses both interchangeably).
            let val_to_f64 = |v: &simd_json::OwnedValue| -> f64 {
                match v {
                    simd_json::OwnedValue::Static(simd_json::StaticNode::F64(f)) => *f,
                    simd_json::OwnedValue::Static(simd_json::StaticNode::I64(i)) => *i as f64,
                    simd_json::OwnedValue::Static(simd_json::StaticNode::U64(u)) => *u as f64,
                    _ => 0.0,
                }
            };
            let strum_ms   = val_to_f64(&fields[0]);
            let data       = fields.get(1).map(val_to_f64).unwrap_or(0.0) as u8;
            let sustain_ms = fields.get(2).map(val_to_f64).unwrap_or(0.0).max(0.0);

            // Reverse the lane formula from build_note_json:
            //   data = direction + (if player != must_hit { 4 } else { 0 })
            // So:
            //   if data >= 4 -> non-dominant side: direction = data - 4, player != must_hit
            //   if data < 4  -> dominant side:     direction = data,     player == must_hit
            let (direction, player) = if data >= 4 {
                (data - 4, !must_hit) // non-dominant side
            } else {
                (data, must_hit)      // dominant side
            };

            // Map back to SNIFF default pitches.
            let pitch = if player {
                *BF_PITCHES.get(direction as usize).unwrap_or(&48)
            } else {
                *OPP_PITCHES.get(direction as usize).unwrap_or(&60)
            };

            let position = ms_to_ticks(strum_ms);
            // Sustain in ms -> ticks. A zero sustain writes length=0.
            // Velocity is set to 0x3F (63, just under 50%) for sustain notes
            // so FL Studio's force-sustain detection treats them correctly,
            // matching the original SNIFF's MakeNote behavior.
            let (length, velocity) = if sustain_ms > 0.0 {
                (ms_to_ticks(sustain_ms), 0x3F_u8)
            } else {
                (0, DEFAULT_VELOCITY)
            };

            // Build the 24-byte record matching the FLP piano-roll layout
            // documented in parse_notes (FL.cs verified):
            //  0– 3: position (u32 LE)
            //  4– 5: TBD (0x4000 — matches FL Studio's own DefaultNote)
            //  6– 7: ChannelNo (0x0000)
            //  8–11: length (u32 LE)
            // 12–15: pitch (u32 LE, low byte = MIDI key)
            // 16–17: FinePitch (0x0078 = 120, default 0 cents)
            // 18:    Release (0x40 = 50%)
            // 19:    Flags (0x00)
            // 20:    Pan (0x40 = centre)
            // 21:    Velocity (0x64 = 78%)
            // 22–23: ModX, ModY (0x80, 0x80 — matches FL Studio's own DefaultNote)
            let mut rec = [0u8; 24];
            rec[0..4].copy_from_slice(&position.to_le_bytes());
            rec[4] = 0x00; // TBD low byte
            rec[5] = 0x40; // TBD high byte (0x4000 LE)
            // bytes 6-7: ChannelNo, zeroed
            rec[8..12].copy_from_slice(&length.to_le_bytes());
            rec[12] = pitch;
            // bytes 13-15: upper bytes of pitch uint, zeroed
            rec[16] = 0x78; // FinePitch low byte (120 = default 0 cents)
            rec[17] = 0x00; // FinePitch high byte
            rec[18] = DEFAULT_RELEASE;
            // rec[19] = 0 (flags, no portamento)
            rec[20] = DEFAULT_PAN;
            rec[21] = velocity;
            rec[22] = 0x80; // ModX (matches FL Studio DefaultNote)
            rec[23] = 0x80; // ModY (matches FL Studio DefaultNote)

            records.push(rec);
        }

        // Emit progress by section rather than note — section count is small
        // compared to note count and this is already a fast pass.
        progress(Progress::Notes { done: si + 1, total: total_sections });
    }

    let note_count = records.len();

    // Sort by position (tick) ascending so FL Studio sees them in order.
    records.sort_unstable_by_key(|r| u32::from_le_bytes(r[0..4].try_into().unwrap()));

    progress(Progress::Stage("Writing FSC"));

    // Encode the note payload length as a base-128 little-endian varint.
    // FL Studio's event reader expects this after the EVENT_PATTERN_NOTES byte
    // (0xE0), exactly the same varint format used by take_varint in flp.rs.
    let payload_bytes = (records.len() * 24) as u32;
    let mut varint: Vec<u8> = Vec::with_capacity(4);
    {
        let mut v = payload_bytes as usize;
        loop {
            let mut byte = (v & 0x7f) as u8;
            v >>= 7;
            if v > 0 { byte |= 0x80; }
            varint.push(byte);
            if v == 0 { break; }
        }
    }

    // The FLdt payload mirrors what the original SNIFF writes:
    //   15 bytes of preamble (version string event + two small events)
    //   0xE0 (EVENT_PATTERN_NOTES)
    //   varint-encoded note payload byte length
    //   note records (24 bytes each)
    //
    // Preamble breakdown (from original C# JSONtoFL data initializer):
    //   0xC7, 0x07  — event ID 199 (variable-length), varint length 7
    //   0x31 0x31 0x2E 0x31 0x2E 0x30 0x00  — "11.1.0\0" (FL version string)
    //   0x1C, 0x03  — event ID 28 (word), value 0x0003
    //   0x41, 0x00, 0x00  — event ID 65 (word = EVENT_NEW_PATTERN), value 0x0000
    //   0xE0  — EVENT_PATTERN_NOTES (224), varint payload length follows
    const PREAMBLE: &[u8] = &[
        0xC7, 0x07, 0x31, 0x31, 0x2E, 0x31, 0x2E, 0x30, 0x00,
        0x1C, 0x03,
        0x41, 0x00, 0x00,
        0xE0,
    ];
    let fldt_len = PREAMBLE.len() + varint.len() + records.len() * 24;

    let file = fs::File::create(output)
        .with_context(|| format!("creating {}", output.display()))?;
    let mut w = BufWriter::with_capacity(1024 * 1024, file);

    // FLhd chunk — 0x0005 for num_channels matches the original SNIFF output.
    w.write_all(b"FLhd")?;
    w.write_all(&6u32.to_le_bytes())?;       // header size always 6
    w.write_all(&FSC_FORMAT.to_le_bytes())?; // format = 0x0010 (FSC)
    w.write_all(&5u16.to_le_bytes())?;       // num_channels = 5 (matches original)
    w.write_all(&FSC_PPQ.to_le_bytes())?;    // PPQ = 96

    // FLdt chunk
    w.write_all(b"FLdt")?;
    w.write_all(&(fldt_len as u32).to_le_bytes())?;
    w.write_all(PREAMBLE)?;
    w.write_all(&varint)?;
    for rec in &records {
        w.write_all(rec)?;
    }

    w.flush().with_context(|| format!("writing {}", output.display()))?;

    Ok(Stats {
        notes:    note_count.to_formatted_string(&Locale::en),
        sections: total_sections.to_formatted_string(&Locale::en),
        time:     start.elapsed().as_micros() as f64 / 1000.0,
        files:    1,
        warnings: String::new(),
    })
}