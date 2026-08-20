// types.rs
// Public data types shared between the converter pipeline and the UI.
// No I/O, no parsing logic, no serde output shapes — just the plain data
// structures that cross the boundary between converter.rs and main.rs.

use num_format::{Locale, ToFormattedString};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Pipeline mode
// ---------------------------------------------------------------------------

/// Which pipeline mode to use when converting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConversionMode {
    /// One FLP, one output JSON. Current single-file behavior.
    Single,
    /// Multiple FLPs merged into one chart. All FLPs share a pattern ID and
    /// BPM layout; tick positions are globally consistent across files.
    Merge,
    /// One FLP, up to three output JSONs — easy/normal/hard discovered by
    /// case-insensitive pattern name substring matching. Zero-note patterns
    /// are skipped. Normal gets no suffix; easy gets "-easy"; hard gets "-hard".
    SplitDifficulties,
    /// Multiple FLPs, one output JSON per input written to a target directory.
    Batch,
}

// ---------------------------------------------------------------------------
// FLP inspection results
// ---------------------------------------------------------------------------

/// Top-level result of inspecting an FLP without fully parsing notes.
pub struct FlpInfo {
    pub bpm: Option<f64>,
    pub patterns: Vec<PatternInfo>,
}

/// A pattern as it appears in the FLP, for populating a picker by name
/// instead of by FL Studio's internal (and otherwise invisible) pattern id.
#[derive(Clone, Debug)]
pub struct PatternInfo {
    pub id: u16,
    pub name: String,
    pub note_count: usize,
    /// Number of pitch-56 (PITCH_BPM_CHANGE) markers in this pattern.
    /// Used by the UI to tell the user how many entries the bpm_changes
    /// list needs and to auto-populate the list length.
    pub bpm_change_count: usize,
}

impl PatternInfo {
    /// Human-readable label: "Pattern Name (1,234 notes)".
    pub fn display_name(&self) -> String {
        format!(
            "{} ({} notes)",
            self.name,
            self.note_count.to_formatted_string(&Locale::en),
        )
    }
}

/// The three difficulty pattern slots resolved from a SplitDifficulties scan.
/// Each is `Some(pattern_id)` if a matching non-empty pattern was found.
#[derive(Clone, Debug, Default)]
pub struct DifficultySlots {
    pub easy: Option<u16>,
    pub normal: Option<u16>,
    pub hard: Option<u16>,
}

impl DifficultySlots {
    /// Returns true if at least one difficulty was found.
    pub fn any(&self) -> bool {
        self.easy.is_some() || self.normal.is_some() || self.hard.is_some()
    }
}

// ---------------------------------------------------------------------------
// Conversion preset
// ---------------------------------------------------------------------------

/// Which side of the chart a pitch belongs to.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Player,
    Opponent,
}

/// Maps a MIDI pitch to a chart lane (side + direction 0-3).
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PitchMapping {
    pub pitch: u8,
    pub side: Side,
    pub direction: u8,
}

/// All user-configurable settings that control a single conversion run.
/// Serializable so it can be saved/loaded as a JSON preset file.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct ConversionPreset {
    pub song_name: String,
    pub pattern_id: u16,
    pub player1: String,
    pub player2: String,
    pub gf_version: String,
    pub stage: String,
    pub song_creator: String,
    pub needs_voices: bool,
    pub bpm_multiplier: f64,
    pub decimals: u8,
    /// Scroll speed. `None` = computed from BPM (chart_bpm / 50).
    pub speed: Option<f64>,
    pub sustain_threshold_steps: f64,
    pub trim_sustains: bool,
    pub pretty_json: bool,
    /// When true, write chart notes and song metadata to two separate files
    /// (`{stem}-chart.{ext}` / `{stem}-metadata.{ext}`) instead of the
    /// original combined single-file format. Default: false (combined).
    pub split_metadata: bool,
    pub alt_marker_pitch: u8,
    pub mapping: Vec<PitchMapping>,
    /// BPM values consumed in order when PITCH_BPM_CHANGE (pitch 56) markers
    /// are encountered. Each entry replaces the current base BPM from that
    /// section onward (the BPM multiplier is applied on top afterward).
    /// The list must have at least as many entries as there are pitch-56
    /// markers in the pattern — conversion fails with a clear error if it
    /// runs out.
    pub bpm_changes: Vec<f64>,
    /// Overrides the starting base BPM instead of reading it from the FLP.
    /// `None` = use the FLP's own tempo event (default).
    pub base_bpm_override: Option<f64>,
}

impl Default for ConversionPreset {
    fn default() -> Self {
        let mut mapping = Vec::with_capacity(8);
        // SNIFF's FL Studio piano-roll layout: BF is 48–51 and opponent is 60–63.
        for (index, pitch) in (48..52).enumerate() {
            mapping.push(PitchMapping {
                pitch,
                side: Side::Player,
                direction: index as u8,
            });
        }
        for (index, pitch) in (60..64).enumerate() {
            mapping.push(PitchMapping {
                pitch,
                side: Side::Opponent,
                direction: index as u8,
            });
        }
        Self {
            song_name: "untitled".into(),
            pattern_id: 1,
            player1: "bf".into(),
            player2: "dad".into(),
            gf_version: "gf".into(),
            stage: "stage".into(),
            song_creator: "".into(),
            needs_voices: true,
            bpm_multiplier: 1.0,
            decimals: 6,
            speed: None,
            sustain_threshold_steps: 4.0,
            trim_sustains: false,
            pretty_json: false,
            split_metadata: false,
            alt_marker_pitch: 58,
            mapping,
            bpm_changes: Vec::new(),
            base_bpm_override: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Progress reporting
// ---------------------------------------------------------------------------

/// Progress updates emitted while converting, for driving a UI progress bar.
/// `Notes` gives an exact done/total count for the current stage; stages with
/// no meaningful note count (raw file I/O) only emit `Stage`.
pub enum Progress {
    Stage(&'static str),
    Notes { done: usize, total: usize },
}

// ---------------------------------------------------------------------------
// Progress tuning
// ---------------------------------------------------------------------------

/// Minimum number of notes between UI progress-bar updates. Sending one
/// message per note would flood the channel on multi-million-note charts;
/// this still gives a smooth bar without the overhead.
pub(crate) const PROGRESS_BATCH: usize = 65_536;

// ---------------------------------------------------------------------------
// Conversion result
// ---------------------------------------------------------------------------

/// Summary returned to the UI after a successful conversion.
#[derive(Default)]
pub struct Stats {
    pub notes: String,
    pub sections: String,
    pub time: f64,
    /// Number of output files written. 1 when no split occurred.
    pub files: usize,
    /// Non-empty when any section exceeded the split threshold and had to be
    /// written anyway (can't split inside a section). Listed by section index.
    pub warnings: String,
}
