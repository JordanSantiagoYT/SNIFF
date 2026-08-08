# SiIva Note Importer For FNF

A tool that convert charts made with FL Studio (FLP/FSC) to chart .json files usable in [Friday Night Funkin'](https://github.com/FunkinCrew/Funkin).

There are two implementations here, which produce near byte-identical output:

* `SNIFF-rust/`: The faster engine. A Rust port of the original SNIFF that is MUCH faster. Can export a chart with 40 million notes in just 2 seconds, which is over 90 times faster than the original SNIFF, and it can handle significantly larger projects than before.
* `SNIFF.sln`: The original product. Works with C#, and is the basic interpretation of SNIFF.

To compile the Rust product:
1. [Install Rust if you haven't.](https://rust-lang.org/tools/install/)
2. Run `cd SNIFF-rust && cargo build --release`. You can do it like that or do them one at a time. The final product will go in ./target/release.

# How does this fork differ from the OG SNIFF?

This fork adds the optimizations from HRK\_EXEX's fork of SNIFF, along with a few modifications. These modifications include:

* BPM Multiplier (Useful for charting songs with a BPM higher than 522)
* Song Credits (Allows you to put a creator's name who created the song you're charting)

In the Rust port, more optimizations/differences are included to make it faster and more usable:
* Rust-based conversion pipeline.
* Parallel section rendering with rayon.
* Zero-copy streaming JSON output via BufWriter and a custom `Serialize` implementation rather than building the entire thing in memory first.
* Pre-built pitch lookup table, giving O(1) lookup per note.
* Notes are sorted by raw tick position rather than converting to milliseconds first.
* JSON conversion to FSC uses simd-json, which can parse JSON significantly faster than a conventional parser.
* All FLP metadata is collected in one event-loop pass over the file, which is much faster.
* All file writes use a 16MB write buffer, minimizing syscall overhead on large outputs.
* Instead of having to name your patterns `easy`, `normal` and `hard` specifically, you can specifically pick which pattern is converted. (You can use Split Difficulties mode if you prefer the old behavior.)
* The new Merge and Batch modes allow you to convert multiple FLPs at a time, or into one file, provided that they all use the same exact pattern name.
* Pitch mapping can be changed or re-assigned, to make custom FPC channels that utilize different layouts of note pitches.
* Supports command-line arguments, functioning just like the GUI version. (though, using the GUI is more recommended)

NOTES:
- Only FL Studio 20 and 21 are supported! Saving using FL 2025 (or later) might break things (unless using FSCs).
- If SNIFF is missing notes but they're there in the FLP, ensure that your notes are in the right places and your channel is:
- - the top channel in the rack
  - preferably an instance of FPC. if not, make sure there are no FPC channels
  - not using any grouped notes
  - selected when saving the project
  - set up in the first pattern, and that pattern is selected while saving.
  - if after all of these it doesn't work, your FL version might be too new for this program. sadly, the original person that made this stopped working on it, so FL25+ FLPs are not supported.
  - If using the Rust port, make sure there are any actual notes in the pattern.
  - there's also [HaxePixel's Rust port of SNIFF,](https://github.com/HaxePixel/SNIFF-RUSTED) however the JSON to FLP direction doesn't convert charts into readable FLP files (aside from.. HaxePixel's SNIFF.)

- Final Note: Like HaxePixel's Rust port of SNIFF, the Rust port shared here was completely vibecoded.