// chart.rs
// Internal chart representation and everything needed to turn it into JSON.
//
// Responsibilities:
//   - Section and ChartNote (the in-memory chart model)
//   - resolve_must_hit_sections (carry mustHitSection state forward)
//   - resolve_section_timing (per-section BPM/ms computation)
//   - build_note_json (shared note → NoteJson conversion)
//   - JSON output types: ChartRoot, SongData, ChartOnlyRoot, SectionJson, NoteJson, SongMetadata
//   - SectionStream (streaming Serialize impl that drains sections on-the-fly)
//   - render_section (parallel fast-path: section → RawValue)
//   - round_num helper

use std::{
    cell::{Cell, RefCell},
    sync::{atomic::AtomicUsize, atomic::Ordering, mpsc},
    time::Duration,
};

use anyhow::{bail, Result};
use rayon::prelude::*;
use serde::{ser::SerializeSeq, Serialize};
use serde_json::value::{to_raw_value, RawValue};

use super::types::{ConversionPreset, Progress, Side, PROGRESS_BATCH};

// ---------------------------------------------------------------------------
// Tuning constants
// ---------------------------------------------------------------------------

/// How often the poll loop reports live progress while a batch is still
/// rendering. 20 ms matches the original SNIFF's polling interval.
pub(crate) const PROGRESS_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Number of sections processed per parallel batch. Higher = more memory,
/// more sections rendered at once. Empirically faster than note-count batching
/// for dense charts; do not change unless you have profiling data.
pub(crate) const PARALLEL_RENDER_CHUNK: usize = 256;

/// BPM and scroll speed are shown to 3 decimal places regardless of the
/// user's strum-time precision setting. round_num takes a raw multiplier
/// (value * factor), so this is 10^3.
pub(crate) const BPM_SPEED_PRECISION: f64 = 1000.0;

// ---------------------------------------------------------------------------
// In-memory chart model
// ---------------------------------------------------------------------------

/// One 16-step section of the chart, as grouped from the FLP's note stream.
/// Timing fields (chart_bpm, ms_per_pulse, etc.) are zeroed by default and
/// only become meaningful after `resolve_section_timing` has run.
#[derive(Default)]
pub(crate) struct Section {
    /// True when this section contains an `alt_marker_pitch` note.
    pub alt: bool,
    /// True when this section contains a PITCH_BPM_CHANGE (pitch 56) note.
    pub bpm_change: bool,
    /// True when this section contains a PITCH_ALT_ANIM (pitch 57) note.
    pub alt_anim: bool,
    /// The last PITCH_MUST_HIT_TRUE/FALSE marker in this section by tick
    /// position (notes may arrive out of order from the FLP). Resolved into
    /// `must_hit_section` by `resolve_must_hit_sections`.
    pub must_hit_marker: Option<(u32, bool)>,
    /// The value written to JSON. Defaults to false; only meaningful after
    /// `resolve_must_hit_sections` has run.
    pub must_hit_section: bool,

    // --- Timing fields (resolved by resolve_section_timing) ---
    /// chart_bpm = base_bpm × bpm_multiplier for this section.
    /// Written into the JSON `bpm` field when bpm_change is true.
    pub chart_bpm: f64,
    /// Milliseconds per FLP tick at this section's tempo.
    pub ms_per_pulse: f64,
    /// Milliseconds per quarter-step at this section's tempo.
    pub ms_per_step: f64,
    /// Sustain threshold in ms. Raw note lengths below this produce a zero
    /// sustain tail; at or above it, tail = raw_ms − ms_per_step.
    pub threshold_ms: f64,
    /// Absolute start time of this section in ms (sum of every earlier
    /// section's tick-width at that section's own tempo).
    pub start_time_ms: f64,
    /// Absolute start tick (= section_index × pulses_per_section).
    /// Stored here so build_note_json avoids recomputing it per note.
    pub start_tick: f64,

    pub notes: Vec<ChartNote>,
}

impl Section {
    /// Records a PITCH_MUST_HIT_TRUE/FALSE note at `position`. If more than
    /// one marker lands in the same section, the latest by tick wins (rare
    /// in practice, but not prevented by the format).
    pub fn record_must_hit_marker(&mut self, position: u32, value: bool) {
        let is_later = match self.must_hit_marker {
            Some((existing, _)) => position >= existing,
            None => true,
        };
        if is_later {
            self.must_hit_marker = Some((position, value));
        }
    }
}

/// A piano-roll note as it exists between grouping and JSON serialisation.
/// Stores raw FLP ticks, not milliseconds — conversion happens later in
/// `build_note_json` once per-section timing is resolved.
pub(crate) struct ChartNote {
    /// Raw tick position from the FLP (not yet converted to ms).
    pub position: u32,
    /// Raw tick duration from the FLP (not yet converted to ms).
    pub length: u32,
    /// 0–3 direction from the pitch mapping, NOT yet combined with the
    /// player/opponent lane offset (that depends on mustHitSection, which
    /// isn't known until `resolve_must_hit_sections` runs).
    pub direction: u8,
    pub player: bool,
    /// Portamento flag (record[19] & 0x08). Used as a per-note Alt Animation
    /// marker independent of the section-level `alt` flag.
    pub portamento: bool,
    /// True when velocity < 64 (below 50%). Forces a sustain tail regardless
    /// of note length — the threshold check is skipped.
    pub force_sustain: bool,
}

// ---------------------------------------------------------------------------
// Resolution passes
// ---------------------------------------------------------------------------

/// Carries `mustHitSection` state forward across sections in timeline order.
/// Starts false; only changes when a section has a PITCH_MUST_HIT_TRUE/FALSE
/// marker. Must run after grouping (so markers are set) and strictly in
/// section order (each section's value depends on all earlier sections).
pub(crate) fn resolve_must_hit_sections(sections: &mut [Section]) {
    let mut current = false;
    for section in sections.iter_mut() {
        if let Some((_, value)) = section.must_hit_marker {
            current = value;
        }
        section.must_hit_section = current;
    }
}

/// Resolves per-section BPM and timing fields. Must run after grouping (so
/// bpm_change flags are set) and strictly in section order (each section's
/// start_time_ms depends on every earlier section's own duration at its own
/// tempo).
///
/// When a section has `bpm_change = true`, the next value from `bpm_changes`
/// is consumed as the new base BPM **starting with that section** (matching
/// original SNIFF: the marker's own section gets the new tempo). Conversion
/// fails if the list is exhausted before all markers are satisfied. Each
/// supplied BPM is validated to be finite and positive.
///
/// `default_base_bpm` is the FLP's global tempo — used until the first
/// pitch-56 marker overrides it.
pub(crate) fn resolve_section_timing(
    sections: &mut [Section],
    pulses_per_section: f64,
    ppq: u16,
    default_base_bpm: f64,
    bpm_multiplier: f64,
    sustain_threshold_steps: f64,
    bpm_changes: &[f64],
) -> Result<()> {
    let mut current_base_bpm = default_base_bpm;
    let mut bpm_change_iter = bpm_changes.iter().enumerate();
    let mut accumulated_ms = 0.0f64;

    for (section_index, section) in sections.iter_mut().enumerate() {
        if section.bpm_change {
            let (list_index, &new_base_bpm) = bpm_change_iter.next().ok_or_else(|| {
                anyhow::anyhow!(
                    "not enough BPM values in bpm_changes list: \
                     pitch-56 marker in section {} has no corresponding entry \
                     (list has {} entr{})",
                    section_index,
                    bpm_changes.len(),
                    if bpm_changes.len() == 1 { "y" } else { "ies" },
                )
            })?;
            if !new_base_bpm.is_finite() || new_base_bpm <= 0.0 {
                bail!(
                    "bpm_changes[{}] = {} is not a valid BPM (must be a positive finite number)",
                    list_index,
                    new_base_bpm
                );
            }
            current_base_bpm = new_base_bpm;
        }

        let chart_bpm  = current_base_bpm * bpm_multiplier;
        let ms_per_pulse = 60_000.0 / (current_base_bpm * ppq as f64 * bpm_multiplier);
        let ms_per_step  = 60_000.0 / (chart_bpm * 4.0);

        section.chart_bpm    = chart_bpm;
        section.ms_per_pulse = ms_per_pulse;
        section.ms_per_step  = ms_per_step;
        section.threshold_ms = sustain_threshold_steps * ms_per_step;
        section.start_time_ms = accumulated_ms;
        section.start_tick   = section_index as f64 * pulses_per_section;

        accumulated_ms += pulses_per_section * ms_per_pulse;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Note conversion
// ---------------------------------------------------------------------------

/// Converts one raw `ChartNote` into its final `NoteJson` representation.
/// Shared by both the sequential and parallel write paths so they can't drift.
///
/// All timing parameters must come from the note's section's resolved fields
/// (set by `resolve_section_timing`) and must be captured *before*
/// `section.notes` is consumed by `into_iter()`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_note_json(
    note: ChartNote,
    alt: bool,
    must_hit_section: bool,
    trim_sustains: bool,
    precision: f64,
    start_time_ms: f64,
    start_tick: f64,
    ms_per_pulse: f64,
    ms_per_step: f64,
    threshold_ms: f64,
) -> NoteJson {
    // Convert raw FLP ticks to ms using this section's tempo. The note's
    // position is relative to the whole pattern, so subtract the section's
    // own start tick first.
    let time = round_num(
        start_time_ms + (note.position as f64 - start_tick) * ms_per_pulse,
        precision,
    );
    let raw_length_ms = note.length as f64 * ms_per_pulse;
    let sustain = if note.force_sustain || raw_length_ms >= threshold_ms {
        // Match SNIFF: the note's initial quarter-step is the tap head, so
        // it is not included in the sustain tail. force_sustain (low-velocity)
        // bypasses the threshold check but still subtracts the tap head.
        (raw_length_ms - ms_per_step).max(0.0)
    } else {
        0.0
    };

    // With mustHitSection=false, Psych Engine reads data 0–3 as opponent and
    // 4–7 as BF; with mustHitSection=true that's flipped. `player !=
    // must_hit_section` captures both cases: it's the same "+4 for the lane
    // that's currently BF's" formula either way, with the sides swapped.
    let data = note.direction + if note.player != must_hit_section { 4 } else { 0 };

    // Portamento on the note acts as a per-note Alt Animation flag, same as
    // the section-level `alt` (set by alt_marker_pitch) but scoped to this
    // single note. Either source triggers the type string. Opponent notes
    // never receive Alt Animation.
    let note_type: &'static str = if (alt || note.portamento) && note.player {
        "Alt Animation"
    } else {
        ""
    };

    if !note_type.is_empty() {
        NoteJson::Full(time, data, round_num(sustain, precision), note_type)
    } else if trim_sustains && sustain == 0.0 {
        NoteJson::Short(time, data)
    } else {
        NoteJson::Sustained(time, data, round_num(sustain, precision))
    }
}

// ---------------------------------------------------------------------------
// JSON output types
// ---------------------------------------------------------------------------
// ChartRoot / SongData / ChartOnlyRoot / SongMetadata / SectionJson / NoteJson
// derive Serialize normally. The streamed `notes` field (on either SongData
// or ChartOnlyRoot) is a SectionStream, which implements Serialize by hand
// and drains the section list element-by-element as serde_json asks for the
// next array element — so each section is built, written to the BufWriter,
// and dropped before the next one starts. Peak memory is just the grouped
// notes (unavoidable) plus one section at a time, not a full second copy of
// the chart as JSON.

/// Combined chart file root: notes + song metadata together in one file,
/// the original single-file format (`{ "song": { ...metadata, "notes": [...] } }`).
/// Used when `ConversionPreset::split_metadata` is false.
#[derive(Serialize)]
pub(crate) struct ChartRoot<'a, 'p> {
    pub song: SongData<'a, 'p>,
}

#[derive(Serialize)]
pub(crate) struct SongData<'a, 'p> {
    pub song: &'a str,
    pub bpm: f64,
    pub speed: f64,
    #[serde(rename = "needsVoices")]
    pub needs_voices: bool,
    pub player1: &'a str,
    pub player2: &'a str,
    #[serde(rename = "gfVersion")]
    pub gf_version: &'a str,
    #[serde(rename = "songCreator", skip_serializing_if = "str::is_empty")]
    pub song_creator: &'a str,
    pub stage: &'a str,
    #[serde(rename = "validScore")]
    pub valid_score: bool,
    pub notes: SectionStream<'p>,
}

/// Chart file root when metadata is exported separately (see `SongMetadata`
/// below): just the notes, wrapped in the same single-element-array shape
/// as the metadata file so both sides of the split look consistent.
/// Used when `ConversionPreset::split_metadata` is true.
#[derive(Serialize)]
pub(crate) struct ChartOnlyRoot<'p> {
    pub notes: SectionStream<'p>,
}

// A standalone metadata thingamabob that also reads from what values you put in!
// This is for exporting the chart itself into a different file while 
// having the metadata also separate.
//
// SongMetadata is written as a bare single-element array — `&[metadata]` —
// not wrapped in a `{ "metadata": ... }` object, to match the shape below.

// Expected shape:
/**
 * bopeebo-chart-hard.json 
   [
    {
      "notes":
      ...
    }
   ]
 *
 * bopeebo-metadata-hard.json 
   [
    {
      "song": "bopeebo",
      "bpm": 100,
      "speed": 1,
      "needsVoices": true,
      "player1": "bf",
      "player2": "dad",
      "gfVersion": "gf",
      "stage": "stage"
    }
   ]
 *
 */
#[derive(Serialize)]
pub(crate) struct SongMetadata<'a> {
    // Song, eg. "bopeebo"
    pub song: &'a str,

    // BPM, eg. 100
    pub bpm: f64,

    // Speed, eg. 1
    pub speed: f64,

    // If the song really needs Voices.
    #[serde(rename = "needsVoices")]
    pub needs_voices: bool,

    // Equivalent to player. eg. "bf"
    pub player1: &'a str,

    // Equivalent to opponent. eg. "dad"
    pub player2: &'a str,

    // Equivalent of gf. eg. "gf"
    #[serde(rename = "gfVersion")]
    pub gf_version: &'a str,

    // Song creator data. Not really needed for most engines. eg. "Jordan Santiago"
    #[serde(rename = "songCreator", skip_serializing_if = "str::is_empty")]
    pub song_creator: &'a str,

    // Stage data. eg. "stage"
    pub stage: &'a str,

    #[serde(rename = "validScore")]
    pub valid_score: bool,
}

#[derive(Serialize)]
pub(crate) struct SectionJson {
    #[serde(rename = "sectionNotes")]
    pub section_notes: Vec<NoteJson>,
    #[serde(rename = "lengthInSteps")]
    pub length_in_steps: u32,
    #[serde(rename = "mustHitSection")]
    pub must_hit_section: bool,
    /// Only present when this section had a pitch-56 marker.
    #[serde(rename = "changeBPM", skip_serializing_if = "Option::is_none")]
    pub change_bpm: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bpm: Option<f64>,
    /// Only present (and true) when this section had a pitch-57 marker.
    #[serde(rename = "altAnim", skip_serializing_if = "Option::is_none")]
    pub alt_anim: Option<bool>,
}

/// Positional note array. Three shapes, shortest that carries all meaningful
/// fields:
///   Short(time, data)                    — no sustain, no noteType
///   Sustained(time, data, sustain)       — with sustain, no noteType
///   Full(time, data, sustain, noteType)  — Alt Animation notes
///
/// The empty `""` noteType is never written: it is meaningless but costs
/// real bytes on every sustained non-alt note (the most common case in any
/// typical chart). Trimming it is unconditional and unrelated to `trim_sustains`.
#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum NoteJson {
    Short(f64, u8),
    Sustained(f64, u8, f64),
    Full(f64, u8, f64, &'static str),
}

// ---------------------------------------------------------------------------
// Streaming section writer
// ---------------------------------------------------------------------------

/// Implements the `notes` array of the chart JSON by draining `sections`
/// one at a time as serde_json requests elements, rather than building a
/// `Vec<SectionJson>` first. Interior mutability (`RefCell`/`Cell`) is
/// required because `Serialize::serialize` only receives `&self`.
pub(crate) struct SectionStream<'p> {
    pub sections: &'p RefCell<Vec<Section>>,
    pub section_count: usize,
    pub trim_sustains: bool,
    pub notes_to_write: usize,
    pub progress: RefCell<&'p mut dyn FnMut(Progress)>,
    pub start: usize,
    /// Populated once `serialize` runs; read back by the caller for `Stats`.
    pub note_count: Cell<usize>,
    pub precision: f64,
    /// Use the parallel `RawValue` fast path instead of the sequential one.
    /// Only valid for compact output: a `RawValue` is bytes verbatim, so a
    /// compactly-rendered section can't retroactively pick up the surrounding
    /// pretty-printer's indentation.
    pub parallel: bool,
}

impl<'p> Serialize for SectionStream<'p> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if self.parallel {
            self.serialize_parallel(serializer)
        } else {
            self.serialize_sequential(serializer)
        }
    }
}

impl<'p> SectionStream<'p> {
    /// Single-threaded write path. Used for pretty JSON (where a RawValue
    /// splice would embed flat bytes into an indented file) and as the
    /// fallback when parallelism is disabled.
    fn serialize_sequential<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(self.section_count))?;
        let mut sections = self.sections.borrow_mut();
        let mut progress_guard = self.progress.borrow_mut();
        let progress: &mut dyn FnMut(Progress) = &mut **progress_guard;

        let mut note_count = 0usize;
        let mut last_reported = 0usize;

        for index in 0..self.section_count {
            let section = std::mem::take(&mut sections[self.start + index]);
            note_count += section.notes.len();
            if self.notes_to_write > 0
                && (note_count - last_reported >= PROGRESS_BATCH
                    || index + 1 == self.section_count)
            {
                last_reported = note_count;
                progress(Progress::Notes { done: note_count, total: self.notes_to_write });
            }

            // Capture timing before into_iter() moves section.notes.
            let alt              = section.alt;
            let must_hit_section = section.must_hit_section;
            let bpm_change       = section.bpm_change;
            let alt_anim         = section.alt_anim;
            let section_chart_bpm = section.chart_bpm;
            let start_time_ms    = section.start_time_ms;
            let start_tick       = section.start_tick;
            let ms_per_pulse     = section.ms_per_pulse;
            let ms_per_step      = section.ms_per_step;
            let threshold_ms     = section.threshold_ms;

            let section_notes: Vec<NoteJson> = section
                .notes
                .into_iter()
                .map(|note| {
                    build_note_json(
                        note, alt, must_hit_section, self.trim_sustains,
                        self.precision, start_time_ms, start_tick,
                        ms_per_pulse, ms_per_step, threshold_ms,
                    )
                })
                .collect();

            seq.serialize_element(&SectionJson {
                section_notes,
                length_in_steps: 16,
                must_hit_section,
                change_bpm: bpm_change.then_some(true),
                bpm: bpm_change.then_some(section_chart_bpm),
                alt_anim: alt_anim.then_some(true),
            })?;
        }
        self.note_count.set(note_count);
        seq.end()
    }

    /// Parallel fast path for compact output. Each section is independent,
    /// so rendering it to its final JSON text (the real CPU cost is
    /// float-to-string formatting, not I/O) can happen on any core.
    /// Writing must stay in order, so each batch is rendered in parallel
    /// then handed to the writer sequentially.
    fn serialize_parallel<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(self.section_count))?;
        let mut sections = self.sections.borrow_mut();
        let mut progress_guard = self.progress.borrow_mut();
        let progress: &mut dyn FnMut(Progress) = &mut **progress_guard;
        let mut notes_done = 0usize;

        // Copy into plain locals so the parallel closure captures two Copy
        // primitives instead of `self` (which holds RefCell fields and is
        // not Sync).
        let trim_sustains = self.trim_sustains;
        let precision     = self.precision;
        let start         = self.start;

        let section_slice = &mut sections[start..start + self.section_count];

        for chunk in section_slice.chunks_mut(PARALLEL_RENDER_CHUNK) {
            let batch_progress = AtomicUsize::new(0);
            let (done_tx, done_rx) = mpsc::channel::<()>();

            let rendered: Vec<(usize, Result<Box<RawValue>>)> = std::thread::scope(|scope| {
                let handle = scope.spawn(|| {
                    let rendered: Vec<(usize, Result<Box<RawValue>>)> = chunk
                        .par_iter_mut()
                        .map(|section| {
                            let note_count = section.notes.len();
                            let raw = render_section(
                                std::mem::take(section),
                                trim_sustains,
                                precision,
                            );
                            batch_progress.fetch_add(note_count, Ordering::Relaxed);
                            (note_count, raw)
                        })
                        .collect();
                    // Wake the poll loop immediately rather than waiting out
                    // the rest of PROGRESS_POLL_INTERVAL once the batch is done.
                    let _ = done_tx.send(());
                    rendered
                });

                loop {
                    match done_rx.recv_timeout(PROGRESS_POLL_INTERVAL) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if self.notes_to_write > 0 {
                                progress(Progress::Notes {
                                    done: notes_done
                                        + batch_progress.load(Ordering::Relaxed),
                                    total: self.notes_to_write,
                                });
                            }
                        }
                    }
                }

                handle
                    .join()
                    .map_err(|_| serde::ser::Error::custom("section render thread panicked"))
            })?;

            for (note_count, raw) in rendered {
                let raw = raw.map_err(serde::ser::Error::custom)?;
                seq.serialize_element(&raw)?;
                notes_done += note_count;
            }
            if self.notes_to_write > 0 {
                progress(Progress::Notes { done: notes_done, total: self.notes_to_write });
            }
        }
        self.note_count.set(notes_done);
        seq.end()
    }
}

// ---------------------------------------------------------------------------
// Parallel section renderer
// ---------------------------------------------------------------------------

/// Renders one section to its final compact JSON bytes via `RawValue`, so it
/// can later be spliced into the output byte-for-byte without re-parsing.
/// Assumes `section.notes` is already sorted (sorting happens once, in
/// parallel, in `pipeline::convert_chart` before any section reaches here).
/// Timing fields must be resolved by `resolve_section_timing` before calling.
pub(crate) fn render_section(
    section: Section,
    trim_sustains: bool,
    precision: f64,
) -> Result<Box<RawValue>> {
    let alt              = section.alt;
    let must_hit_section = section.must_hit_section;
    let bpm_change       = section.bpm_change;
    let alt_anim         = section.alt_anim;
    let section_chart_bpm = section.chart_bpm;
    let start_time_ms    = section.start_time_ms;
    let start_tick       = section.start_tick;
    let ms_per_pulse     = section.ms_per_pulse;
    let ms_per_step      = section.ms_per_step;
    let threshold_ms     = section.threshold_ms;

    let section_notes: Vec<NoteJson> = section
        .notes
        .into_iter()
        .map(|note| {
            build_note_json(
                note, alt, must_hit_section, trim_sustains,
                precision, start_time_ms, start_tick,
                ms_per_pulse, ms_per_step, threshold_ms,
            )
        })
        .collect();

    Ok(to_raw_value(&SectionJson {
        section_notes,
        length_in_steps: 16,
        must_hit_section,
        change_bpm: bpm_change.then_some(true),
        bpm: bpm_change.then_some(section_chart_bpm),
        alt_anim: alt_anim.then_some(true),
    })?)
}

// ---------------------------------------------------------------------------
// Helper
// ---------------------------------------------------------------------------

/// Rounds `value` to `factor` decimal places (pass 10^n as factor).
pub(crate) fn round_num(value: f64, factor: f64) -> f64 {
    (value * factor).round() / factor
}

// ---------------------------------------------------------------------------
// Pitch-mapping lookup (used by pipeline)
// ---------------------------------------------------------------------------

/// Builds a 256-entry lookup table from `preset.mapping`, indexed by MIDI
/// pitch. O(1) per note vs O(mapping.len()) for a linear scan.
pub(crate) fn build_pitch_lookup(preset: &ConversionPreset) -> [Option<(u8, bool)>; 256] {
    let mut table: [Option<(u8, bool)>; 256] = [None; 256];
    for m in &preset.mapping {
        table[m.pitch as usize] = Some((m.direction, m.side == Side::Player));
    }
    table
}