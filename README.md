# SiIva Note Importer For FNF

Tool to convert charts made in FL Studio to .json files usable in [Friday Night Funkin'](https://github.com/FunkinCrew/Funkin).  
Developed for use internally at SiIvaGunner.

# How does this fork differ from the OG SNIFF?

This fork adds the optimizations from HRK\_EXEX's fork of SNIFF, along with a few modifications. These modifications include:

* BPM Multiplier (Useful for charting songs with a BPM higher than 522)
* Song Credits (Allows you to put a creator's name who created the song you're charting)

NOTES:
- Only FL Studio 20 and 21 are supported! Saving using FL 2025 might break things.
- If SNIFF is missing notes but they're there in the FLP, ensure that your notes are in the right places and your channel is:
- - the top channel in the rack
  - preferably an instance of FPC. if not, make sure there are no FPC channels
  - not using any grouped notes
  - selected when saving the project
  - set up in the first pattern, and that pattern is selected while saving.
  - if after all of these it doesn't work, your FL version might be too new for this program. sadly, the original person that made this stopped working on it, so FL25 FLPs are not supported.
