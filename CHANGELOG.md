# Changelog

## 2026-07-31 (1.9.1)

- **Fixed: large WAVs written by Max/MSP opened as a fraction of their length.** A 14GB,
  58-channel, 96kHz take showed as 1m34s of an 11m12s recording. Their `RF64` headers are wrong
  in two ways at once: the `data` chunk's 32-bit size field carries the low 32 bits of the real
  size instead of the marker that means "the real size is elsewhere", and the header contains a
  duplicate, so the audio actually starts 24 bytes later than the first `data` chunk says.
  Correcting only the size would have been worse than the bug — 24 bytes is not a whole number
  of frames, so every channel would have been read into its neighbour's place. Both are now
  derived from the file's own length and verified before being used, so correctly-written files
  are unaffected.
- **Fixed: switching back to a large streamed buffer froze the app.** With two buffers open, going
  to another one and back left the streamed buffer using the *other* buffer's waveform overview.
  Finding nothing usable there, the renderer fell back to reading the audio itself — every column,
  every visible channel, straight off disk, which on a 14GB file is hundreds of gigabytes for a
  single frame. It looked like a hang with no CPU use because it was waiting on the disk. Each
  buffer now keeps its own overview, so switching is instant and correct either way.
- **Fixed: the hint panel changed height and the whole layout jumped.** It now always reserves the
  height of its tallest state, so nothing below it moves — whichever panel has focus, and whether
  or not the buffer is streamed.
- **A streamed buffer can now be saved.** The workflow that makes Remove Empty Channels worth
  having on a huge file: open a 14GB 58-channel take, drop the channels that hold nothing, and
  Save As writes just the ones you kept — as a proper multichannel WAV, `RF64` if it needs to be,
  at whatever bit depth you choose. One pass over the source, nothing held in memory, progress
  shown and Esc stops it. Saving over the file being read is refused rather than attempted.
  Afterwards the buffer becomes the file it wrote, which for a trimmed take is often small enough
  to open fully editable again. A real example: 14.0GB and 58 channels in, 0.96GB and 4 channels
  out, same 11m12s length.
- **A streamed buffer's toolbar now shows what actually works on it** — Save As, Remove Empty
  Channels and Export Channels, clickable — instead of a panel of commands that only produce
  refusals. Menu entries that a streamed buffer would refuse are greyed out, driven off the same
  rule that does the refusing, so the menu cannot say one thing and the app do another.
- **Files up to 4GB now open fully editable, rather than 1.5GB** (`max_resident_mb`). That is
  about three minutes of 58-channel 96kHz audio, or six at 48kHz. Anything larger still opens
  read-only and streamed from disk. Note that an editable buffer costs roughly twice its size in
  memory once playback is running.
- Bumped version to 1.9.1.

## 2026-07-30 (1.9.0)

Multichannel support, other audio formats, and files too large to hold in memory.

- **Files over 4GB open at all.** A plain WAV cannot exceed 4GB — its size field is 32-bit — so
  recorders switch to the `RF64` form once a take passes that, and the reader this app used could
  not read one: it rejected any file not starting with the literal bytes `RIFF`. A 20GB take
  therefore failed on its first four bytes, in milliseconds, with the error thrown away — so
  pressing Enter on it looked exactly like the keypress never registering. Both halves are fixed:
  `RF64` and `BW64` now load, and a load that fails says why instead of doing nothing visible.
- **A file too large for memory opens read-only, streamed from disk.** Samples are held as 32-bit
  float whatever the source depth, so a 30GB recording needs 30GB of memory — plus another copy
  for playback. Above a configurable budget (`max_resident_mb`, 1.5GB by default) the audio stays
  on disk and only the waveform overview is held, which is about a thirtieth of the size: a 30GB
  56-channel file opens in under a minute using ~1.1GB. Such a buffer is marked
  `[streamed, read-only]` in the title, and displaying, scrolling, zooming, Remove Empty Channels
  and Export Channels all work on it. Editing, saving and playback are refused with a message
  naming the action and the buffer's size, rather than appearing to work and doing nothing.
  Anything that fitted before still opens fully editable, unchanged.
- **Remove Empty Channels and Export Channels work at that scale.** Remove Empty Channels needs no
  extra reading at all — the peaks it compares are already measured while the overview is built —
  and it is undoable with Ctrl+Z like anything else: the audio never moves, so removing channels
  is a change to which channels the file *presents* and undo simply puts the old list back — no
  copy of a 30GB file is involved. Export Channels reads the source exactly once no matter how many
  files it writes, and writes `RF64` itself if an output would exceed 4GB.
- **Waveform drawing got roughly 100x cheaper at wide zoom levels**, on ordinary files as much as
  large ones. Each column's min/max was read from the coarsest cached resolution that fitted,
  which minimized one cost and maximized a much larger one; picking the cheapest resolution
  instead gives the identical answer for a small fraction of the work.
- **Fixed: opening a file no longer reads it twice.** Finding a file's markers meant loading the
  entire file into memory a second time, after the audio had already been decoded, purely to
  locate a few bytes of metadata near the end of it.
- **The waveform shows six channel panes at a time, scrolled with the mouse wheel.** The pane
  layout divided the whole waveform area by the channel count, so a 30-channel file got one row
  per channel — no centre row for the zero line, room for a single dB gutter mark — and past
  about 42 channels the split collapsed to zero-height panes. The area now shows a window of six,
  moved with the wheel over the waveform or `,`/`.` (one pane) and `<`/`>` (a whole window), with
  the visible range in the title and a scrollbar on the right edge. At six channels or fewer
  nothing changes: same split, no indicator, wheel inert.
- **Process ▸ Remove Empty Channels.** Measures each channel's peak across the whole file and
  drops every channel below a threshold, default -48 dBFS, so a 30-channel capture with real
  audio on four becomes a 4-channel buffer. Peak rather than RMS, so a channel that is silent
  apart from one short event is kept. Undoable — Ctrl+Z puts the channels back.
- **File ▸ Export Channels.** Splits a multichannel buffer into per-channel WAVs: each channel
  is Mono, Skip, or paired with the channel below it, written into a subfolder as
  `<stem>_chN.wav` / `<stem>_chN-M.wav`. Channel numbers are zero-padded to the source's channel
  count, so a 30-channel export sorts in channel order rather than ch1, ch10, ch11, … ch2.
  Always WAV at the source's own rate and depth. Until now only channels 1 and 2 were reachable
  as separate audio, via New from Left/Right Channel.
- **FLAC and AIFF files open like WAVs.** The Files panel lists `.wav`, `.flac`, `.aif` and
  `.aiff` intermixed, and all four load, play and audition. WAV still goes through its own
  reader, so BWF markers and `bext` survive; the `.headstails` sidecar works for every format.
  MP3 is export-only and is not listed.
- **File ▸ Export writes FLAC or MP3.** Mono and stereo only — a multichannel buffer is blocked
  with a pointer at Export Channels, and MP3 blocks a sample rate it cannot store (96 kHz among
  them) naming Resample, rather than failing after you commit. MP3 is CBR with a
  128/160/192/256/320 kbps picker. Save and Save As are unchanged and still own the WAV working
  file.
- **Fixed: quick Save on a buffer loaded from a FLAC or AIFF would have written WAV bytes over
  the source** under a misleading extension. Save and the Buffers panel now redirect to Save As
  prefilled with a `.wav` name; Save All skips such buffers and leaves them dirty rather than
  interrupting the batch with a modal.
- **Fixed: Save As showed the last-used bit depth rather than the document's own**, and prefilled
  the whole file name — so a buffer opened from `beta.flac` offered `beta.flac.wav`. Only the
  queued Save As path had been setting the depth.
- **Fixed: scrolling the channel window in graphics mode crashed the app.** Any file with more
  channels than fit on screen, on one fast scroll of the wheel or a held key — the per-channel
  image state was addressed by channel number but only ever appended to, so a jump bigger than
  the number of visible panes ran off the end of it. Scrolling one pane at a time never showed
  it, which is why it survived this long.
- **The waveform fills a tall terminal.** Six channel panes were drawn however much height
  there was, and since each pane needs an odd number of rows to put amplitude zero on a real
  centre row, up to eleven rows were left empty below the last one — a conspicuous band on a
  high-resolution screen. The pane count now grows with the available height, so a taller
  window shows more of a 58-channel file instead of more emptiness. Nothing changes at the
  heights that were already full.
- **Waveform drawing on a streamed file is no longer sluggish.** Scrolling or zooming a 30GB
  file was reading half a gigabyte off disk *per redraw*: a disk read carries every channel
  whether or not it is wanted, each channel was caching its own copy of the same bytes, and the
  fine correction applied at each column's edges stopped sharing that cache as you zoomed out.
  A redraw now reads one window at most when the view moves, and nothing at all once it settles,
  while still drawing exactly what a fully-loaded file draws.
- **Every dialog says how to commit and how to cancel.** Eight prompts — Normalize, Resample,
  Remove Empty Channels, the three renames, Open Directory and Save Curve — showed a bare input
  box with no `Enter`/`Esc` hint, which also meant they had no clickable submit row. The hint is
  set off by a blank line, and its keys are peach like everywhere else.
- **A long load can be stopped.** Building the waveform overview for a very large file takes
  the best part of a minute and was uninterruptible; Esc now abandons it and closes the buffer.
  The panel's own text was also being cut off mid-sentence.
- **Menu entries that ask for something before acting now end in `...`**, per the usual
  convention, and toolbar buttons with no keyboard shortcut to show — which rendered as a bare
  label with nothing to press — were removed; they remain in the menus.
- **Fixed: File ▸ Export resized and jumped** as you cycled the format, because its height
  depended on whether a blocker message was showing. Its labels were also each starting in a
  different column.
- Bumped version to 1.9.0.

## 2026-07-29 (1.8.0)

- **A zero line across every channel.** The waveform had no visual reference for where
  amplitude zero is, so a zero crossing's position had to be inferred from the trace itself.
  It is a background element by construction — the waveform always draws on top of it, and
  where the two land in the same character cell the waveform wins outright, digital silence
  included (silent stretches now draw the trace's own flat line at zero rather than nothing).
- **An optional time ruler.** A m:ss row between the waveform and the status bar, toggled from
  the View menu. Its tick interval steps through round values as you zoom — minutes, seconds,
  milliseconds — and the ticks sit on round absolute times, so scrolling slides the ruler
  rather than renumbering it.
- **Fixed: the two channels of a stereo file rendered at different vertical scales**, with
  different dB gutter marks beside each. The undividable leftover row was going to one pane,
  and every amplitude-to-row mapping is derived from the pane's height. Every channel now gets
  the same height, and an odd one, which is also what puts amplitude zero on a real centre row
  instead of between two rows. The rows that leaves over sit below the last channel, so the
  panes themselves stay flush against each other.
- **Fixed: zero-crossing snap did nothing at all on stereo.** It required every channel to
  cross zero at the same sample index and agree on rounding, which two channels even three
  samples out of phase never satisfy — so the boundary was returned untouched. It now snaps to
  the nearest point where all channels are simultaneously quietest, and its search window is a
  span of time rather than a fixed sample count, so it reaches as far into a 96kHz file as into
  a 44.1kHz one.
- **Fixed: Convolution Reverb ignored every setting and clipped.** `fastconv` parses its flags
  before the filenames, unlike every other CDP binary; with them trailing it silently discarded
  the amplitude scale, the float-output flag and the dry/wet mix all at once, so every
  configuration produced a byte-identical, clipped result. Losing float output is also what made
  the clipping unrecoverable.
- **Fixed: Remove DC Offset invented an offset on files that had none.** It measured the mean,
  which on material with short positive lobes and long deep negative ones is dominated by the
  waveform's own asymmetry rather than by any offset — so "removing" it lifted the whole file,
  silence included, off the zero line. It now measures the level the signal actually sits at.
- **Remove DC Offset can process each channel separately.** CDP shifts the whole file by a
  single value, so a stereo file whose channels are offset in different directions had no value
  that corrected both; with the option on, each channel is measured and corrected on its own.
- **Fixed: Waveset Thin left one channel of a stereo file ending in silence.** CDP runs that
  family mono-only, and its output length depends on the audio, so the two channels came back
  different lengths and the shorter was padded. The padding is still the right answer — it keeps
  every sample CDP produced — but it is now named in the status bar instead of appearing as a
  channel that looks broken.
- **Fixed: markers and head/tail marks were cleared by processes that change length slightly.**
  Reported for Waveset Repeat In Place, which returns 2-4% longer despite promising no time
  stretching. Marks now move with the change instead: a mark a given fraction of the way through
  the processed range stays that fraction of the way through the result.
- **Scramble's per-segment modes take their cut times from head/tail marks.** The eight modes
  that split the sound into separately-processed segments needed those times typed into a
  dialog field; they now come from the marks you place on the waveform with `h`. Both these and
  the DISTMORE family now say what they are missing the moment the dialog opens, with Preview
  and Apply dimmed, instead of only when Apply appears to do nothing.
- **Fixed: Step-Freeze Spectrum rejected its own default.** Its time step is bounded by the
  analysis window at one end and the selection's duration at the other, and the catalog declared
  a fixed range that honoured neither.
- **27 CDP processes had development notes where their descriptions should be**, so the browser
  showed argument shapes and internal cross-references instead of any account of what the
  process does. All 27 now carry real descriptions taken from CDP's own documentation.
- Bumped version to 1.8.0.

## 2026-07-28 (1.7.1)

- **CDP processes are now grouped the way CDP groups them.** The browser's group list followed
  a taxonomy of our own (`distort`, `texture`, `filter`, …); it now uses CDP's own headings —
  DISTORT, BLUR, FOCUS, MORPH, REPITCH and the rest — taken straight from its two index pages.
  Anything you read about CDP in its documentation, manuals or GUI now names the same group you
  see here. The old scheme also had a `texture` bucket holding 82 of 407 processes next to
  groups holding one, which was not much of a taxonomy.
- **The browser has a Domain column.** Choosing All, Recent, Time-domain or Spectral on the left
  fills the Groups column beside it, so picking a process is two narrow choices instead of one
  long list. Each domain offers "All" first, so you can browse a whole domain without picking a
  group. Tab and the arrow keys move across all the columns and skip the Groups column when
  there is nothing in it.
- Bumped version to 1.7.1.

## 2026-07-28 (1.7.0)

- **24 more CDP distortion processes**, completing the coverage of CDP's own `cdistort`
  documentation page. New: Waveset Repeat In Place (`distort repeat2`), Waveset Repeat Below
  Frequency (`replim`), two more Waveset Thin modes (`delete 1`/`3`), four more Waveset Reform
  shapes (fixed square, fixed triangle, half-cycle invert, contour exaggerate), all three
  Impulse Train modes (`pulsed`), Quirk Power Factor over the whole signal, and twelve more
  Scramble reorderings — by size, by level, and segment-wise versions of both, where only two
  were available before. Every entry was verified by running the real binary rather than
  reading its usage text, which is how several ranges were corrected before shipping.
- **Fixed: many CDP processes clipped their output.** Reported for Accumulate and Convolution
  Reverb, but a sweep of the whole catalog found 46 processes affected, and the cause was
  shared: CDP's own spectral resynthesis stage clamps at full scale, destroying the peaks
  before the result ever reaches the editor. Those processes now run with headroom reserved
  and the level restored exactly afterwards — bit-for-bit, so anything that never needed the
  headroom is untouched — and only if the result genuinely still exceeds full scale is it
  brought down, with the reduction named in the status bar instead of changing level silently.
  Convolution Reverb additionally writes floating-point output now, so its peaks survive at
  all.
- **Spectral Bridge (Frozen Points) works.** It has never been runnable — it needs a
  window-grabbing pre-pass that was never built, and refused to start with "not supported
  yet". The pre-pass now runs automatically as part of the process.
- **Removed the broken "Grab Static Spectrum Window" process.** It returned an empty buffer
  every single time, by construction. Spectral Freeze is the working version of the same idea.
- **Fixed: the envelope editor drew two curves at once** in terminals with graphics support —
  the real curve and a stale text-mode one about a row below it. The y-axis labels could also
  push a tick mark into the plot area.
- Fixed the Waveset Band-Filter (Between) defaults, which described an impossible band and so
  discarded the entire sound whatever it was given.
- Bumped version to 1.7.0.

## 2026-07-27

- **New Head/Tail mark system for the CDP DISTMORE family.** A second, separate set of marks
  from ordinary cue markers: `h` inserts one at the cursor, `H` deletes the nearest, and they
  can be dragged with the mouse. They are flat and alternating (Head, Tail, Head, Tail — the
  first is always a Head), which is CDP's own convention, so both the `H1`/`T1`/`H2`/`T2`
  labels and each mark's role fall out of its position: inserting one in the middle renumbers
  everything after it. Drawn in orange with a dashed line, one label row below the ordinary
  markers, and counted in the status bar as pairs. They shift with every edit and undo exactly
  like ordinary markers, and persist to a `<name>.headstails` file beside the audio in CDP's
  own marklist format, so the file is directly usable by `distmore` and readable by hand.
  All thirteen DISTMORE processes now take their marks from the waveform instead of a
  hand-typed list of times, which was unusable for something that describes positions in the
  sound you are looking at.
- **Picked input buffers now persist in CDP presets** and in "Recall last process". Buffers
  are matched by file path first and display name second, so same-named files from different
  folders resolve correctly and never-saved buffers resolve at all. If any buffer a preset
  names is no longer open, the picker resets rather than restoring a partial pick — order is
  what these processes read as structure, so a partial restore would silently change what the
  preset does.
- **Dialogs are now mouse-aware throughout.** Clicking inside a text field puts the caret on
  the character you clicked; clicking a CDP parameter row focuses it, and clicking a row whose
  editor is a separate overlay opens it (the input-buffers picker, and the list, table,
  marker-time, hilite-band, formant and file editors); clicking a buffer in the picker toggles
  it into the pick; chain-editor rows are selectable by click; and the wheel scrolls every
  dialog that has a list. Eleven dialogs previously had no click handling at all.
- Renamed the CDP "input files" row to **"input buffers"** — everything it offers is an
  already-open document, never a file on disk — and stopped it overflowing the dialog.
- **Fixed: undo after a CDP process lost Head/Tail marks.** A length-preserving process (which
  every DISTMORE process is) now also leaves them exactly where they were, so one run no
  longer consumes the marks the next one needs.
- **Fixed: Housekeep "Remove DC Offset" failed every single time.** Its Offset defaulted to
  zero, which CDP rejects outright. It now pre-fills with the negative of the selection's
  measured DC offset — the value that actually removes it — and its range matches CDP's real
  limits instead of a much narrower guess.
- **Fixed: Tesselate failed with more than one source.** Its Sources table needs one row per
  input file and never grew with the buffer pick, so it could not do the thing it exists for.
  The table now tracks the pick, keeping values already typed and staggering the entry delays
  CDP requires to differ.
- **Fixed: Distmore Zigzag Whole File's Output Duration range was wrong.** CDP accepts 2x to
  64x the input's own duration; the fixed range shown put the default below the floor for any
  selection longer than a second. The range and default now scale with the selection.
- **Fixed: a DISTMORE process is no longer defeated by a stray selection.** A selection too
  small to hold two Head/Tail pairs is almost always an accidental drag, so those processes
  widen back to the whole file instead of refusing. Every other process still honours a small
  selection exactly as before.
- Long errors in the CDP parameters dialog now wrap instead of being clipped at the right
  edge, which had been hiding the half of the message that explained the cause.
- **Removed two processes that cannot work here**: Ts Oscillator (it takes no input sound at
  all) and Speculate (its numbered outputs are spectral analysis files, not audio, and a
  single run wrote over a gigabyte across 84 files, freezing the editor).
- **Fixed: the waveform could end up crushed into the left edge with empty space beside it**
  after a process (or an undo of one) shortened the sound. The horizontal zoom was left
  scaled for the old, longer file; it is now re-clamped whenever the length changes, so the
  view never spans more audio than the file actually holds.
- Bumped version to 1.6.0.

## 2026-07-25

- CDP Process and CDP Chain preset rows gain the same "(none)" cycle slot the envelope
  editor's preset row got: Left/Right cycling all the way past the last saved preset (or
  back past the first) now wraps around to a "(none)" slot holding whatever values/steps
  were set by hand before cycling started, instead of that state being lost the moment
  preset browsing begins. Saving a new preset before ever cycling also snapshots it, so the
  hand-set state survives either way.
- Bumped version to 1.5.1.

## 2026-07-24

- **Breakpoint envelope presets now use the same interaction as CDP Process/Chain
  presets** instead of their own separate full-screen picker dialog: the envelope editor's
  "Preset" row shows the current preset (or "(none)") and a saved count, `Tab`/`Shift+Tab`
  cycle through every saved preset and load each one immediately (rescaled to fit the
  current field), `s` opens an inline save prompt prefilled with the current preset's name,
  and `d` deletes the currently-cycled-to preset immediately — no separate picker dialog to
  open first. Cycling all the way around includes an extra "(none)" slot holding whatever
  shape was hand-drawn before the first `Tab` press, so that shape is never lost just from
  browsing presets.
- Fixed a rounding-noise bug where inserting a breakpoint (mouse double-click, or the `n`
  midpoint-insert key) could land on a raw binary-fraction value like `20.079999999999984`
  instead of a clean one.

## 2026-07-23

- **CDP chains automatically merge adjacent spectral (PVOC) steps into one shared
  anal/synth pass** instead of wrapping each one individually — same result, far less work
  for a chain of 2+ consecutive spectral processes. The chain editor's own step list (and
  the Process browser) now show a pale `[pvoc]` badge on every spectral process, and the
  chain editor brackets a merged run with "PVOC Analyze"/"PVOC Resynthesize" marker rows
  (never selectable — the cursor structurally can't land on them).
- **New: system-wide breakpoint-envelope presets.** Any automatable field's envelope editor
  (`e`) can save (`s`) and load (`l`) a named shape, reusable across any process/param, not
  scoped to the one it was drawn on — loading rescales the shape's own timing to fit the
  current field's time span while leaving its shape unchanged. `d` in the picker deletes a
  saved preset.
- **New: "Recall last process" (`Ctrl+L`) in the CDP Process browser**, mirroring the CDP
  Chain editor's existing "recall last chain" (`l`) — every successfully *applied* (not
  previewed) process auto-saves its parameters, so reopening the browser and pressing
  `Ctrl+L` reopens the params dialog on that same process with your last values intact.
- Fixed a rounding-noise display bug (envelope values showing e.g. `73.35999999999999`
  after a nudge) and a marker-overlay regression where a point near the top of its range
  drew on top of the envelope editor's own header text. Hint lines in the envelope editor
  and CDP Process browser now wrap properly on narrow terminals instead of clipping or
  permanently reserving extra vertical space. Fixed the CDP Chain "Save chain as" prompt
  coloring its whole hint line orange instead of just the key. Fixed stray vertical divider
  fragments poking into the blank row above the CDP Process browser's hints bar.
- Bumped version to 1.5.0, covering the CDP chain PVOC auto-merge/badges, envelope
  presets, recall-last-process, and the fixes above.

## 2026-07-22

- **CDP process titles renamed to a CDP-WASM-SUITE-style, plain-English convention**
  (`catalog_titles.toml`, a title-only override layer so the generated `catalog.toml` never
  needs hand-editing) — every renamed title still reveals its own CDP binary. Fixed 4 real
  catalog-key collisions found along the way (two different processes silently sharing one
  key, merge-by-key semantics shadowing one of each pair). Added 27 new catalog entries for
  confirmed CDP-WASM-SUITE gaps (filter fixed/variable, 8 more distort modes, clip mode 1,
  grain align, specfold fold/invert, glisten, chirikov, packet), verified against the real
  CDP8 binaries. Corrected stale source comments citing "CDP 7.1" — the bundled binaries'
  own usage banner is misleading; this is actually CDP Release 8
  (github.com/ComposersDesktop/CDP8).
- **New "CDP Chain..." (Ctrl+H): multi-step CDP pipelines with unlimited-depth
  side-chains.** A linear list of CDP processes runs as one pipeline; any dual-input step
  can be fed by its own side-chain (a sub-chain run against a separately-picked buffer)
  instead of a raw open buffer, nested to any depth. Reuses the existing Browser/Params flow
  to add/edit each step; a stack-based execution engine walks the chain as a post-order tree
  and splices the final result as a single undo step. Real audio preview works both for the
  whole chain and mid-edit on one in-progress step (upstream steps run for real, plus the
  step's current values). Chains save/load as named presets, track recent use, and
  auto-save the last successfully-run chain to its own recall slot (`l` in the chain editor)
  so an unsaved but carefully built chain is never lost after a bare Run. `p` previews the
  chain up to and including just the selected step.
- Fixed: a multi-step chain's marker-preservation tolerance was derived from only its last
  step's category, as if the whole chain were one CDP process — but each spectral step
  re-analyzes the previous step's already-padded output from scratch, so the drift compounds
  down the chain. A chain of just 3 ordinary spectral steps (nothing "time-altering") could
  drift far enough to silently collapse every cue marker in range. Tolerance is now the sum
  of every top-level step's own tolerance, matching the real compounding drift.
- Added `Ctrl+L` ("Reload from disk") to the Buffers panel: re-reads the active document's
  file wholesale (samples, cue markers, bext, bit depth) and clears its undo history — the
  old stack's commands store sample data from before the reload, so replaying them would
  corrupt it. Confirms first on a dirty buffer, reloads immediately if already clean,
  no-ops on a never-saved buffer. New matching toolbar button.
- Bumped version to 1.4.0, covering the CDP-WASM-SUITE renames/gaps, the CDP Chain feature
  (plus its recall-last-chain and per-step-preview follow-ups), the chain marker-tolerance
  fix, and the Buffer-panel Reload-from-disk shortcut above.

## 2026-07-21

- **Dot-matrix waveform renderer replaces the eighth-block bars entirely**, in both the
  character-glyph and graphics-mode (kitty/Sixel/iTerm2) renderers. Each terminal column
  splits into a left/right sub-column with its own min/max (2x horizontal resolution) and
  each row into 4 braille dot-rows (4x vertical resolution), giving the waveform a textured,
  btop-style look instead of solid bars. Colored by a green → yellow (-6dB) → red (0dB)
  amplitude gradient (`theme::gradient_color`, graded by dB via a new `dsp::linear_to_db`,
  not raw linear position — most of a waveform's on-screen height is quiet in linear terms,
  so a linear-position gradient was nearly invisible). The gradient is a toggle ("Gradient"
  in the View menu); off, the waveform draws flat green instead. A selection now shows as a
  dimmed green background with flat black dots, no gradient.
- **View menu toggles now show a checkmark when active** (Zero-Crossing Snap, Fine Step
  Mode, Auto Vertical Zoom, Insertion Point/Viewport Follows Playback, Graphics Mode,
  Gradient), consistent with the toolbar's own active-state highlighting. Checkmarks and
  shortcuts render in fixed-width columns (measured in characters, not UTF-8 bytes — `✓` is
  one display column but three bytes) so they line up vertically instead of drifting with
  each label's length.
- Fixed: pressing Space to play a selection continued playing past the selection's end once
  loop playback was toggled off, instead of stopping there. `AudioEngine` gained
  `play_bounded`/`seek_bounded` (play/seek once, no wraparound, but still stop at an end
  frame — `DocumentSource` already supported this via `loop_end` with no `loop_start`, it
  just wasn't exposed). `App::playback_bound` now distinguishes looped (loop playback on),
  bounded (a selection with loop playback off), and unbounded playback, and falls back to
  the selection's start when the cursor sits at its far edge (the common case after a
  left-to-right drag) so the whole selection plays instead of nothing.
- Bumped version to 1.3.0, covering the dot-matrix waveform renderer (text and graphics
  mode), the amplitude gradient and its toggle, View menu checkmarks, and the
  selection-playback bound fix above.
- **CDP UI cleanup pass** (user report):
  - The CDP Process browser's Groups column no longer lists "pitch curve" — every process
    tagged with that subcategory is curve-in/curve-out and was already unconditionally
    excluded from this browser (`is_curve_only_process`), so the group could only ever show
    "No matches". `App::cdp_groups` now filters subcategories through the same eligibility
    check `cdp_filter_entries` applies, so a listed group is always populate-able.
    (`psow`'s pitch-subcategory processes, e.g. "Psow Reinforce Harmonics", were never
    actually excluded from the real "pitch" group — they just sit near the end of its long,
    catalog-order list.)
  - Buffers-panel row tags shortened: `[Curve]` → `[p]`, `[Formant]` → `[f]`, `[Snapshot]` →
    `[s]` (`FormantBufferKind::tag`, `App::buffer_names`). The old full-word tags routinely
    ran buffer names out of the panel's width.
  - CDP Process browser capability badges shortened to the same convention: "pitch curve" →
    `[p]`, "formants" → `[f]`, "snapshot" → `[s]` (`cdp_process_badges`) — the old full-word
    badges plus a long process title regularly overflowed the process list's column.
  - The Processes column widened (46 → 62 cols) at the Description column's expense
    (`CDP_BROWSER_PROCESSES_WIDTH`), and the Description column is now mouse-wheel
    scrollable when its text overflows the popup (`cdp_browser_desc_max_scroll`); the
    Processes column is also mouse-wheel scrollable (moves `selected`, same as Up/Down).
    Both are hit-tested against `cdp_browser_layout`, a geometry helper factored out of the
    renderer so a scroll can never land on the wrong column.
  - **Fixed (NASTY BUG):** opening the envelope editor's "use curve" picker (`c`) while
    graphics mode was on left the picker completely obscured by the envelope's own bitmap
    curve, redrawn on top of it every frame. The graphics-mode redraw block matched on
    `dialog.envelope` being `Some` without checking whether the curve picker sub-overlay was
    open, and reused `dialog_row_rects.first()` as its target `Rect` — but the picker's own
    renderer returns an empty row-rect list, so `dialog_row_rects` never got updated for the
    picker's frame and still held the *envelope grid's* stale `Rect` from the frame before
    `c` was pressed. Now gated on `edit.curve_picker.is_none()`.
- Bumped version to 1.3.1, covering the CDP UI cleanup pass above.

## 2026-07-20

- **USERGUIDE and README rewritten for accuracy and brevity.** `USERGUIDE.md` shrinks from
  345 to 234 lines, consolidating redundant sections (e.g. the `i`/`f` toggles previously
  listed under both Playback and Toggles) and tightening prose throughout. Adds the complete
  Tier 2/3 CDP workflows (pitch curves, formants, freeze-at-cursor) that were missing
  entirely, and documents the new capability badges (`>1 inputs`, `pitch curve`,
  `formants`, `snapshot`) and buffer row types (`[Curve]`/`[Formant]`/`[Snapshot]`). Both
  docs also fix a stale menu path — "Options → Configure CDP Directory" is now "CDP →
  Configure CDP Directory" (the Options menu no longer exists).
- **CDP dialog UX consistency audit.** `b`/`e` smart-activation keys now work from anywhere
  in a CDP params form instead of only when the target field already has focus (priority:
  the focused field if eligible, else the first not-yet-configured eligible field, else the
  first eligible field). `Enter` on an unset required envelope/list field, or an unpicked
  formant-buffer field, now opens its editor/picker instead of running Apply and
  immediately failing with a generic "value out of range" error, matching the standalone
  curve-transform dialog. The process browser gained capability badges for "pitch curve"
  and "formants"/"snapshot" alongside the existing ">1 inputs", and curve-only transforms
  (Repitch Exaggerate/Smooth/...) are now hidden from the main browser since they can only
  ever run against an open pitch curve. Also fixed a real bug found while testing the
  above: `Space` was unconditionally intercepted for every dialog, so no free-text field
  anywhere — CDP browser search, every Rename dialog, Open Directory, Save Curve As, Load
  Pitch Curve, CDP Setup — could contain a space; it now falls through to normal text
  insertion except in the four dialogs that use it as a checkbox toggle.
- The CDP directory now defaults to `~/cdp` (resolved against the real `$HOME` at startup)
  before prompting, instead of always starting from an empty setting.
- Bumped version to 1.2.0, covering Tier 2 (pitch curve extraction, editing, CDP
  transforms) and Tier 3 (formant/snapshot buffers, freeze-at-cursor) of the CDP
  integration, the dialog UX consistency audit above, and the new `~/cdp` default.

## 2026-07-19

- Added **"Freeze Formant Snapshot at Cursor"**, a new CDP menu action that freezes a
  `[Snapshot]` buffer at the waveform cursor with no manual steps: it reuses an existing
  `[Formant]` extraction on the current document, or runs Extract Formants automatically
  first and chains the freeze onto its result. Replaces the old per-buffer freeze flow (the
  `f` key and typed-time prompt in the Formant Info popup), which is now purely read-only.
- Reworked the curve-transform params dialog (Repitch Quantise etc.) to match the main CDP
  params dialog's UX, after a user report that it was a bespoke reimplementation that had
  drifted: `Enter` on a required-list field now opens its editor (previously a no-op off
  the Apply row), `Shift+Tab` navigates backward (previously dead — terminals emit
  `BackTab`, which the handler didn't catch), and mouse clicks on form rows now
  focus/open/toggle fields or run Apply.
- Fixed the envelope `c` curve-picker giving no feedback when no curves were open — it now
  always opens the picker, showing "(no open curves)" instead of silently doing nothing.

## 2026-07-18

- **Tier 3 of the CDP integration: formant and snapshot buffers.** CDP → Extract Formants
  captures a selection's spectral envelope as a `[Formant]` buffer (best on voice or an
  instrument with real timbre); Formants Put and Oneform Put impose a `[Formant]` or
  `[Snapshot]` buffer onto other audio (Replace/Layer and Impose/Replace variants), and two
  new pitch/frequency-band Formant Vocode processes round out the catalog additions.
- Fixed the code-review findings in `FABLE-REVIEW.md` (FR-1 through FR-9), each with a
  regression test: a stale Preview cache that didn't invalidate when the picked formant
  buffer changed (could splice stale audio); dirty curves not counting toward the quit
  confirmation and not being covered by Save All; an in-progress hand edit in the curve
  editor being silently discarded on a failed transform; curve-template undo; a `tick_cdp`
  job-id check that ran after (instead of before) consuming pending CDP results; `Ctrl+W`
  not closing a focused curve/formant row in the Buffers panel; no inline validation on the
  freeze-time prompt; a no-op `Enter` in the curve editor; and the formant-buffer picker not
  preselecting the current pick.

## 2026-07-17

- Continued Tier 3 CDP work: formant-related catalog entries (Formants Put, Oneform Put,
  and the pitch/frequency-band Formant Vocode processes) and their pipeline plumbing.

## 2026-07-15 – 2026-07-16

- **Tier 2 of the CDP integration: pitch curves.** CDP → Extract Pitch Curve analyses a
  selection (best on a clear monophonic note/melody) into a `[Curve]` buffer with a
  Time/Hz table editor — arrows select, typing overwrites, `n` inserts and `Delete` removes
  points, `t` applies a CDP curve transform (quantise, smooth, vibrato, and the rest of the
  new Repitch process family). A curve can drive any pitch-curve-badged process (e.g. Psow
  Stretch) by loading it into the process's pitch field, rescaled to the selection; `Ctrl+S`
  saves a curve to disk and CDP → Load Pitch Curve reads one back (a hand-typed or loaded
  curve can be edited but can't run a transform, having no CDP source).

## 2026-07-13

- **CDP: reverb re-added, dual-input processes marked in the browser, 36 new processes, and
  a real sample-rate-dependent-range bug fixed.** `Reverb (Comb/Allpass)` is back in the
  catalog (dropped two sessions ago for a WAV-format incompatibility that now has a real
  fix). The process browser marks any process needing a second buffer as input with a pale
  ">1 inputs" note next to its name, so that's visible before opening it. A further pass
  over every CDP binary not yet in the catalog added `caltrain`, `cantor`, `constrict`,
  `distortt`, `frfractal`, `hover`/`hover2`, `prefix`, `strans`, `tremolo`, `rotor`,
  `synfilt`, `clicknew`, `distmark`, `verges`, `motor`, `shifter`, `superaccu`, `brownian`,
  `phasor`, `fastconv` (a new convolution-with-a-second-buffer effect), `subtract`,
  `specsphinx`, `spectwin`, and the start of a `pitch`/`repitch` family (transpose, pick,
  tune, chord-building) — 36 processes total, each individually verified against the real
  CDP binaries. Also fixed `Inharmonic Glissandos`, reported failing at its own unchanged
  default settings: its real valid range depends on the file's sample rate, which the
  catalog previously declared as a fixed range that only happened to work at common sample
  rates.

## 2026-07-12

- **CDP: fixed `rmverb` silently distorting audio.** Reported as "produces good tail but
  entirely distorts the source audio" — the binary was misreading the app's normal 32-bit
  float WAV input as raw integer samples, producing garbled output without ever raising an
  error. Fixed by writing plain 16-bit audio to the small set of CDP binaries that need it,
  rather than the app's usual working format.

## 2026-07-10

- **CDP: three more real bugs fixed from manual testing**, and a fourth parameter shape
  (plain ordered lists — grain-onset times, per-grain transpositions) added alongside the
  existing breakpoint-envelope one, covering `Grain Reposition`, `Grain Repitch`, `Grain
  Rerhythm`, and `Stutter`. The list editor now enforces ascending order for time-based
  lists and scales its nudge step to the actual selection length instead of the CDP
  binary's own maximum (which made a single tap jump by minutes on a short file); `Grain
  Reposition` failed outright at some parameter combinations because a few of its ranges
  depend on the real selection's duration, not a fixed catalog value; and long CDP error
  messages now wrap to fit the dialog instead of being cut off mid-sentence.

## 2026-07-09

- **CDP: 13 new processes SoundThread never covered, and a smoke-test harness to add more
  safely.** The built-in catalog (previously all SoundThread-derived) gains a hand-authored
  extension file with `Time Stretch (Spectral)` (phase-vocoder time-stretch, distinct from
  the existing granular one), `Iterate`, `Gate (Silence)`/`Gate (Trim)`, `Echo`, `DVD Wind`,
  `Flatten`, `Tremolo Envelope`, `Trim Silent Ends`, `Waveset Double`, `Emphasise Changes`,
  `Spectral Band`, and `Impulse Stream` — each verified against the real CDP binaries via a
  new gated test that runs every catalog entry once and asserts it succeeds
  (`TUI_WAVE_CDP_SMOKE=1 cargo test catalog_smoke_test`), catching two real bugs (a wrong
  binary name, two params in the wrong argv position) before they shipped.

## 2026-07-08

- **CDP: breakpoint automation, a two-step browser/params flow, and per-process presets.**
  Any automatable parameter — shown in **green** in the parameter form — can now be driven
  by a breakpoint envelope instead of a fixed value: press `e` on it to open a dedicated
  editor (insert/delete/drag points, coarse and fine nudging, a graphics-mode curve overlay
  with a reference waveform in terminals that support kitty/Sixel/iTerm2 graphics). The
  process browser is a fixed-size list+description dialog again — it no longer resizes as
  you scroll — with working PageUp/PageDown and click-to-open; selecting a process opens a
  separate parameter dialog sized for just that process, with its own scroll if the process
  has more parameters than fit. That dialog also gained a preset row: `s` saves the current
  values under a name, `d` deletes the selected preset, `←`/`→` cycles through saved ones —
  stored per process under `$XDG_CONFIG_HOME/tui-wave/cdp_presets/`. Also fixes a real bug
  where automating certain parameters (e.g. `blur_blur`'s "Blurring") made CDP reject the
  run with an out-of-range error, and tightens up the parameter form's column alignment and
  the browser's description-text margins.

## 2026-07-07

- **CDP (Composer's Desktop Project) integration.** A new dialog-driven front-end to the CDP
  suite of external command-line sound-transformation tools, reachable with `Ctrl+P` or
  **Process → CDP Process…**. Browse/search a catalog of ~120 processes, edit their
  parameters in a generated form, Preview the result through the speakers without touching
  the document, then Apply it to the selection (or whole file) with full undo. Spectral
  processes are wrapped transparently in phase-vocoder analysis/resynthesis; dual-input
  processes (combine/morph/vocode) take a second open buffer via a picker row; synthesis
  processes insert at the cursor. The external binaries run on a background thread so the UI
  never blocks, with a cancellable progress dialog and CDP's own error text surfaced in a
  scrollable viewer. CDP isn't bundled — configure the binaries directory on first use or via
  the new **Options → Configure CDP Directory…** menu. Custom/override process definitions can
  be dropped into `$XDG_CONFIG_HOME/tui-wave/cdp/*.toml` (see
  `docs/cdp-custom-process-example.toml`). The built-in catalog is derived from SoundThread
  (MIT — see `THIRD_PARTY_NOTICES.md`).

## 2026-07-03

- Export Regions' Limit length/Normalize options (added 2026-07-02) gained the validation,
  layout, and mouse fixes a code review turned up: a checked option with a blank or
  unparseable value now blocks "Do!" and focuses the offending field instead of silently
  falling back to a value (a blank Normalize field used to boost every exported region to
  0 dBFS; a blank limit used to silently disable a cap the checkbox said was on); a
  sub-millisecond length limit no longer rounds down to an empty WAV; the dialog's
  clickable "Do!" row now lines up with the rendered hints bar (it was one row off) and
  never collides with a field row on a short terminal; and clicking a checkbox+value row's
  value text now focuses that field for editing instead of only ever toggling the checkbox.
  Also extracted the peak/dB-gain math shared by Normalize, Gain, mix-to-mono, and the
  dB-scale axis into one `model::dsp` module so it can't drift between call sites again.

- Fixed selecting to the end of the file (Shift+End, Shift+`]` past the last marker, and a
  mouse drag into the last visible column) excluding the file's actual last sample —
  selection bounds are exclusive-end everywhere, but these paths clamped to the last
  sample's *index* rather than one past it, so deleting or trimming a "select to end"
  selection always left a sliver of the original ending behind.

## 2026-07-02

- Export Regions to Subfolder gained two more per-region options, both off by default:
  **Limit length** (ms) truncates the end of each region so it can't exceed the given
  duration, and **Normalize regions** (dB) scales each region independently to a target
  peak level. Per-region processing order is limit length, then normalize, then fades — a
  region is trimmed to size before its peak is measured for normalization, and fades are
  applied last so the envelope taper is never itself part of that peak measurement or of
  what gets cut off by the length limit.

- Pressing Enter on a buffer in the Buffers panel now hands focus to the waveform after
  switching to it (both the plain-Enter and filter-search Enter paths), instead of leaving
  the Buffers panel focused — picking a buffer to work on is almost always followed by
  editing it. The Files panel keeps its existing behavior of staying focused after Enter,
  since browsing to open several files in a row shouldn't require re-focusing in between.

- Fade In/Out with no active selection now defaults to a cursor-relative range instead of
  the whole file: Fade In runs from the start of the file to the insertion point, Fade Out
  runs from the insertion point to the end of the file. Other operations that share the
  same "act on the whole file when nothing's selected" default (Normalize, Gain) are
  unaffected — this is fade-specific, since a fade's direction gives it an obvious anchor
  the others don't have.

- The Gain dialog now offers **per-channel gain** on stereo buffers: a "Per-channel gain"
  checkbox (only shown when the active document has exactly 2 channels) splits the single
  Gain field into separate Left/Right dB fields when checked, so each channel can be
  boosted or attenuated independently. Unchecked (the default), Gain behaves exactly as
  before — one value applied uniformly to every channel. Vertical order is Gain/Left, then
  Right (blank until checked), a blank separator, then the checkbox, then Tanh limiter; the
  popup is a fixed size whether or not the box is checked, so toggling it never resizes or
  reflows the dialog.

- Graphics-mode waveform is now anti-aliased. Span edges stay in continuous sub-pixel
  coordinates and the fractional first/last pixel of each column blends against the
  background, so sub-pixel amplitude changes render as a smooth curve instead of flat runs
  with hard one-pixel jumps (the staircase visible at some zoom levels). Applies to both
  the mid-zoom min/max bars and the high-zoom polyline.

## 2026-07-01

- Fixed the graphics-mode waveform breaking into dashes at mid zoom levels. Each pixel
  column's min/max bar covered only its own samples, so on steep slopes the inter-sample
  step across a column boundary fell between adjacent bars and the trace visibly
  disconnected. Bars now extend to overlap the previous column's bar by at least one pixel
  row — the bar-mode counterpart of the connection the high-zoom polyline mode already had.

## 2026-06-30

- Files panel: **Rename** (`Ctrl+r`) renames the selected `.wav` on disk via a name dialog (Esc
  cancels; a buffer open on that file follows the rename), and **Delete** (`Del`) removes it
  from disk after a confirmation (deleting is irreversible). Both also appear in the Files
  toolbar.

- The waveform header now shows the active buffer's real name (e.g. `_NEW_006` for a
  never-saved buffer, matching the Buffers panel) instead of "untitled", and drops the
  meaningless "tui-wave —" prefix. The no-file placeholder reads "No file loaded".
- "Reset Config to Defaults" now asks for confirmation before wiping keybindings (and still
  backs the old config up to `.bak`).
- Confirmation dialogs now show "(Esc) cancel" instead of "(n) cancel" — Esc is the natural
  cancel key (any non-confirming key still cancels).
- Shift+Tab now cycles backward, the reverse of Tab — both for panel focus (Waveform →
  Buffers → Files → Waveform) and for fields within a dialog (Save As, Gain, Mix to Mono,
  Export Regions, Fade). Works under the kitty keyboard protocol (Tab+Shift) and on terminals
  that send a legacy BackTab.
- Fixed menu dropdown entries that overlap the Files/Buffers panels being unclickable — the
  panel underneath was stealing the click. An open menu now takes mouse precedence over the
  panels beneath it, matching how it already intercepts the keyboard.
- "Reset Config to Defaults" now backs up the existing `config.toml` to `config.toml.bak`
  before overwriting it, so a reset can be undone.
- Playback that reaches the end of a (non-looping) track now actually stops: previously the
  "playing" state stuck, so the next Space press paused a finished track instead of replaying
  it. Space now replays from the cursor in one press.

- Fixed zoom (Up/Down) restarting playback from the cursor position instead of continuing
  from the current playhead. Navigation actions seek the audio position only when the
  cursor actually moves; zoom-only actions leave the playhead untouched.
- Quick Save (Ctrl+S) now preserves the source file's original bit depth (16-bit int saves
  as 16-bit, 24-bit as 24-bit, float as float) instead of always promoting to 32-bit float.
  Save As now defaults to the document's original bit depth rather than float.
- Renamed "unsaved" buffers (no path yet) from `_UNSAVED_001` to `_NEW_001` in the Buffers
  panel for clearer intent.
- **UI restructure**: removed the Channels menu/toolbar section. Mix to Mono moved to
  Process. New from Left / New from Right moved to File. Both menus and the toolbar
  reflect the change.
- **Export Regions to Subfolder** (Shift+E): chops the active buffer at its markers and
  saves each region as a numbered WAV file into a new subfolder. Opens a dialog to set the
  subfolder name, base filename, bit depth, optional dither, and optional fade in/out (with
  an editable millisecond length, default 5 ms) applied to each region. If no markers are present,
  shows an info popup. The first region is `[file start → first marker]`, the last is
  `[last marker → file end]`; files are named `{base}-001.wav`, `-002.wav`, etc.

## 2026-06-29

- Fixed Fade In / Fade Out silently doing nothing on small selections. When zero-crossing
  snap contracted both endpoints of a short selection to the same crossing (making the range
  degenerate), the fade was skipped with no feedback. The fix falls back to the un-snapped
  range in that case so the fade always applies over at least the selected samples.

## 2026-06-28

- All keyboard shortcuts are now configurable via `~/.config/tui-wave/config.toml` under a
  `[keybindings]` section. Every action lists its default key string(s) there on first save.
  Key string format: `"ctrl+x"`, `"shift+left"`, `"L"`, `"space"`, `"delete"`, etc.
  Menu and toolbar display strings now reflect the configured binding — remapping a key
  updates every shortcut hint in the menu and toolbar accordingly.
- The config file (`~/.config/tui-wave/config.toml`) is now written on the very first
  launch so all available keybindings are immediately visible without having to trigger a
  toggle first. On subsequent launches after an upgrade, any newly-added default bindings
  are appended automatically without touching existing custom entries.
- Shift+letter shortcuts now show as `S+C`, `S+L`, `S+M` etc. in the toolbar (and
  `Shift+C`, `Shift+L`, `Shift+M` in the menus) instead of the bare uppercase letter,
  making it clear that Shift is required.
- **File › Reset Config to Defaults**: resets the `[keybindings]` section of the config
  file to factory defaults while preserving all other settings (snap, zoom, loop, etc.).
  Takes effect immediately — the key map and all shortcut hints update without restarting.
- All option-bearing dialogs now follow a consistent multi-row UX: checkboxes appear as
  `[X] Label` rows, cycle selectors show `◄ Label ►`, and a hints bar at the bottom of
  each popup lists the relevant keys (`Tab:next  Space:check  ←→:change  Enter:apply`).
  Dialogs are now mouse-aware: clicking a row focuses it; clicking a checkbox row also
  toggles it; all other mouse events are absorbed while a dialog is open.
  - **Gain**: text field and `[X] Tanh limiter` checkbox as separate rows; Tab/Space/Enter.
  - **Fade In / Fade Out**: `◄ Curve ►` cycle row; ←/→ to step through Exp/Log/Linear.
  - **Save As**: filename field, format cycle row (`◄ Format ►`), and `[X] Dither` checkbox
    as three distinct rows; Tab to move focus, ←/→ to change format, Space to toggle dither.
  - **Mix to Mono**: added `Space:check` hint to the existing hints bar.
- Mix to Mono dialog: Tab now only cycles between channel input fields (it no longer toggles
  the tanh checkbox as a side effect). Press Space to toggle the tanh limiter when that row
  is focused.
- Markers are now preserved when creating new buffers via Copy to New (`C`), New from Left
  (`L`), New from Right (`R`), and Mix to Mono. If a selection is active, only markers
  within that range are carried over, with their positions shifted to be relative to the
  new buffer's start.
- Fixed Auto-Insert Markers missing a transient at the very start of a file: if the
  opening audio decays significantly into the second analysis frame, position 0 is now
  correctly marked.
- Fixed the transient threshold not actually defaulting to 13 dB on a fresh install
  (the toolbar field was updated but the config default wasn't).

- Added a **Channels** menu and toolbar group with three commands: **Mix to Mono**
  (`Ctrl+m`), **New from Left** (`L`), and **New from Right** (`R`). All three are
  selection-aware — if a selection is active, only that range goes into the new buffer;
  otherwise the whole file does.
- Mix to Mono opens a dialog to set per-channel gain in dB (`0` = unity, `-inf` = silence
  that channel). Tab cycles through fields and the tanh soft-limiter toggle; Del sets the
  current field to `-inf`.
- The selected-range waveform is now rendered as a dark bar on a cyan background instead of
  yellow-on-dark, giving much higher contrast.
- The dB scale no longer pins 0 dB to the top row when zoomed in vertically — marks that
  fall outside the visible amplitude range disappear, so the scale always reflects what's
  actually on screen.
- The transient detection threshold defaults to 13 dB (was 6 dB).
- Added graphics-mode waveform rendering (kitty/Sixel/iTerm2): when a supported terminal
  is detected, the waveform is drawn as a real bitmap at pixel resolution rather than
  character blocks. Toggled with `g`; persists between sessions. Falls back to text mode
  silently in tmux, screen, or unsupported terminals.

## 2026-06-25

- Added an **Audition** toggle to the Files panel: navigating to a file previews it by
  playing straight from disk, without loading it into a buffer. A single click in the
  Files panel now selects (and auditions, if enabled); double-click opens.
- Added PgUp/PgDn paging in the Files panel for browsing directories with many files.
- Settings — zero-crossing snap, auto vertical zoom, fine-step mode, loop playback, and
  Audition — now persist between sessions (`~/.config/tui-wave/config.toml`).
- The Files panel is focused on startup, so the first thing you do is pick a file.
- Double-clicking the waveform between two markers (or before the first / after the last)
  selects that whole region.
- Clicking any panel — including the waveform — now focuses it.
- The Buffers panel loads the selected buffer immediately as you navigate, no Enter needed.
- Added Ctrl+A (Select All); Ctrl+R (Reverse) now works with no selection by reversing the
  whole file.
- Normalize now defaults to 0 dB instead of -1 dB.
- Arrow-key navigation accelerates the longer you hold a key — fixed so that fast manual
  tapping (not an actual held key) never falsely triggers acceleration.
- Renamed a few on-screen shortcut legends for clarity (backtick → `~`, Snap/Auto/Fine →
  zeroXSnap/AutoVZoom/fineNavi).
- Fixed the waveform going blank at high zoom (down to 1 sample/column): a single-sample
  column now draws a thin mark at its amplitude instead of vanishing.
- Added **Insertion Point Follows Playback** (`i`): pausing snaps the cursor to wherever
  playback stopped.
- Added **Viewport Follows Playback** (`f`): once the playhead reaches the right edge
  during playback, the view recenters and keeps scrolling so the playhead stays visible.
- Audition's shortcut moved from `p` to `a` in the Files panel (the same `a` still means
  Auto Vertical Zoom when the Waveform is focused — the app is modal).
- Marker insert/delete/rename/drag-move are now all undoable, like any other edit.
- A marker sitting exactly on the insertion point now renders in the cursor's accent
  color, so it no longer looks like the cursor has disappeared.
- Fixed the menu's dropdown rendering underneath the waveform/toolbar instead of on top of it.
- Added **Next Rising Edge** (`/`): jumps the cursor to right before the next transient
  (a sudden rise in volume) from the current position onward. The detection threshold
  defaults to 6dB, is adjustable with `+`/`-`, and persists between sessions.
- Added **Auto-Insert Markers at Transients** (`t`): scans the whole file and drops a
  marker before every detected transient, using the same threshold as Next Rising Edge,
  as a single undoable action.
- Added **Technical Fades** (`Ctrl+b`): a fixed 5ms exponential fade in at the start and
  fade out at the end of the whole file — the standard pre-export move to mask the click a
  hard cut to/from silence would otherwise leave at the file's boundaries.
- Fixed Next Rising Edge stopping well before the actual transient: a faint puff of
  pre-roll noise rising out of near-silence no longer gets mistaken for the real one.
- Added **Previous Rising Edge** (`?`): the same transient detection, searching backward.
- Next/Previous Rising Edge now center the viewport on the new cursor position instead of
  just nudging it into view, so there's context on both sides at any zoom level.
- Added Shift+[ / Shift+] (`{` / `}`): selects from the cursor to the previous/next marker
  (or the start/end of the file if there's none), advancing the cursor to the selection's
  new edge and scrolling it into view.
- Added a "Deselect" button to the toolbar's EDIT group.
- The toolbar's transient threshold now reads "Thresh 6dB" instead of a bare "6dB".
- Fixed "Save All & Quit" (and closing a single buffer with unsaved changes) silently
  discarding never-saved buffers instead of asking for a filename — it now prompts for a
  name for each one, in turn, before actually quitting/closing.
- Fixed a waveform display glitch right after a fade (most visible with Technical Fades'
  short 5ms ramp): the cache backing the waveform could report a column's level as already
  back at full volume one column early, bleeding in the next bin's content. The fade math
  itself was always correct — this was purely a display-precision bug in the cache, now exact.
- Added graphics-mode waveform rendering: on terminals that support the kitty (or
  compatible Sixel/iTerm2) graphics protocol, the waveform now draws as a real bitmap
  instead of character glyphs, with markers, the insertion point, and the playhead
  rasterized directly into the image. Falls back automatically (and unconditionally on
  tmux/screen) to the existing text renderer when no compatible protocol is detected.
  Toggle with `g`; persists between sessions via `graphics_mode` in the config file,
  defaulting to on whenever a capable terminal is detected.

## 2026-06-24

- Added a directory-aware Files panel (browse folders, not just load one fixed file) and a
  Buffers panel for working with multiple open documents at once.
- Added a modal command toolbar that shows different commands depending on whether the
  Waveform, Files, or Buffers panel is focused.
- Added search/filter within both the Files and Buffers panels.
- Added a modifier-free "fine-step" toggle for single-sample-precision navigation.
- Added a Process menu.
- Several toolbar layout and visual polish passes (grouped sections, column alignment,
  accent colors).
- Added a README with a screenshot.

## 2026-06-23

- Added Fade In/Out, Trim, and Resample (sample-rate conversion) commands.
- Added timeline markers (cue points), saved/loaded via BWF-compatible WAV chunks
  (interoperable with Audacity/Sound Forge).
- Added Gain with optional soft-clip (tanh) saturation, and a Normalize dialog.
- Fixed zero-crossing snapping for multi-channel audio.

## 2026-06-22

- Initial release: load and view a WAV file as a waveform, with keyboard navigation and
  zoom.
- Audio playback with sample-accurate position tracking.
- Selection, cut/copy/paste, and undo/redo.
- Menu bar and toolbar.
- Save/export WAV, with dirty-flag tracking and a quit confirmation.
- Catppuccin Mocha theme, sub-cell-precise waveform rendering, a dB scale, and a visible
  playhead marker.
- Loop playback and zero-crossing snapping for selections.
- Fixed a performance issue with large files and reworked zoom keybindings.
