# ============================================================
# tui-wave — Record
#
# NOT part of praatAudioTools. This script is tui-wave's own and lives in this
# repository, not in the third_party/praat-audiotools submodule, so a submodule
# update can neither remove it nor overwrite it.
#
# Why it exists: praatAudioTools records from the microphone in ten places (the
# Vector Chain `Live_*` scripts), always with the same single line, but never on
# its own — the capture is always welded to a processing chain that follows it.
# This exposes just the capture.
#
# The recording line below is upstream's, with the fixed 44100 and gain of 1.0
# turned into arguments. The device name is deliberately still the literal
# "Microphone": that is what Praat calls the system's currently-selected input,
# so the recording follows whatever the OS sound settings point at, which is the
# behaviour the Live_* chains already have. A real device name here would pin
# the capture to one interface and fail outright when it was not plugged in.
#
# Balance stays at 0.5 (centred). It only means anything for a stereo capture,
# and there is no reading of it that belongs in a dialog for recording a take.
# ============================================================

# The bracketed ranges are not decoration. They follow praatAudioTools' own convention
# (`Fold_depth_(0-1)`, `Base_frequency_(Hz)`), and the converter reads them: a bound is only
# ever taken from a name that declares one, because everything else it could do would be
# inventing a range and presenting it as fact. Praat drops the bracketed part when deriving the
# variable, so these are still `duration_seconds` and `input_gain` below.
form Record
    positive Duration_seconds_(0.1-3600) 10.0
    word Sample_rate 44100
    positive Input_gain_(0-1) 1.0
endform

Record Sound (fixed time): "Microphone", input_gain, 0.5, sample_rate$, duration_seconds

# The driver saves whatever Sound object is left selected, so nothing else to do:
# `Record Sound (fixed time)` leaves the new recording selected.
Rename: "Recording"
