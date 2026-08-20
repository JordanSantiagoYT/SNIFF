mod converter;
mod cli;

use std::path::PathBuf;

use converter::{convert_file, convert_json_to_fsc, ConversionMode, ConversionPreset};
use eframe::egui;
use serde::{Deserialize, Serialize};

/// Top-level direction: which way the conversion is going.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Direction {
    /// FLP / FSC -> Psych Engine chart JSON (the original pipeline).
    #[default]
    ToChart,
    /// Psych Engine chart JSON -> FL Studio Score (.fsc).
    ToScore,
}

// ---------------------------------------------------------------------------
// Persistent configuration
// ---------------------------------------------------------------------------

/// The subset of App state that survives across sessions. Everything else
/// (file paths, conversion progress, runtime pattern data) is session-only.
#[derive(Serialize, Deserialize)]
struct PersistedState {
    /// 0 = ToChart, 1 = ToScore. Stored as u8 so it's stable across renames.
    direction: u8,
    preset: ConversionPreset,
    split_threshold: Option<u64>,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            direction: 0,
            preset: ConversionPreset::default(),
            split_threshold: None,
        }
    }
}

/// Returns the path to SNIFF's config file, creating the directory if needed.
/// Returns None if the platform config dir can't be determined.
fn config_path() -> Option<std::path::PathBuf> {
    let mut path = dirs::config_dir()?;
    path.push("SNIFF");
    std::fs::create_dir_all(&path).ok()?;
    path.push("config.json");
    Some(path)
}

/// Loads persisted state from disk. Returns defaults silently on any error
/// (missing file, parse error, schema mismatch from an old version) so
/// first-run and upgrades are both handled gracefully.
fn load_config() -> PersistedState {
    let path = match config_path() {
        Some(p) => p,
        None => return PersistedState::default(),
    };
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(_) => return PersistedState::default(),
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

/// Saves persisted state to disk. Errors are silently ignored — a failed
/// save is annoying but not fatal.
fn save_config(state: &PersistedState) {
    let path = match config_path() {
        Some(p) => p,
        None => return,
    };
    if let Ok(json) = serde_json::to_vec_pretty(state) {
        let _ = std::fs::write(path, json);
    }
}

fn main() -> eframe::Result<()> {
    // If any arguments were passed, run headlessly in CLI mode and exit.
    // The GUI launches only when the binary is invoked with no arguments.
    if std::env::args().len() > 1 {
        cli::run_cli();
        // run_cli() exits on error; if we reach here it succeeded.
        std::process::exit(0);
    }
    eframe::run_native(
        "SNIFF",
        eframe::NativeOptions::default(),
        Box::new(|_| Ok(Box::<App>::default())),
    )
}

/// Which files the user has selected, shaped by the current pipeline mode.
enum PipelineInputs {
    /// Single FLP → single output JSON.
    Single(Option<PathBuf>),
    /// Multiple FLPs merged into one chart on a shared timeline.
    Merge(Vec<PathBuf>),
    /// Single FLP, three potential outputs (easy / normal / hard).
    SplitDifficulties(Option<PathBuf>),
    /// Multiple FLPs, each converted independently into an output directory.
    Batch(Vec<PathBuf>),
}

impl PipelineInputs {
    fn primary(&self) -> Option<&PathBuf> {
        match self {
            PipelineInputs::Single(p) | PipelineInputs::SplitDifficulties(p) => p.as_ref(),
            PipelineInputs::Merge(v) | PipelineInputs::Batch(v) => v.first(),
        }
    }

    fn extra(&self) -> &[PathBuf] {
        match self {
            PipelineInputs::Single(_) | PipelineInputs::SplitDifficulties(_) => &[],
            PipelineInputs::Merge(v) | PipelineInputs::Batch(v) => {
                if v.is_empty() { &[] } else { &v[1..] }
            }
        }
    }

    fn mode(&self) -> ConversionMode {
        match self {
            PipelineInputs::Single(_) => ConversionMode::Single,
            PipelineInputs::Merge(_) => ConversionMode::Merge,
            PipelineInputs::SplitDifficulties(_) => ConversionMode::SplitDifficulties,
            PipelineInputs::Batch(_) => ConversionMode::Batch,
        }
    }

    /// Returns a display string summarising the selected inputs.
    fn display(&self) -> String {
        match self {
            PipelineInputs::Single(None) | PipelineInputs::SplitDifficulties(None) => {
                "No input selected".to_owned()
            }
            PipelineInputs::Single(Some(p)) | PipelineInputs::SplitDifficulties(Some(p)) => {
                p.display().to_string()
            }
            PipelineInputs::Merge(v) | PipelineInputs::Batch(v) if v.is_empty() => {
                "No inputs selected".to_owned()
            }
            PipelineInputs::Merge(v) => {
                format!("{} FLPs (first: {})", v.len(), v[0].display())
            }
            PipelineInputs::Batch(v) => {
                format!("{} FLPs (first: {})", v.len(), v[0].display())
            }
        }
    }
}

impl Default for PipelineInputs {
    fn default() -> Self {
        PipelineInputs::Single(None)
    }
}

struct App {
    /// Which conversion direction is active.
    direction: Direction,
    // --- ToChart fields (FLP/FSC -> JSON) ---
    inputs: PipelineInputs,
    /// Output file for Single/Merge/SplitDifficulties; output directory for Batch.
    output: Option<PathBuf>,
    // --- ToScore fields (JSON -> FSC) ---
    /// Input chart JSON for the ToScore direction.
    score_input: Option<PathBuf>,
    /// Output FSC path for the ToScore direction.
    score_output: Option<PathBuf>,
    preset: ConversionPreset,
    status: String,
    /// Patterns from the first (or only) inspected FLP.
    patterns: Vec<converter::PatternInfo>,
    detected_bpm: Option<f64>,
    converting: bool,
    progress_stage: String,
    progress_done: usize,
    progress_total: usize,
    progress_rx: Option<std::sync::mpsc::Receiver<ProgressUpdate>>,
    /// None = no splitting. Some(n) = start a new output file when
    /// adding the next section would push the running note count over n.
    split_threshold: Option<u64>,
    /// Merge mode: total bpm_change_count across all inputs after a scan.
    /// None means the scan hasn't run yet (UI shows the first-file count).
    merge_scan_total: Option<usize>,
}

impl Default for App {
    fn default() -> Self {
        let saved = load_config();
        Self {
            direction: if saved.direction == 1 { Direction::ToScore } else { Direction::ToChart },
            inputs: PipelineInputs::default(),
            output: None,
            score_input: None,
            score_output: None,
            preset: saved.preset,
            status: String::new(),
            patterns: Vec::new(),
            detected_bpm: None,
            converting: false,
            progress_stage: String::new(),
            progress_done: 0,
            progress_total: 0,
            progress_rx: None,
            split_threshold: saved.split_threshold,
            merge_scan_total: None,
        }
    }
}

enum ProgressUpdate {
    Progress(converter::Progress),
    Finished(Result<converter::Stats, String>),
}

impl App {
    /// Resizes `preset.bpm_changes` to match the detected marker count.
    fn sync_bpm_changes(&mut self) {
        let count = match &self.inputs {
            PipelineInputs::Merge(_) => {
                self.merge_scan_total.unwrap_or_else(|| {
                    self.patterns
                        .iter()
                        .find(|p| p.id == self.preset.pattern_id)
                        .map_or(0, |p| p.bpm_change_count)
                })
            }
            _ => self
                .patterns
                .iter()
                .find(|p| p.id == self.preset.pattern_id)
                .map_or(0, |p| p.bpm_change_count),
        };
        let default = self.detected_bpm.unwrap_or(120.0);
        self.preset.bpm_changes.resize(count, default);
    }

    /// Inspect the first input and populate patterns/BPM. Called after any
    /// file pick. In Merge/Batch mode, only the first file is inspected here;
    /// the full scan for bpm_change_count happens via the "Scan all inputs" button.
    fn inspect_first(&mut self, path: &PathBuf) {
        match converter::inspect(path) {
            Ok(info) => {
                if let Some(first) = info.patterns.first() {
                    self.preset.pattern_id = first.id;
                }
                self.patterns = info.patterns;
                self.detected_bpm = info.bpm;
                self.status.clear();
                self.merge_scan_total = None;
                self.sync_bpm_changes();
            }
            Err(error) => {
                self.patterns.clear();
                self.detected_bpm = None;
                self.status = format!("Could not inspect FLP: {error:#}");
            }
        }
    }

    /// Opens a file picker and adds the result to the inputs.
    /// Single/SplitDifficulties: single-file picker, replaces current selection.
    /// Merge/Batch: multi-file picker, appends all chosen files to the list.
    fn pick_input(&mut self) {
        let mode = self.inputs.mode();
        let was_empty = match &self.inputs {
            PipelineInputs::Merge(v) | PipelineInputs::Batch(v) => v.is_empty(),
            _ => false,
        };

        match mode {
            ConversionMode::Single | ConversionMode::SplitDifficulties => {
                let Some(path) = rfd::FileDialog::new()
                    .add_filter("FL Studio project/score", &["flp", "fsc"])
                    .pick_file()
                else { return };
                self.output = Some(path.with_extension("json"));
                self.inspect_first(&path.clone());
                self.inputs = match mode {
                    ConversionMode::Single => PipelineInputs::Single(Some(path)),
                    _ => PipelineInputs::SplitDifficulties(Some(path)),
                };
            }
            ConversionMode::Merge | ConversionMode::Batch => {
                let Some(paths) = rfd::FileDialog::new()
                    .add_filter("FL Studio project/score", &["flp", "fsc"])
                    .pick_files()
                else { return };
                if was_empty {
                    if let Some(first) = paths.first() {
                        self.inspect_first(&first.clone());
                        self.output = if mode == ConversionMode::Batch {
                            first.parent().map(|p| p.to_path_buf())
                        } else {
                            Some(first.with_extension("json"))
                        };
                    }
                }
                match &mut self.inputs {
                    PipelineInputs::Merge(v) | PipelineInputs::Batch(v) => v.extend(paths),
                    _ => {}
                }
            }
        }
    }

    /// Removes the file at `index` from a Merge/Batch list. If the first file
    /// is removed, re-inspects the new first file to keep patterns/BPM current.
    fn remove_input(&mut self, index: usize) {
        match &mut self.inputs {
            PipelineInputs::Merge(v) | PipelineInputs::Batch(v) => {
                if index >= v.len() { return; }
                v.remove(index);
                self.merge_scan_total = None;
                if index == 0 {
                    if let Some(new_first) = v.first().cloned() {
                        self.inspect_first(&new_first);
                        self.output = if matches!(self.inputs, PipelineInputs::Batch(_)) {
                            new_first.parent().map(|p| p.to_path_buf())
                        } else {
                            Some(new_first.with_extension("json"))
                        };
                    } else {
                        self.patterns.clear();
                        self.detected_bpm = None;
                        self.output = None;
                    }
                }
            }
            _ => {}
        }
    }

    /// Switch to a new pipeline mode, preserving the first input if possible.
    fn switch_mode(&mut self, new_mode: ConversionMode) {
        let first = self.inputs.primary().cloned();
        self.merge_scan_total = None;
        self.inputs = match new_mode {
            ConversionMode::Single => PipelineInputs::Single(first),
            ConversionMode::Merge => PipelineInputs::Merge(first.into_iter().collect()),
            ConversionMode::SplitDifficulties => PipelineInputs::SplitDifficulties(first),
            ConversionMode::Batch => PipelineInputs::Batch(first.into_iter().collect()),
        };
        if let Some(first_path) = self.inputs.primary() {
            self.output = match self.inputs.mode() {
                ConversionMode::Batch => first_path.parent().map(|p| p.to_path_buf()),
                _ => Some(first_path.with_extension("json")),
            };
        }
    }

    fn scan_merge_inputs(&mut self) {
        let paths: Vec<PathBuf> = match &self.inputs {
            PipelineInputs::Merge(v) => v.clone(),
            _ => return,
        };
        let pattern_id = self.preset.pattern_id;
        match converter::scan_merge_inputs(&paths, pattern_id) {
            Ok(total) => {
                self.merge_scan_total = Some(total);
                self.status = format!("Scan complete: {total} BPM change(s) across all inputs.");
                self.sync_bpm_changes();
            }
            Err(e) => {
                self.status = format!("Scan failed: {e:#}");
            }
        }
    }

    fn load_preset(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("SNIFF-compatible preset", &["json"])
            .pick_file()
        else {
            return;
        };
        match (|| -> anyhow::Result<ConversionPreset> {
            let bytes = std::fs::read(&path)?;
            Ok(serde_json::from_slice(&bytes)?)
        })() {
            Ok(preset) => {
                self.preset = preset;
                self.status = format!("Loaded {}", path.display());
            }
            Err(error) => self.status = format!("Could not load preset: {error:#}"),
        }
    }

    fn save_preset(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Preset", &["json"])
            .set_file_name("fnf-converter-preset.json")
            .save_file()
        else {
            return;
        };
        match (|| -> anyhow::Result<()> {
            let json = serde_json::to_vec_pretty(&self.preset)?;
            std::fs::write(&path, json)?;
            Ok(())
        })() {
            Ok(()) => self.status = format!("Saved {}", path.display()),
            Err(error) => self.status = format!("Could not save preset: {error:#}"),
        }
    }

    /// Kicks off a JSON -> FSC conversion on a background thread.
    fn start_score_convert(&mut self) {
        let Some(input) = self.score_input.clone() else {
            self.status = "Choose an input chart JSON first.".into();
            return;
        };
        let Some(output) = self.score_output.clone() else {
            self.status = "Choose an output FSC location first.".into();
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        self.progress_rx = Some(rx);
        self.converting = true;
        self.progress_stage = "Starting...".into();
        self.progress_done = 0;
        self.progress_total = 0;
        self.status.clear();

        std::thread::spawn(move || {
            let progress_tx = tx.clone();
            let result = convert_json_to_fsc(
                &input,
                &output,
                &mut |update| { let _ = progress_tx.send(ProgressUpdate::Progress(update)); },
            );
            let _ = tx.send(ProgressUpdate::Finished(result.map_err(|e| format!("{e:#}"))));
        });
    }

    fn start_convert(&mut self) {
        let Some(primary) = self.inputs.primary().cloned() else {
            self.status = "Choose an input FLP first.".into();
            return;
        };
        let Some(output) = self.output.clone() else {
            self.status = "Choose an output location first.".into();
            return;
        };
        let extra: Vec<PathBuf> = self.inputs.extra().to_vec();
        let mode = self.inputs.mode();
        let preset = self.preset.clone();
        let split_threshold = self.split_threshold;
        let (tx, rx) = std::sync::mpsc::channel();
        self.progress_rx = Some(rx);
        self.converting = true;
        self.progress_stage = "Starting...".into();
        self.progress_done = 0;
        self.progress_total = 0;
        self.status.clear();

        std::thread::spawn(move || {
            let progress_tx = tx.clone();
            let result = convert_file(
                &primary,
                &extra,
                &output,
                &preset,
                &mode,
                split_threshold,
                |update| {
                    let _ = progress_tx.send(ProgressUpdate::Progress(update));
                },
            );
            let _ = tx.send(ProgressUpdate::Finished(result.map_err(|e| format!("{e:#}"))));
        });
    }

    /// Builds a PersistedState snapshot from the current App state and writes
    /// it to disk. Called at the end of every update() so changes are saved
    /// whenever the user interacts with the UI.
    fn save_state(&self) {
        save_config(&PersistedState {
            direction: if self.direction == Direction::ToScore { 1 } else { 0 },
            preset: self.preset.clone(),
            split_threshold: self.split_threshold,
        });
    }

    /// Drains any progress/completion messages from the background
    /// conversion thread. Returns true while a conversion is still running,
    /// so the caller knows to keep repainting.
    fn poll_conversion(&mut self) -> bool {
        let Some(rx) = self.progress_rx.take() else {
            return false;
        };
        let mut finished = false;
        while let Ok(update) = rx.try_recv() {
            match update {
                ProgressUpdate::Progress(converter::Progress::Stage(stage)) => {
                    self.progress_stage = stage.to_owned();
                    self.progress_done = 0;
                    self.progress_total = 0;
                }
                ProgressUpdate::Progress(converter::Progress::Notes { done, total }) => {
                    self.progress_done = done;
                    self.progress_total = total;
                }
                ProgressUpdate::Finished(result) => {
                    self.converting = false;
                    self.status = match result {
                        Ok(stats) => {
                            let output_str = match self.direction {
                                Direction::ToScore => self.score_output.as_ref(),
                                Direction::ToChart => self.output.as_ref(),
                            }.map_or_else(
                                || "(no output selected)".to_owned(),
                                |p| p.display().to_string(),
                            );
                            let file_suffix = if stats.files > 1 {
                                format!(" across {} files", stats.files)
                            } else {
                                String::new()
                            };
                            format!(
                                "Wrote {} notes in {} sections to {}{} in {:.1} ms{}",
                                stats.notes,
                                stats.sections,
                                output_str,
                                file_suffix,
                                stats.time,
                                stats.warnings,
                            )
                        }
                        Err(error) => format!("Conversion failed: {error}"),
                    };
                    finished = true;
                }
            }
        }
        if !finished {
            self.progress_rx = Some(rx);
        }
        self.converting
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        if self.poll_conversion() {
            ctx.request_repaint();
        }
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("SNIFF");

            // --- Direction strip ---
            ui.horizontal(|ui| {
                if ui.selectable_label(self.direction == Direction::ToChart, "FLP / FSC -> Chart JSON").clicked() {
                    self.direction = Direction::ToChart;
                }
                if ui.selectable_label(self.direction == Direction::ToScore, "Chart JSON -> FSC").clicked() {
                    self.direction = Direction::ToScore;
                }
            });
            ui.separator();

            match self.direction {
                Direction::ToChart => {
                    ui.label("Reads piano-roll notes directly from the FLP/FSC. Fixed markers: 53/54 toggle mustHitSection, 56 = BPM change, 57 = altAnim. Configurable alt-note marker below.");
                    ui.separator();

                    // --- Pipeline mode selector ---
                    ui.collapsing("Pipeline", |ui| {
                        ui.horizontal(|ui| {
                            for (label, mode) in [
                                ("Single",             ConversionMode::Single),
                                ("Merge Inputs",       ConversionMode::Merge),
                                ("Split Difficulties", ConversionMode::SplitDifficulties),
                                ("Batch",              ConversionMode::Batch),
                            ] {
                                if ui.selectable_label(self.inputs.mode() == mode, label).clicked()
                                    && self.inputs.mode() != mode
                                {
                                    self.switch_mode(mode);
                                }
                            }
                        });
                        match self.inputs.mode() {
                            ConversionMode::Single => {
                                ui.label("One input, one chart JSON.");
                            }
                            ConversionMode::Merge => {
                                ui.label("Multiple FLPs merged onto a single shared timeline. All inputs must share the same BPM layout and pattern structure.");
                                if ui.add_enabled(!self.converting, egui::Button::new("Scan all inputs for BPM changes")).clicked() {
                                    self.scan_merge_inputs();
                                }
                                if let Some(total) = self.merge_scan_total {
                                    ui.label(format!("Total BPM changes across all inputs: {total}"));
                                }
                            }
                            ConversionMode::SplitDifficulties => {
                                ui.label("One FLP, up to three JSONs. Patterns whose names contain 'easy', 'normal', or 'hard' (case-insensitive) are each written to their own file.");
                            }
                            ConversionMode::Batch => {
                                ui.label("Multiple FLPs converted independently, each written to the output directory.");
                            }
                        }
                    });

                    // --- Input / output pickers ---
                    match self.inputs.mode() {
                        ConversionMode::Single | ConversionMode::SplitDifficulties => {
                            ui.horizontal(|ui| {
                                if ui.add_enabled(!self.converting, egui::Button::new("Choose FLP / FSC...")).clicked() {
                                    self.pick_input();
                                }
                                ui.monospace(self.inputs.display());
                            });
                        }
                        ConversionMode::Merge | ConversionMode::Batch => {
                            let paths: Vec<std::path::PathBuf> = match &self.inputs {
                                PipelineInputs::Merge(v) | PipelineInputs::Batch(v) => v.clone(),
                                _ => vec![],
                            };
                            let mut to_remove: Option<usize> = None;
                            if paths.is_empty() {
                                ui.label("No inputs selected.");
                            } else {
                                // Cap the visible list at ~8 rows; scrollable if more are added.
                                egui::ScrollArea::vertical()
                                    .id_salt("input_list_scroll")
                                    .max_height(ui.text_style_height(&egui::TextStyle::Body) * 8.5)
                                    .show(ui, |ui| {
                                        for (i, path) in paths.iter().enumerate() {
                                            ui.horizontal(|ui| {
                                                if ui.add_enabled(!self.converting, egui::Button::new("-").min_size(egui::vec2(20.0, 0.0))).clicked() {
                                                    to_remove = Some(i);
                                                }
                                                let label = if i == 0 {
                                                    format!("[1 - primary] {}", path.display())
                                                } else {
                                                    format!("[{}] {}", i + 1, path.display())
                                                };
                                                ui.monospace(label);
                                            });
                                        }
                                    });
                            }
                            if let Some(i) = to_remove {
                                self.remove_input(i);
                            }
                            if ui.add_enabled(!self.converting, egui::Button::new("+ Add FLP / FSC...")).clicked() {
                                self.pick_input();
                            }
                        }
                    }
                    ui.horizontal(|ui| {
                        let is_batch = matches!(self.inputs, PipelineInputs::Batch(_));
                        let out_label = if is_batch { "Output folder..." } else { "Output JSON..." };
                        if ui.add_enabled(!self.converting, egui::Button::new(out_label)).clicked() {
                            if is_batch {
                                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                    self.output = Some(path);
                                }
                            } else if let Some(path) = rfd::FileDialog::new()
                                .add_filter("JSON", &["json"])
                                .set_file_name("chart.json")
                                .save_file()
                            {
                                self.output = Some(path);
                            }
                        }
                        ui.monospace(self.output.as_ref().map_or_else(
                            || "No output selected".to_owned(),
                            |p| p.display().to_string(),
                        ));
                    });

                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label("BPM multiplier");
                        ui.add(egui::DragValue::new(&mut self.preset.bpm_multiplier).speed(0.05).range(1.0..=f64::INFINITY));
                        ui.label("Sustain threshold (steps)");
                        ui.add(egui::DragValue::new(&mut self.preset.sustain_threshold_steps).range(0.0..=128.0));
                    });
                    ui.horizontal(|ui| {
                        let mut custom_bpm = self.preset.base_bpm_override.is_some();
                        if ui.checkbox(&mut custom_bpm, "Override starting BPM").changed() {
                            self.preset.base_bpm_override = if custom_bpm {
                                Some(self.preset.base_bpm_override.or(self.detected_bpm).unwrap_or(120.0))
                            } else {
                                None
                            };
                        }
                        match self.preset.base_bpm_override.as_mut() {
                            Some(bpm) => {
                                ui.add(egui::DragValue::new(bpm).speed(0.5).range(1.0..=f64::INFINITY));
                            }
                            None => {
                                ui.label(self.detected_bpm.map_or_else(
                                    || "auto -- choose an FLP to read".to_owned(),
                                    |bpm| format!("auto -- {bpm:.3} (from FLP)"),
                                ));
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        let auto_speed = self
                            .detected_bpm
                            .map(|bpm| bpm * self.preset.bpm_multiplier / 50.0);
                        let mut custom = self.preset.speed.is_some();
                        if ui.checkbox(&mut custom, "Custom scroll speed").changed() {
                            self.preset.speed = if custom {
                                Some(self.preset.speed.or(auto_speed).unwrap_or(1.0))
                            } else {
                                None
                            };
                        }
                        match self.preset.speed.as_mut() {
                            Some(speed) => {
                                ui.add(egui::DragValue::new(speed).speed(0.01).range(0.05..=f64::INFINITY));
                            }
                            None => {
                                ui.label(auto_speed.map_or_else(
                                    || "auto -- choose an FLP to compute".to_owned(),
                                    |speed| format!("auto -- {speed:.3} (chart BPM / 50)"),
                                ));
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Strum Time Precision");
                        ui.add(egui::DragValue::new(&mut self.preset.decimals).speed(1).range(1..=13));
                    });
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.preset.trim_sustains, "Trim sustain lengths to whole steps");
                        ui.checkbox(&mut self.preset.pretty_json, "Pretty Print JSON");
                    });

                    ui.horizontal(|ui| {
                        ui.label("Pitch 58 marker");
                        ui.add(egui::DragValue::new(&mut self.preset.alt_marker_pitch).range(0..=127));
                        if !matches!(self.inputs, PipelineInputs::SplitDifficulties(_)) {
                            ui.label("Pattern");
                            if self.patterns.is_empty() {
                                ui.add(egui::DragValue::new(&mut self.preset.pattern_id).range(0..=u16::MAX));
                                ui.label("(choose an FLP to pick by name)");
                            } else {
                                let selected_text = self
                                    .patterns
                                    .iter()
                                    .find(|pattern| pattern.id == self.preset.pattern_id)
                                    .map_or_else(
                                        || format!("Pattern id {} (not in this FLP)", self.preset.pattern_id),
                                        |pattern| pattern.display_name(),
                                    );
                                egui::ComboBox::from_id_salt("pattern_picker")
                                    .selected_text(selected_text)
                                    .show_ui(ui, |ui| {
                                        let before = self.preset.pattern_id;
                                        for pattern in &self.patterns {
                                            ui.selectable_value(
                                                &mut self.preset.pattern_id,
                                                pattern.id,
                                                pattern.display_name(),
                                            );
                                        }
                                        if self.preset.pattern_id != before {
                                            self.merge_scan_total = None;
                                            self.sync_bpm_changes();
                                        }
                                    });
                            }
                        } else {
                            ui.label("Pattern: auto-detected (easy / normal / hard)");
                        }
                        ui.label("Song name");
                        ui.text_edit_singleline(&mut self.preset.song_name);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Player (BF)");
                        ui.text_edit_singleline(&mut self.preset.player1);
                        ui.label("Opponent");
                        ui.text_edit_singleline(&mut self.preset.player2);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Girlfriend");
                        ui.text_edit_singleline(&mut self.preset.gf_version);
                        ui.label("Stage");
                        ui.text_edit_singleline(&mut self.preset.stage);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Song creator");
                        ui.text_edit_singleline(&mut self.preset.song_creator);
                        ui.checkbox(&mut self.preset.needs_voices, "Has Voices file?");
                    });
                    ui.horizontal(|ui| {
                        if ui.button("Load preset...").clicked() { self.load_preset(); }
                        if ui.button("Save preset...").clicked() { self.save_preset(); }
                        if ui.button("Reset mapping").clicked() { self.preset = ConversionPreset::default(); }
                    });

                    ui.collapsing("Pitch mapping (JSON preset controls this precisely)", |ui| {
                        ui.label("SNIFF default: 48-51 = BF left/down/up/right; 60-63 = opponent left/down/up/right.");
                        for mapping in &mut self.preset.mapping {
                            ui.horizontal(|ui| {
                                ui.label("Pitch"); ui.add(egui::DragValue::new(&mut mapping.pitch).range(0..=127));
                                ui.selectable_value(&mut mapping.side, converter::Side::Player, "BF");
                                ui.selectable_value(&mut mapping.side, converter::Side::Opponent, "Opponent");
                                ui.label("Direction"); ui.add(egui::DragValue::new(&mut mapping.direction).range(0..=3));
                            });
                        }
                    });

                    ui.collapsing("BPM changes (pitch 56 markers)", |ui| {
                        let detected_count = match &self.inputs {
                            PipelineInputs::Merge(_) => self.merge_scan_total.or_else(|| {
                                self.patterns
                                    .iter()
                                    .find(|p| p.id == self.preset.pattern_id)
                                    .map(|p| p.bpm_change_count)
                            }),
                            _ => self.patterns
                                .iter()
                                .find(|p| p.id == self.preset.pattern_id)
                                .map(|p| p.bpm_change_count),
                        };
                        match detected_count {
                            Some(0) => { ui.label("No BPM changes detected in this pattern."); }
                            Some(n) => {
                                let scan_note = if matches!(self.inputs, PipelineInputs::Merge(_))
                                    && self.merge_scan_total.is_none()
                                {
                                    " (first file only -- press Scan all inputs for full count)"
                                } else {
                                    ""
                                };
                                ui.label(format!("{n} BPM change{} detected.{scan_note}", if n == 1 { "" } else { "s" }));
                            }
                            None => { ui.label("Choose an FLP to detect BPM changes."); }
                        }
                        ui.label("One entry per BPM change, in timeline order. Each value replaces the current base BPM from that section onward (BPM multiplier is applied on top).");
                        let mut to_remove: Option<usize> = None;
                        for (i, bpm) in self.preset.bpm_changes.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                ui.label(format!("Change {}:", i + 1));
                                ui.add(egui::DragValue::new(bpm).speed(0.5).range(1.0..=f64::INFINITY));
                                if ui.small_button("X").clicked() {
                                    to_remove = Some(i);
                                }
                            });
                        }
                        if let Some(i) = to_remove {
                            self.preset.bpm_changes.remove(i);
                        }
                        if ui.button("Add BPM change").clicked() {
                            let default = self.detected_bpm.unwrap_or(120.0);
                            self.preset.bpm_changes.push(default);
                        }
                        if self.preset.bpm_changes.is_empty() {
                            ui.label("(no BPM changes -- leave empty if the chart has no pitch-56 markers)");
                        }
                    });

                    ui.separator();
                    ui.horizontal(|ui| {
                        let mut split_enabled = self.split_threshold.is_some();
                        if ui.checkbox(&mut split_enabled, "Split output by note count").changed() {
                            self.split_threshold = if split_enabled {
                                Some(self.split_threshold.unwrap_or(1_000_000))
                            } else {
                                None
                            };
                        }
                        if let Some(threshold) = self.split_threshold.as_mut() {
                            ui.add(egui::DragValue::new(threshold).speed(100_000).range(1..=u64::MAX));
                            ui.label("notes per file");
                        } else {
                            ui.label("(all sections in one file)");
                        }
                    });

                    ui.separator();
                    if self.converting {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(&self.progress_stage);
                        });
                        if self.progress_total > 0 {
                            let fraction = self.progress_done as f32 / self.progress_total as f32;
                            ui.add(
                                egui::ProgressBar::new(fraction)
                                    .text(format!("{} / {} notes", self.progress_done, self.progress_total)),
                            );
                        } else {
                            ui.add(egui::ProgressBar::new(0.0).animate(true));
                        }
                    }
                    if ui.add_enabled(!self.converting, egui::Button::new("Convert")).clicked() {
                        self.start_convert();
                    }
                    if !self.status.is_empty() { ui.label(&self.status); }
                } // end Direction::ToChart

                Direction::ToScore => {
                    ui.label("Converts a Psych Engine chart JSON back to an FL Studio Score (.fsc). BPM is read from the JSON. Output PPQ is always 96. Uses SNIFF's default pitch layout (48-51 = BF, 60-63 = opponent).");
                    ui.separator();

                    ui.horizontal(|ui| {
                        if ui.add_enabled(!self.converting, egui::Button::new("Choose JSON...")).clicked() {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("Chart JSON", &["json"])
                                .pick_file()
                            {
                                self.score_output = Some(path.with_extension("fsc"));
                                self.score_input = Some(path);
                            }
                        }
                        ui.monospace(self.score_input.as_ref().map_or_else(
                            || "No input selected".to_owned(),
                            |p| p.display().to_string(),
                        ));
                    });

                    ui.horizontal(|ui| {
                        if ui.add_enabled(!self.converting, egui::Button::new("Output FSC...")).clicked() {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("FL Studio Score", &["fsc"])
                                .set_file_name("score.fsc")
                                .save_file()
                            {
                                self.score_output = Some(path);
                            }
                        }
                        ui.monospace(self.score_output.as_ref().map_or_else(
                            || "No output selected".to_owned(),
                            |p| p.display().to_string(),
                        ));
                    });

                    ui.separator();
                    if self.converting {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(&self.progress_stage);
                        });
                        if self.progress_total > 0 {
                            let fraction = self.progress_done as f32 / self.progress_total as f32;
                            ui.add(
                                egui::ProgressBar::new(fraction)
                                    .text(format!("{} / {} sections", self.progress_done, self.progress_total)),
                            );
                        } else {
                            ui.add(egui::ProgressBar::new(0.0).animate(true));
                        }
                    }
                    if ui.add_enabled(!self.converting, egui::Button::new("Convert")).clicked() {
                        self.start_score_convert();
                    }
                    if !self.status.is_empty() { ui.label(&self.status); }
                } // end Direction::ToScore
            } // end match self.direction

            // Persist UI state after every interaction. egui only calls
            // update() on actual input events or repaint requests, so this
            // won't hammer the disk during idle.
            if !self.converting {
                self.save_state();
            }
        });
    }
}