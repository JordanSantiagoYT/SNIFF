// pipeline.rs
// Top-level conversion orchestration.
//
// Responsibilities:
//   - convert_file  — public entry point; dispatches on ConversionMode
//   - convert_chart — private core pipeline shared by all modes:
//                     groups notes → resolves timing → sorts → writes JSON
//   - song_name_from — helper that picks the right song name string

use std::{
    cell::{Cell, RefCell},
    fs,
    io::{BufWriter, Write},
    path::Path,
    time::Instant,
};

use anyhow::{bail, Context, Result};
use num_format::{Locale, ToFormattedString};
use rayon::prelude::*;

use super::{
    chart::{
        build_pitch_lookup, resolve_must_hit_sections, resolve_section_timing,
        round_num, ChartNote, ChartOnlyRoot, ChartRoot, Section, SectionStream,
        SongData, SongMetadata, BPM_SPEED_PRECISION, PARALLEL_RENDER_CHUNK,
    },
    flp::{is_fsc, parse_fsc, parse_flp, PITCH_ALT_ANIM, PITCH_BPM_CHANGE, PITCH_MUST_HIT_FALSE, PITCH_MUST_HIT_TRUE},
    inspect::{find_difficulty_patterns, inspect},
    types::{ConversionMode, ConversionPreset, Progress, Stats, PROGRESS_BATCH},
};

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Reads FLP(s) according to `mode`, converts, and writes output JSON(s).
///
/// - `primary_input`  — first (or only) FLP path
/// - `extra_inputs`   — remaining FLPs for Merge and Batch modes
/// - `output`         — output file for Single/Merge/SplitDifficulties;
///                      output *directory* for Batch
/// - `split_threshold` — split output into multiple files when note count
///                       exceeds this per file; `None` = single file always
pub fn convert_file(
    primary_input: &Path,
    extra_inputs: &[std::path::PathBuf],
    output: &Path,
    preset: &ConversionPreset,
    mode: &ConversionMode,
    split_threshold: Option<u64>,
    mut progress: impl FnMut(Progress),
) -> Result<Stats> {
    let start_time = Instant::now();
    if !preset.bpm_multiplier.is_finite() || preset.bpm_multiplier <= 0.0 {
        bail!("BPM multiplier must be a positive number");
    }

    match mode {
        // -----------------------------------------------------------------
        ConversionMode::Single => {
            progress(Progress::Stage("Reading FLP"));
            let bytes = fs::read(primary_input)
                .with_context(|| format!("reading {}", primary_input.display()))?;
            progress(Progress::Stage("Parsing notes"));
            let project = if is_fsc(&bytes) {
                parse_fsc(&bytes, &mut progress)?
            } else {
                parse_flp(&bytes, Some(preset.pattern_id), &mut progress)?
            };
            if project.notes.is_empty() {
                bail!("The pattern you selected doesn't have any notes.");
            }
            // FSC has no tempo event — BPM must come from base_bpm_override.
            let flp_bpm = project.bpm
                .or(preset.base_bpm_override)
                .context("FSC has no tempo event — enable 'Override starting BPM' and set a BPM")?;
            if project.ppq == 0 {
                bail!("FLP has PPQ 0");
            }
            let song = song_name_from(preset, primary_input);
            let mut stats = convert_chart(
                project.notes, project.ppq, preset, &song,
                output, "", split_threshold, &mut progress, flp_bpm,
            )?;
            stats.time = start_time.elapsed().as_micros() as f64 / 1000.0;
            Ok(stats)
        }

        // -----------------------------------------------------------------
        ConversionMode::Merge => {
            // All FLPs share globally-consistent tick positions, so we read
            // them all into one Vec<FlNote> and run the pipeline once.
            //
            // Pattern IDs are per-project creation-order indices in FL Studio,
            // not stable cross-project identifiers — FLP #2 might have the
            // same musical pattern at ID 2 while FLP #1 has it at ID 1.
            // We resolve by name instead: find the name of preset.pattern_id
            // in FLP #1, then look that name up in every subsequent FLP and
            // use whatever ID it has there. If a file has no pattern with
            // that name, it is skipped with a warning.
            //
            // parse_flp collects the full pattern list in the same event-loop
            // pass as note extraction (project.patterns), so no separate
            // inspect() call is needed — each FLP is read exactly once.
            let all_inputs: Vec<&Path> = std::iter::once(primary_input)
                .chain(extra_inputs.iter().map(|p| p.as_path()))
                .collect();
            let mut combined_notes = Vec::new();
            let mut ppq = 0u16;
            let mut flp_bpm = None;
            let mut target_name: Option<String> = None;
            let mut skipped: Vec<String> = Vec::new();

            for (i, path) in all_inputs.iter().enumerate() {
                progress(Progress::Stage("Reading FLP"));
                let bytes = fs::read(path)
                    .with_context(|| format!("reading {}", path.display()))?;
                progress(Progress::Stage("Parsing notes"));

                if i == 0 {
                    // FSC has a single implicit pattern; FLP uses pattern_id.
                    let project = if is_fsc(&bytes) {
                        parse_fsc(&bytes, &mut progress)?
                    } else {
                        parse_flp(&bytes, Some(preset.pattern_id), &mut progress)?
                    };
                    let name = project
                        .patterns
                        .iter()
                        .find(|p| p.id == preset.pattern_id)
                        .or_else(|| project.patterns.first())
                        .map(|p| p.name.clone())
                        .unwrap_or_else(|| format!("Pattern {}", preset.pattern_id));
                    target_name = Some(name);
                    ppq = project.ppq;
                    flp_bpm = project.bpm;
                    combined_notes.extend(project.notes);
                    continue;
                }

                // For subsequent files: FSC is always a single-pattern file,
                // so we parse it directly. FLP uses the patterns-only pass to
                // resolve the target pattern name to an ID, then parses again.
                if is_fsc(&bytes) {
                    let project = parse_fsc(&bytes, &mut progress)?;
                    combined_notes.extend(project.notes);
                    continue;
                }
                let info = parse_flp(&bytes, None, &mut |_| {})?;
                let name = target_name.as_deref().unwrap_or("");
                let pattern_id = match info.patterns.iter().find(|p| p.name == name) {
                    Some(p) => p.id,
                    None => {
                        skipped.push(
                            path.file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("(unknown)")
                                .to_owned(),
                        );
                        continue;
                    }
                };
                let project = parse_flp(&bytes, Some(pattern_id), &mut progress)?;
                combined_notes.extend(project.notes);
            }

            if combined_notes.is_empty() {
                bail!("No notes found across all input FLPs/FSCs for the selected pattern.");
            }
            let flp_bpm = flp_bpm
                .or(preset.base_bpm_override)
                .context("No tempo event found — enable 'Override starting BPM' and set a BPM")?;
            if ppq == 0 {
                bail!("FLP has PPQ 0");
            }
            let song = song_name_from(preset, primary_input);
            let mut stats = convert_chart(
                combined_notes, ppq, preset, &song,
                output, "", split_threshold, &mut progress, flp_bpm,
            )?;
            stats.time = start_time.elapsed().as_micros() as f64 / 1000.0;
            if !skipped.is_empty() {
                stats.warnings = format!(
                    " Warning: {} file{} skipped (no pattern named {:?}): {}.",
                    skipped.len(),
                    if skipped.len() == 1 { "" } else { "s" },
                    target_name.as_deref().unwrap_or(""),
                    skipped.join(", "),
                );
            }
            Ok(stats)
        }

        // -----------------------------------------------------------------
        ConversionMode::SplitDifficulties => {
            progress(Progress::Stage("Reading FLP"));
            let bytes = fs::read(primary_input)
                .with_context(|| format!("reading {}", primary_input.display()))?;
            let info = inspect(primary_input)?;
            let slots = find_difficulty_patterns(&info);
            if !slots.any() {
                bail!("No easy, normal, or hard patterns with notes found in this FLP.");
            }

            let stem = primary_input
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("chart");
            let ext = output
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("json");
            let base_dir = output.parent().unwrap_or(Path::new("."));
            // Base path passed to convert_chart; it inserts "-chart"/"-metadata"
            // before the difficulty suffix below, e.g. "{stem}.{ext}" + "-hard"
            // -> "{stem}-chart-hard.{ext}" / "{stem}-metadata-hard.{ext}".
            let base_output = base_dir.join(format!("{stem}.{ext}"));

            let difficulties: &[(Option<u16>, &str)] = &[
                (slots.easy,   "-easy"),
                (slots.normal, ""),
                (slots.hard,   "-hard"),
            ];

            let mut total_notes    = 0u64;
            let mut total_sections = 0u64;
            let mut total_files    = 0usize;
            let mut first_ppq      = 0u16;
            let mut first_flp_bpm  = None;

            for (pattern_id_opt, difficulty_suffix) in difficulties {
                let Some(pattern_id) = pattern_id_opt else { continue };
                progress(Progress::Stage("Parsing notes"));
                let project = parse_flp(&bytes, Some(*pattern_id), &mut progress)?;
                if first_ppq == 0 {
                    first_ppq    = project.ppq;
                    first_flp_bpm = project.bpm;
                }
                let flp_bpm = project.bpm
                    .or(first_flp_bpm)
                    .context("FLP does not contain a tempo event")?;
                if project.ppq == 0 {
                    bail!("FLP has PPQ 0");
                }
                let song = song_name_from(preset, primary_input);
                let stats = convert_chart(
                    project.notes, project.ppq, preset, &song,
                    &base_output, difficulty_suffix, split_threshold, &mut progress, flp_bpm,
                )?;
                total_notes    += stats.notes.replace(',', "").parse::<u64>().unwrap_or(0);
                total_sections += stats.sections.replace(',', "").parse::<u64>().unwrap_or(0);
                total_files    += stats.files;
            }

            Ok(Stats {
                notes:    total_notes.to_formatted_string(&Locale::en),
                sections: total_sections.to_formatted_string(&Locale::en),
                time:     start_time.elapsed().as_micros() as f64 / 1000.0,
                files:    total_files,
                warnings: String::new(),
            })
        }

        // -----------------------------------------------------------------
        ConversionMode::Batch => {
            // output is a directory. Each input FLP → output_dir/stem.json.
            let all_inputs: Vec<&Path> = std::iter::once(primary_input)
                .chain(extra_inputs.iter().map(|p| p.as_path()))
                .collect();
            let mut total_notes    = 0u64;
            let mut total_sections = 0u64;
            let mut total_files    = 0usize;
            let mut all_warnings   = String::new();

            for path in &all_inputs {
                progress(Progress::Stage("Reading FLP"));
                let bytes = fs::read(path)
                    .with_context(|| format!("reading {}", path.display()))?;
                progress(Progress::Stage("Parsing notes"));
                let project = if is_fsc(&bytes) {
                    parse_fsc(&bytes, &mut progress)?
                } else {
                    parse_flp(&bytes, Some(preset.pattern_id), &mut progress)?
                };
                if project.notes.is_empty() {
                    // Skip inputs with no notes rather than aborting the batch.
                    continue;
                }
                let flp_bpm = project.bpm
                    .or(preset.base_bpm_override)
                    .context("FSC has no tempo event — enable 'Override starting BPM' and set a BPM")?;
                if project.ppq == 0 {
                    bail!("FLP has PPQ 0 ({})", path.display());
                }
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("chart");
                let out_path = output.join(format!("{stem}.json"));
                let song = song_name_from(preset, path);
                let stats = convert_chart(
                    project.notes, project.ppq, preset, &song,
                    &out_path, "", split_threshold, &mut progress, flp_bpm,
                )?;
                total_notes    += stats.notes.replace(',', "").parse::<u64>().unwrap_or(0);
                total_sections += stats.sections.replace(',', "").parse::<u64>().unwrap_or(0);
                total_files    += stats.files;
                if !stats.warnings.is_empty() {
                    all_warnings.push_str(&format!("{}: {}", stem, stats.warnings));
                }
            }

            Ok(Stats {
                notes:    total_notes.to_formatted_string(&Locale::en),
                sections: total_sections.to_formatted_string(&Locale::en),
                time:     start_time.elapsed().as_micros() as f64 / 1000.0,
                files:    total_files,
                warnings: all_warnings,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Core conversion pipeline
// ---------------------------------------------------------------------------

/// Groups `notes` into sections, resolves timing and mustHitSection state,
/// sorts, and writes output JSON for one or more splits (more than one only
/// when `split_threshold` forces a split). All mode-specific note-collection
/// logic lives in `convert_file`; this function only cares about the
/// already-assembled note vec.
///
/// `output` is a base path (`{stem}.{ext}`) that file names are derived
/// from, plus a `_2`, `_3`, ... suffix for additional split files, and
/// `difficulty_suffix` (typically `""`, `"-easy"`, or `"-hard"`).
///
/// When `preset.split_metadata` is false (default), each split writes one
/// combined file: `{stem}{difficulty_suffix}.{ext}`, the original format
/// with notes nested under song metadata. When true, each split writes two
/// files instead: `{stem}-chart{difficulty_suffix}.{ext}` (notes only) and
/// `{stem}-metadata{difficulty_suffix}.{ext}` (song metadata only).
fn convert_chart(
    notes: Vec<super::flp::FlNote>,
    ppq: u16,
    preset: &ConversionPreset,
    song: &str,
    output: &Path,
    difficulty_suffix: &str,
    split_threshold: Option<u64>,
    progress: &mut dyn FnMut(Progress),
    flp_bpm: f64,
) -> Result<Stats> {
    let base_bpm = preset.base_bpm_override.unwrap_or(flp_bpm);
    // chart_bpm here is the *initial* (section 0) tempo. Per-section
    // chart_bpm is resolved later by resolve_section_timing.
    let chart_bpm = base_bpm * preset.bpm_multiplier;
    // Auto scroll speed tracks the BPM multiplier: speeding the chart up
    // should speed the scroll rate up proportionally.
    let speed = preset.speed.unwrap_or(chart_bpm / 50.0);
    // Section boundaries are always ppq*4 ticks wide regardless of tempo.
    let pulses_per_section = ppq as f64 * 4.0;
    // Clamped defensively: a hand-edited preset with an out-of-range value
    // would overflow 10^decimals to infinity/NaN, which is not valid JSON.
    // The UI already caps this at 13.
    let precision = 10_f64.powi(preset.decimals.min(13) as i32);

    // 256-entry pitch lookup (indexed by MIDI pitch) replaces a per-note
    // linear scan over preset.mapping — O(1) instead of O(mapping.len()).
    let pitch_lookup = build_pitch_lookup(preset);

    // -----------------------------------------------------------------
    progress(Progress::Stage("Grouping notes into sections"));
    let total_notes = notes.len();

    // One cheap extra pass to pre-size the section Vec, avoiding
    // realloc/copy as notes arrive.
    let max_position = notes.iter().map(|note| note.position).max();
    let estimated_sections = max_position.map_or(0, |position| {
        (position as f64 / pulses_per_section).floor() as usize + 1
    });
    let mut sections: Vec<Section> = Vec::with_capacity(estimated_sections);
    let mut notes_to_write = 0usize;

    for (index, note) in notes.into_iter().enumerate() {
        if total_notes > 0 && (index % PROGRESS_BATCH == 0 || index + 1 == total_notes) {
            progress(Progress::Notes { done: index + 1, total: total_notes });
        }
        let section_index =
            (note.position as f64 / pulses_per_section).floor() as u32 as usize;
        if section_index >= sections.len() {
            sections.resize_with(section_index + 1, Section::default);
        }
        let section = &mut sections[section_index];

        if note.key == preset.alt_marker_pitch {
            section.alt = true;
            continue;
        }
        if note.key == PITCH_BPM_CHANGE {
            section.bpm_change = true;
            continue;
        }
        if note.key == PITCH_ALT_ANIM {
            section.alt_anim = true;
            continue;
        }
        if note.key == PITCH_MUST_HIT_TRUE {
            section.record_must_hit_marker(note.position, true);
            continue;
        }
        if note.key == PITCH_MUST_HIT_FALSE {
            section.record_must_hit_marker(note.position, false);
            continue;
        }

        let Some((direction, player)) = pitch_lookup[note.key as usize] else {
            continue;
        };

        // Store raw ticks — NOT yet converted to ms. The correct ms_per_pulse
        // depends on this note's section's resolved BPM (set by
        // resolve_section_timing after all markers are read), which in turn
        // depends on how many PITCH_BPM_CHANGE markers appear in sections
        // at or before this one. That can't be known while notes are still
        // arriving out of order.
        section.notes.push(ChartNote {
            position: note.position,
            length:   note.length,
            direction,
            player,
            portamento: note.portamento,
            // Velocity < 64 = below 50%. Treated as a forced sustain:
            // always gets a tail regardless of note length.
            force_sustain: note.velocity < 64,
        });
        notes_to_write += 1;
    }

    let section_count = sections.len();

    // -----------------------------------------------------------------
    // Resolution passes (strictly sequential; carry state forward)
    resolve_must_hit_sections(&mut sections);
    resolve_section_timing(
        &mut sections,
        pulses_per_section,
        ppq,
        base_bpm,
        preset.bpm_multiplier,
        preset.sustain_threshold_steps,
        &preset.bpm_changes,
    )?;

    // -----------------------------------------------------------------
    progress(Progress::Stage("Sorting sections"));
    // Every section's notes are independent, so sorting can run across all
    // available cores.
    let mut notes_sorted = 0usize;
    for chunk in sections.chunks_mut(PARALLEL_RENDER_CHUNK) {
        chunk.par_iter_mut().for_each(|section| {
            let must_hit = section.must_hit_section;
            section.notes.sort_unstable_by(|a, b| {
                let a_data = a.direction + if a.player != must_hit { 4 } else { 0 };
                let b_data = b.direction + if b.player != must_hit { 4 } else { 0 };
                // Sort by raw tick (u32) rather than converted ms (f64):
                // within one section tempo is uniform, so the positive
                // constant ms_per_pulse preserves order — integer comparison
                // is exact and avoids f64::total_cmp entirely.
                a.position.cmp(&b.position).then(a_data.cmp(&b_data))
            });
        });
        notes_sorted += chunk.iter().map(|s| s.notes.len()).sum::<usize>();
        if notes_to_write > 0 {
            progress(Progress::Notes { done: notes_sorted, total: notes_to_write });
        }
    }

    // -----------------------------------------------------------------
    // Split boundary computation
    let mut file_ranges: Vec<(usize, usize)> = Vec::new();
    let mut oversized_sections: Vec<usize> = Vec::new();
    match split_threshold {
        None => {
            file_ranges.push((0, section_count));
        }
        Some(threshold) => {
            let mut file_start = 0usize;
            let mut file_notes = 0u64;
            for (i, section) in sections.iter().enumerate() {
                let n = section.notes.len() as u64;
                if file_notes > 0 && file_notes + n > threshold {
                    file_ranges.push((file_start, i - file_start));
                    file_start = i;
                    file_notes = 0;
                }
                if n > threshold {
                    oversized_sections.push(i);
                }
                file_notes += n;
            }
            if file_start < section_count {
                file_ranges.push((file_start, section_count - file_start));
            }
        }
    }

    let file_count = file_ranges.len();
    let warnings = if oversized_sections.is_empty() {
        String::new()
    } else {
        format!(
            " Warning: {} section{} exceeded the split threshold and could not be split \
             (section{} {}).",
            oversized_sections.len(),
            if oversized_sections.len() == 1 { "" } else { "s" },
            if oversized_sections.len() == 1 { "" } else { "s" },
            oversized_sections
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(", "),
        )
    };

    // -----------------------------------------------------------------
    // Write loop. Combined mode writes one file per split (original
    // format); split-metadata mode writes a chart file + a metadata
    // file per split (see `ConversionPreset::split_metadata`).
    let sections_cell = RefCell::new(sections);
    progress(Progress::Stage("Writing chart JSON"));
    let progress_dyn: &mut dyn FnMut(Progress) = progress;
    let mut total_notes_written = 0usize;

    let base_stem = output.file_stem().and_then(|s| s.to_str()).unwrap_or("chart");
    let base_ext  = output.extension().and_then(|s| s.to_str()).unwrap_or("json");

    for (file_index, (file_start, file_section_count)) in file_ranges.iter().copied().enumerate() {
        let split_suffix = if file_index == 0 {
            String::new()
        } else {
            format!("_{}", file_index + 1)
        };

        // Per-split bpm reflects the active tempo at that split point.
        let file_bpm = sections_cell.borrow()[file_start].chart_bpm;

        let file_notes_to_write: usize = {
            let s = sections_cell.borrow();
            s[file_start..file_start + file_section_count]
                .iter()
                .map(|sec| sec.notes.len())
                .sum()
        };

        let stream = SectionStream {
            sections:      &sections_cell,
            start:         file_start,
            section_count: file_section_count,
            trim_sustains: preset.trim_sustains,
            notes_to_write: file_notes_to_write,
            progress:      RefCell::new(progress_dyn),
            note_count:    Cell::new(0),
            precision,
            parallel: !preset.pretty_json,
        };

        if preset.split_metadata {
            // Two files: "{stem}-chart{suffix}.{ext}" and "{stem}-metadata{suffix}.{ext}".
            let chart_path = output.with_file_name(format!(
                "{base_stem}-chart{difficulty_suffix}{split_suffix}.{base_ext}"
            ));
            let metadata_path = output.with_file_name(format!(
                "{base_stem}-metadata{difficulty_suffix}{split_suffix}.{base_ext}"
            ));

            let chart_root = ChartOnlyRoot { notes: stream };
            total_notes_written += write_chart_file(&chart_path, &chart_root, preset.pretty_json)?;

            let metadata = SongMetadata {
                song,
                bpm:          round_num(file_bpm, BPM_SPEED_PRECISION),
                speed:        round_num(speed, BPM_SPEED_PRECISION),
                needs_voices: preset.needs_voices,
                player1:      &preset.player1,
                player2:      &preset.player2,
                gf_version:   &preset.gf_version,
                song_creator: &preset.song_creator,
                stage:        &preset.stage,
                valid_score:  true,
            };
            write_metadata_file(&metadata_path, &metadata, preset.pretty_json)?;
        } else {
            // Original combined format: one file, notes nested under song metadata.
            let file_path = output.with_file_name(format!(
                "{base_stem}{difficulty_suffix}{split_suffix}.{base_ext}"
            ));

            let root = ChartRoot {
                song: SongData {
                    song,
                    bpm:          round_num(file_bpm, BPM_SPEED_PRECISION),
                    speed:        round_num(speed, BPM_SPEED_PRECISION),
                    needs_voices: preset.needs_voices,
                    player1:      &preset.player1,
                    player2:      &preset.player2,
                    gf_version:   &preset.gf_version,
                    song_creator: &preset.song_creator,
                    stage:        &preset.stage,
                    valid_score:  true,
                    notes: stream,
                },
            };
            total_notes_written += write_combined_file(&file_path, &root, preset.pretty_json)?;
        }
    }

    Ok(Stats {
        notes:    total_notes_written.to_formatted_string(&Locale::en),
        sections: section_count.to_formatted_string(&Locale::en),
        time:     0.0, // set by convert_file for the whole operation
        // Split mode writes a chart file *and* a metadata file per split;
        // combined mode writes just one file per split.
        files:    if preset.split_metadata { file_count * 2 } else { file_count },
        warnings,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Writes the chart-only JSON (`[ { "notes": [...] } ]`) to `path`.
/// Returns the note count written, read back from the drained `SectionStream`.
fn write_chart_file(path: &Path, root: &ChartOnlyRoot<'_>, pretty: bool) -> Result<usize> {
    let file = fs::File::create(path)
        .with_context(|| format!("creating {}", path.display()))?;
    let mut writer = BufWriter::with_capacity(16 * 1024 * 1024, file);
    if pretty {
        serde_json::to_writer_pretty(&mut writer, &[root])?;
    } else {
        serde_json::to_writer(&mut writer, &[root])?;
    }
    writer.flush().with_context(|| format!("writing {}", path.display()))?;
    Ok(root.notes.note_count.get())
}

/// Writes the original combined-format JSON (`{ "song": { ...metadata,
/// "notes": [...] } }`, not array-wrapped) to `path`. Returns the note
/// count written, read back from the drained `SectionStream`.
fn write_combined_file(path: &Path, root: &ChartRoot<'_, '_>, pretty: bool) -> Result<usize> {
    let file = fs::File::create(path)
        .with_context(|| format!("creating {}", path.display()))?;
    let mut writer = BufWriter::with_capacity(16 * 1024 * 1024, file);
    if pretty {
        serde_json::to_writer_pretty(&mut writer, root)?;
    } else {
        serde_json::to_writer(&mut writer, root)?;
    }
    writer.flush().with_context(|| format!("writing {}", path.display()))?;
    Ok(root.song.notes.note_count.get())
}

/// Writes the song-metadata JSON (`[ { "song": ..., "bpm": ..., ... } ]`)
/// to `path`. Small and non-streaming, so a plain `BufWriter` is enough.
fn write_metadata_file(path: &Path, metadata: &SongMetadata<'_>, pretty: bool) -> Result<()> {
    let file = fs::File::create(path)
        .with_context(|| format!("creating {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    if pretty {
        serde_json::to_writer_pretty(&mut writer, &[metadata])?;
    } else {
        serde_json::to_writer(&mut writer, &[metadata])?;
    }
    writer.flush().with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Returns the song name for the chart JSON: the preset's `song_name` if
/// non-empty, otherwise the input file's stem.
fn song_name_from<'a>(preset: &'a ConversionPreset, input: &'a Path) -> String {
    if preset.song_name.trim().is_empty() {
        input
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("untitled")
            .to_owned()
    } else {
        preset.song_name.clone()
    }
}