// cli.rs
// Command-line interface for SNIFF.
//
// Direction is auto-detected from the first input file's extension:
//   .json        -> ToScore (Chart JSON -> FSC)
//   .flp / .fsc  -> ToChart (FLP/FSC -> Chart JSON)
//
// Mode defaults to Single when one input is given, Merge when multiple
// inputs are given and --mode is not specified. --mode always wins if
// provided explicitly.
//
// Pattern resolution: --pattern does a case-insensitive exact match against
// the FLP's pattern names. If no match is found, an error is printed and
// the process exits.

use std::path::PathBuf;
use std::process;

use clap::Parser;

use crate::converter::{
    convert_file, convert_json_to_fsc, inspect, ConversionMode, ConversionPreset, Progress,
};

// ---------------------------------------------------------------------------
// Argument definition
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(
    name = "sniff",
    about = "SNIFF — FL Studio project/score to Friday Night Funkin' chart converter",
    long_about = None,
)]
pub struct Cli {
    /// Input file(s). FLP/FSC -> Chart JSON, or a single JSON -> FSC.
    /// Direction is auto-detected from the first file's extension.
    #[arg(required = true)]
    pub inputs: Vec<PathBuf>,

    /// Output file (Chart JSON or FSC) or output directory (batch mode).
    #[arg(short = 'o', long)]
    pub output: PathBuf,

    /// Pipeline mode. Defaults to 'single' for one input, 'merge' for multiple.
    /// Accepted values: single, merge, split, batch.
    /// Ignored when converting JSON -> FSC (always single input).
    #[arg(long, value_name = "MODE")]
    pub mode: Option<String>,

    /// Pattern name to use (case-insensitive exact match). FLP/FSC only.
    /// Defaults to the first pattern in the file if omitted.
    #[arg(long, value_name = "NAME")]
    pub pattern: Option<String>,

    /// Song name embedded in the output JSON.
    #[arg(long, value_name = "NAME")]
    pub song: Option<String>,

    /// Override the starting BPM instead of reading it from the FLP.
    /// Required when the input is an FSC (which has no tempo event).
    #[arg(long, value_name = "BPM")]
    pub bpm: Option<f64>,

    /// BPM multiplier applied on top of the base BPM. Default: 1.0.
    #[arg(long, value_name = "MULT")]
    pub mult: Option<f64>,

    /// Player 1 (BF) character name. Default: bf.
    #[arg(long, value_name = "NAME")]
    pub p1: Option<String>,

    /// Player 2 (opponent) character name. Default: dad.
    #[arg(long, value_name = "NAME")]
    pub p2: Option<String>,

    /// Girlfriend character name. Default: gf.
    #[arg(long, value_name = "NAME")]
    pub gf: Option<String>,

    /// Stage name. Default: stage.
    #[arg(long, value_name = "NAME")]
    pub stage: Option<String>,

    /// Song creator / credit string embedded in the output JSON.
    #[arg(long, value_name = "STRING")]
    pub credit: Option<String>,

    /// Include a voices track (needsVoices: true). Default: false.
    #[arg(long, default_value_t = false)]
    pub voices: bool,

    /// Trim zero-length sustains from output (omit the sustain field entirely).
    /// Default: false.
    #[arg(long, default_value_t = false)]
    pub trimsus: bool,

    /// Write indented (pretty-printed) JSON instead of compact JSON.
    /// Default: false.
    #[arg(long, default_value_t = false)]
    pub prettyprint: bool,

    /// Split output into multiple files when note count exceeds this threshold.
    /// Omit to write everything to a single file.
    #[arg(long, value_name = "NOTES")]
    pub split: Option<u64>,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Runs the CLI pipeline. Prints a result line to stdout on success, or an
/// error to stderr and exits with code 1 on failure.
pub fn run_cli() {
    let cli = Cli::parse();

    if let Err(e) = run(&cli) {
        eprintln!("error: {e:#}");
        process::exit(1);
    }
}

fn run(cli: &Cli) -> anyhow::Result<()> {
    let first = cli.inputs.first().expect("clap ensures at least one input");

    // Auto-detect direction from the first input's extension.
    let is_to_score = first
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("json"))
        .unwrap_or(false);

    if is_to_score {
        run_to_score(cli, first)
    } else {
        run_to_chart(cli)
    }
}

// ---------------------------------------------------------------------------
// ToScore: JSON -> FSC
// ---------------------------------------------------------------------------

fn run_to_score(cli: &Cli, input: &PathBuf) -> anyhow::Result<()> {
    if cli.inputs.len() > 1 {
        anyhow::bail!("JSON -> FSC conversion only accepts a single input file");
    }

    let mut last_stage = String::new();
    let stats = convert_json_to_fsc(input, &cli.output, &mut |p| {
        if let Progress::Stage(s) = p {
            last_stage = s.to_owned();
        }
    })?;

    println!(
        "Wrote {} notes in {} sections to {} in {:.1} ms",
        stats.notes,
        stats.sections,
        cli.output.display(),
        stats.time,
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// ToChart: FLP/FSC -> Chart JSON
// ---------------------------------------------------------------------------

fn run_to_chart(cli: &Cli) -> anyhow::Result<()> {
    // Build the preset from defaults, then overlay CLI flags.
    let mut preset = ConversionPreset::default();

    if let Some(s) = &cli.song    { preset.song_name   = s.clone(); }
    if let Some(s) = &cli.p1      { preset.player1     = s.clone(); }
    if let Some(s) = &cli.p2      { preset.player2     = s.clone(); }
    if let Some(s) = &cli.gf      { preset.gf_version  = s.clone(); }
    if let Some(s) = &cli.stage   { preset.stage       = s.clone(); }
    if let Some(s) = &cli.credit  { preset.song_creator = s.clone(); }
    if let Some(b) = cli.bpm      { preset.base_bpm_override = Some(b); }
    if let Some(m) = cli.mult     { preset.bpm_multiplier    = m; }
    if cli.voices     { preset.needs_voices  = true; }
    if cli.trimsus    { preset.trim_sustains = true; }
    if cli.prettyprint { preset.pretty_json  = true; }

    // Resolve pipeline mode.
    let mode = resolve_mode(cli)?;

    // Resolve pattern ID from name if --pattern was given.
    let first = &cli.inputs[0];
    if let Some(ref name) = cli.pattern {
        let info = inspect(first)?;
        let lower = name.to_lowercase();
        let pattern = info
            .patterns
            .iter()
            .find(|p| p.name.to_lowercase() == lower)
            .ok_or_else(|| {
                // List available names to help the user.
                let available = info
                    .patterns
                    .iter()
                    .map(|p| format!("  \"{}\"", p.name))
                    .collect::<Vec<_>>()
                    .join("\n");
                anyhow::anyhow!(
                    "no pattern named {:?} found in {}\nAvailable patterns:\n{}",
                    name,
                    first.display(),
                    available,
                )
            })?;
        preset.pattern_id = pattern.id;
    }

    // Split primary from extras.
    let (primary, extra) = cli.inputs.split_first().expect("at least one input");
    let extra: Vec<PathBuf> = extra.to_vec();

    let mut last_stage = String::new();
    let stats = convert_file(
        primary,
        &extra,
        &cli.output,
        &preset,
        &mode,
        cli.split,
        |p| {
            if let Progress::Stage(s) = p {
                last_stage = s.to_owned();
            }
        },
    )?;

    let file_suffix = if stats.files > 1 {
        format!(" across {} files", stats.files)
    } else {
        String::new()
    };
    println!(
        "Wrote {} notes in {} sections to {}{} in {:.1} ms{}",
        stats.notes,
        stats.sections,
        cli.output.display(),
        file_suffix,
        stats.time,
        stats.warnings,
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Mode resolution
// ---------------------------------------------------------------------------

fn resolve_mode(cli: &Cli) -> anyhow::Result<ConversionMode> {
    match cli.mode.as_deref() {
        Some(m) => match m.to_lowercase().as_str() {
            "single" => Ok(ConversionMode::Single),
            "merge"  => Ok(ConversionMode::Merge),
            "split"  => Ok(ConversionMode::SplitDifficulties),
            "batch"  => Ok(ConversionMode::Batch),
            other    => anyhow::bail!(
                "unknown mode {:?} — accepted values: single, merge, split, batch",
                other
            ),
        },
        // Default: single for one input, merge for multiple.
        None => {
            if cli.inputs.len() > 1 {
                Ok(ConversionMode::Merge)
            } else {
                Ok(ConversionMode::Single)
            }
        }
    }
}
