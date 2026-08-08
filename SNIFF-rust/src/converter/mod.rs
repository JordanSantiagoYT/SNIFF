// mod.rs
// Module root for the converter crate.
//
// Submodules:
//   types      — shared public data types (ConversionPreset, Stats, Progress, …)
//   flp        — raw FLP/FSC binary parsing (event reader, note parser, byte helpers)
//   inspect    — lightweight FLP/FSC query API (inspect, find_difficulty_patterns, …)
//   chart      — in-memory chart model and JSON serialization
//   pipeline   — FLP/FSC -> chart JSON orchestration (convert_file)
//   fsc_export — chart JSON -> FSC orchestration (convert_json_to_fsc)

mod types;
mod flp;
mod inspect;
mod chart;
mod pipeline;
mod fsc_export;

// Re-export everything main.rs references directly.
pub use types::{ConversionMode, ConversionPreset, PatternInfo, Progress, Side, Stats};
pub use inspect::{inspect, scan_merge_inputs};
pub use pipeline::convert_file;
pub use fsc_export::convert_json_to_fsc;
