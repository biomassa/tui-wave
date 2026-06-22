# Manual testing checklist

Re-run before any tag/release. These exercise TUI/audio behavior that automated tests
can't practically cover (real terminal emulators, real audio hardware, human judgment of
"does this feel right").

- [ ] Terminal restores correctly after a normal quit (`q`) and after a forced panic.
- [ ] Resize the terminal (smaller and larger) while the app is running — waveform, menu,
      and toolbar all reflow without corruption or stale click targets.
- [ ] Play audio, then resize the terminal mid-playback — no audio glitch or dropout.
- [ ] Play audio, then hold a zoom/scroll key — no audio stutter.
- [ ] Click each toolbar button and confirm it matches its keyboard shortcut's effect.
- [ ] Open each top-level menu via Alt+letter, navigate with arrows, activate with Enter;
      repeat via mouse click; confirm identical results. Also try the F10 fallback.
- [ ] Test in at least two terminal emulators (e.g. a plain terminal + tmux) — Alt-key and
      mouse reporting vary across terminals/multiplexers.
- [ ] Load a real-world WAV (not just the test fixtures) at a few sample rates (44.1k,
      48k, 96k) and bit depths (16-bit, 24-bit, 32-bit float).
- [ ] Save, then reload the saved file — confirm round-trip fidelity.
- [ ] Test on both Linux and macOS before calling a phase done — cpal/rodio backend
      behavior (ALSA/PulseAudio vs CoreAudio) is the most likely source of platform bugs.
- [ ] Open a very long file (30+ minutes) — confirm scroll/zoom stays responsive and
      memory usage is sane.
