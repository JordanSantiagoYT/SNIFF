// inspect.rs
// Public query functions that interrogate FLP/FSC files without performing a
// full chart conversion. These are called by the UI to populate pattern
// pickers, detect difficulty slots, and pre-scan BPM change counts.

use std::{fs, path::Path};

use anyhow::{Context, Result};

use super::flp::{inspect_bytes, is_fsc, parse_fsc};
use super::types::{DifficultySlots, FlpInfo};

/// Reads an FLP or FSC from disk and returns its pattern list and BPM.
/// For FLP this is the cheap path (counts notes without decoding records).
/// For FSC the file is fully parsed since there are no pattern events to
/// count from — the note count comes from actually reading the records.
/// BPM is always None for FSC (no tempo event; must come from the preset).
pub fn inspect(input: &Path) -> Result<FlpInfo> {
    let bytes = fs::read(input).with_context(|| format!("reading {}", input.display()))?;
    if is_fsc(&bytes) {
        // parse_fsc gives us a synthetic FlpInfo with one "Score" pattern.
        let project = parse_fsc(&bytes, &mut |_| {})?;
        Ok(FlpInfo {
            bpm: None,
            patterns: project.patterns,
        })
    } else {
        inspect_bytes(&bytes)
    }
}

/// Scans a `FlpInfo` for easy/normal/hard difficulty patterns by
/// case-insensitive substring match on the pattern name. Returns the first
/// match per slot. Zero-note patterns are skipped.
pub fn find_difficulty_patterns(info: &FlpInfo) -> DifficultySlots {
    let mut slots = DifficultySlots::default();
    for pattern in &info.patterns {
        if pattern.note_count == 0 {
            continue;
        }
        let lower = pattern.name.to_lowercase();
        if slots.easy.is_none() && lower.contains("easy") {
            slots.easy = Some(pattern.id);
        } else if slots.normal.is_none() && lower.contains("normal") {
            slots.normal = Some(pattern.id);
        } else if slots.hard.is_none() && lower.contains("hard") {
            slots.hard = Some(pattern.id);
        }
    }
    slots
}

/// Scans all FLPs/FSCs in `inputs` and returns the total BPM change
/// count for `pattern_id` summed across every file. FSC files always
/// contribute 0 (they have no marker system).
pub fn scan_merge_inputs(inputs: &[std::path::PathBuf], pattern_id: u16) -> Result<usize> {
    let mut total = 0usize;
    for path in inputs {
        let info = inspect(path)?;
        total += info
            .patterns
            .iter()
            .find(|p| p.id == pattern_id)
            .map_or(0, |p| p.bpm_change_count);
    }
    Ok(total)
}
