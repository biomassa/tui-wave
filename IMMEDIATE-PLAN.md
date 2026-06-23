# Current errors
- [x] decibel scale is wrong when auto vertical zoom is selected
- [x] auto vertical zoom should work within the current window (at current horizontal zoom level), so it has to auto-vertical-zoom what is currently visible and dynamically adapt the decibel scale so everything stays correct
- [x] selecting with mouse does not work

# TODOs
- [x] optional snapping to zero crossing with any destructive operations
- [x] test multiple undo levels (deep undo + cross-buffer isolation tests in ui::app / model::history)
- [x] BWF import / export (so markers are read and written) — cue/adtl markers round-trip; bext chunk preserved (src/model/bwf.rs)
- [x] on the left should be a panel with the list of wave files in the current directory. It should be searchable with "/" shortcut (standard vim search shortcut). selected file opens in the interface, dirty (edited but unsaved) filenames have color accent and an asterisk mark. there should be a save as... command that works on currently open file and a save all command that works on all dirty (changed but unsaved) files
- [x] if the program is opened without an argument, it opens empty and user selects files from panel on the left
- [x] if the program is launched with a directory as an argument, it shows files from that directory in the panel on the left

# TODOs - operations on audio
- [x] normalize all file / selected range
- [x] gain all file / selected range
- [x] resample (windowed-sinc, whole-file; src/commands/resample.rs — Ctrl+e)
- [x] bit depth converting (with optional dithering) — Save As cycles 16/24/32-bit, Ctrl+D toggles TPDF dither
- [x] reverse
- [x] inserting / moving / deleting markers — m/M insert/delete, [ ] jump, mouse-drag to move, double-click label to rename
