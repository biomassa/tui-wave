#!/usr/bin/env python3
"""Convert praatAudioTools scripts into a tui-wave process catalog.

Run manually; the output is committed. Mirrors scripts/convert_soundthread_catalog.py.

    python3 scripts/convert_praat_audiotools.py

Reads the submodule at third_party/praat-audiotools and writes
src/model/cdp/praat_catalog.toml, plus a companion exclusion report at
docs/praat-excluded-scripts.md naming every script that was left out and why.

Why a converter rather than hand-authored entries: there are 416 candidate scripts with
~5500 parameter declarations between them. Hand-maintaining that is not reviewable, and
upstream is a moving target.

Two things this cannot get from the source, both handled deliberately below:

* **Ranges.** A Praat `form` declares a default and nothing else, except for the ~20 numeric
  parameters (of ~2700) that state a range inside their own name, e.g. `Threshold_(0-1)`.
  Those are read verbatim (see `range_from_name`); everything else is left genuinely
  unbounded, keeping only the floor Praat's own form parser enforces. Bounds used to be
  synthesised at ten times the default, which was invention presented as fact.
* **Which scripts work.** Roughly one in six cannot be driven headlessly at all. They are
  detected statically here (see EXCLUSIONS) rather than being discovered at runtime by the
  user.
"""

from __future__ import annotations

import ast
import math
import re
import subprocess
import sys
from dataclasses import dataclass, field, replace
from decimal import Decimal
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
PLUGIN = REPO / "third_party" / "praat-audiotools"
OUT_CATALOG = REPO / "src" / "model" / "cdp" / "praat_catalog.toml"
OUT_REPORT = REPO / "docs" / "praat-excluded-scripts.md"

# Upstream directory -> browser group heading. Must match `PRAAT_DIRS` in
# src/model/cdp/group.rs; a directory absent from here is excluded wholesale.
#
# Four headings are shortened from their directory name because the browser's Groups column
# gives a heading 13 usable columns: Generative & Synthesis -> Generative, Dynamics & Envelope
# -> Dynamics, Spatial & Surround -> Spatial, Time & Granular -> Time/Granular.
GROUP_DIRS = {
    "AI & Adaptive": "AI & Adaptive",
    "Analysis": "Analysis",
    "Distortion": "Distortion",
    "Dynamics & Envelope": "Dynamics",
    "Filter & Color": "Filter/Color",
    "Generative & Synthesis": "Generative",
    "Modulation": "Modulation",
    "Pitch": "Pitch",
    "Reverb": "Reverb",
    "Spatial & Surround": "Spatial",
    "Spectral": "Spectral",
    "Time & Granular": "Time/Granular",
    "Vector Chain": "Vector Chain",
    # Praat scripts that drive a Python helper. Their own heading, so the extra prerequisite
    # (numpy/scipy/soundfile on PATH) is visible in the browser rather than a surprise at run
    # time -- and so they can be avoided wholesale by anyone who has not installed them.
    "py": "py",
}

# `py/` shells out to Python/IRCAM/VST3 tooling and is out of scope entirely. `Max-MSP` is a
# single Max/MSP helper rather than a sound process.
#
# `Vector Chain` is *not* skipped: those scripts chain sibling scripts located through
# `preferencesDirectory$`, which the runner satisfies by pointing Praat at an app-owned
# preferences directory holding a `plugin_AudioTools` symlink (see `prepare_prefs_dir`).
SKIP_DIRS = {"Max-MSP"}

# Third-party Python modules this app is willing to make a prerequisite. Everything in the
# standard library is fine; anything else here disqualifies the script.
#
# The `py/` scripts write a temp WAV, shell out to a sibling `.py`, and read the result back.
# Their Python halves split sharply: about half need only array maths, while the rest want
# torch, librosa, encodec, ddsp, PySide6 or tkinter -- and six open a Tk window, which is the
# same unusable-headless problem `beginPause` is. Requiring numpy/scipy/soundfile is a
# reasonable ask; requiring a deep-learning stack to open a waveform editor is not.
#
# Derived per script rather than listed, so this survives a submodule bump: a new numpy-only
# script appears on its own, and one that gains a torch import drops out with a reason.
PY_ALLOWED_IMPORTS = {
    "numpy",
    "scipy",
    "soundfile",
    # The optional extras `install.sh` offers, needed only by the interactive editors:
    # `sounddevice` for Arranger and Performance Launcher, `PIL` (pillow) for Spectral Eraser.
    # Listing them here means those three appear in the browser whether or not the extras were
    # installed -- and if they were not, the script's own dependency check reports it by name.
    # That is the bargain the whole `py` group already makes, and the reason it is its own group.
    #
    # `pedalboard` is deliberately NOT here, and not for a policy reason: wheel 0.9.24 dies with
    # SIGILL on import (reproducible, on a 13th-gen Intel Core i7-1355U), so `VST_Effect_from_
    # Praat` could not work even with it installed.
    "sounddevice",
    "PIL",
    # ---- The optional tiers `install.sh` and `setup-environment.sh` offer -----------------
    #
    # Same bargain as `sounddevice`/`PIL` above, and for the same reason: a process listed here
    # appears in the browser whether or not its library was installed, and if it was not, the
    # helper's own dependency check reports the missing module *by name* (verified across
    # these helpers -- every one guards its imports). `praat_error_lines` never truncates
    # stderr, so even a helper without a guard surfaces a `ModuleNotFoundError` naming the
    # module. That is strictly better than hiding the process, which left a user unable to
    # discover that the capability exists at all.
    #
    # Split into two tiers because the sizes are not comparable and the installers ask about
    # them separately: the light tier is ~150 MB of ordinary wheels, the ML tier is ~2.5 GB.
    #
    # Light tier -- librosa (6 processes), scikit-learn (4), nara_wpe (1), mido (1).
    #
    # OpenCV (`cv2`) was here for one process, `MotionControl`, and left with it: that script is
    # now `NEVER_PLANNED`, and cv2 is imported by no other helper in the plugin. A dependency
    # kept for a process nobody can reach is ~90 MB of wheel that buys nothing, so the light
    # tier lost it rather than carrying it against some future script that might want it.
    "librosa",
    "sklearn",
    "nara_wpe",
    "mido",
    # ML tier -- torch (5 processes), and the codec/synthesis stacks built on it. Kept behind
    # its own prompt: `torch` alone is roughly 2.5 GB, and several of these additionally need
    # model files the library install does not provide (IRCAM's RAVE wants a `.ts` the user
    # supplies), so installing the tier does not by itself make every process usable.
    "torch",
    "torchaudio",
    "dac",
    "encodec",
    "ddsp",
    "gin",
}

# Standard-library modules that mean the helper opens a window and waits for the user.
#
# These do NOT disqualify a script -- the windows genuinely work, being a separate Python
# process with its own display, unlike Praat's own `beginPause` which segfaults under `--run`.
# What they cannot do is finish inside a wall-clock limit, so the entry is marked `interactive`
# and the runner drops its timeout for it (see `ProcessDef::interactive`).
#
# Being in the standard library says nothing about whether a thing can run headlessly, and that
# gap cost a real failure: `spatial_panner.py` imports `tkinter`, the rule was "nothing beyond
# numpy/scipy/soundfile and stdlib", so it shipped as an ordinary process. The window opened,
# the runner killed Praat at its timeout, and pressing Apply afterwards wrote into a closed
# pipe -- "broken pipe" (user report, 2026-08-03).
PY_INTERACTIVE_IMPORTS = {"tkinter", "turtle", "idlelib"}


def python_imports(path: Path) -> set[str]:
    """Top-level modules a Python file imports.

    Parsed with `ast`, not a regex. A regex over `^\s*(?:import|from)\s+(\w+)` reads ordinary
    English out of docstrings as dependencies -- these files contain lines like "from the
    original version which lacked...", "from them with Boltzmann probability" and "from scratch
    on the log-Mel patches", which had four perfectly usable scripts excluded for needing
    modules named `the`, `them` and `scratch`.

    A file that does not parse is treated as importing nothing rather than crashing the
    converter; it will fail loudly at run time instead, which is the right place for a syntax
    error in someone else's script.
    """
    text = path.read_text(encoding="utf-8", errors="replace")
    try:
        tree = ast.parse(text)
    except SyntaxError:
        return set()
    modules: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            modules.update(alias.name.split(".")[0] for alias in node.names)
        elif isinstance(node, ast.ImportFrom):
            # `level > 0` is a relative import (`from . import x`) -- a sibling file, not a
            # dependency to install.
            if node.module and not node.level:
                modules.add(node.module.split(".")[0])
    return modules


def python_helper_requirements(script: Path) -> tuple[list[str], set[str]] | None:
    """The `.py` helpers a `py/` script drives, and the non-stdlib modules they need.

    `None` when the script drives no resolvable helper at all -- either it is pure Praat that
    merely lives in this directory, or it only writes a `temp_*_probe.py` at run time to test
    for its dependencies, which says nothing about what it ultimately needs.
    """
    text = script.read_text(encoding="utf-8", errors="replace")
    names = sorted({n for n in re.findall(r"([A-Za-z_0-9]+\.py)", text) if not n.startswith("temp_")})
    helpers = [script.parent / n for n in names]
    helpers = [h for h in helpers if h.is_file()]
    if not helpers:
        return None
    needed: set[str] = set()
    for helper in helpers:
        needed |= python_imports(helper)
    extras = needed - sys.stdlib_module_names - PY_ALLOWED_IMPORTS
    return [h.name for h in helpers], extras, bool(needed & PY_INTERACTIVE_IMPORTS)

# Scripts that DO contain a blocking construct, but on a code path this app can guarantee is
# never taken. Keyed by path relative to the plugin root; each entry says which construct, why
# it is unreachable, and what the catalog entry has to do to keep it that way.
#
# This is an override of a *static* detector by a *reachability* argument, so every entry has to
# carry the argument. A wrong one costs a segfault at run time, not a clean error -- confirmed
# against praat 6.6.30, a `beginPause` under `--run` exits 139 with a core dump.
#
# Two kinds of adjustment, and the difference is not cosmetic:
#
#   drop_options  -- name an `optionmenu` param and the option labels to remove. Safe to drop
#                    outright, because Praat matches an optionmenu by *label* and the catalog
#                    stores labels verbatim, so nothing is positional here.
GUI_BLOCKING_OVERRIDES: dict[str, dict] = {
    # Both of these call `chooseFolder$` -- but only as a *fallback for a blank field*, and both
    # say so in the same comment: "use the typed path, or fall back to a dialog when the field is
    # left blank". The field is now a `ParamKind::FolderPath`, which `cdp_validate_fields` blocks
    # Apply on until a folder is picked, so it can never arrive blank and the dialog can never be
    # reached. That is an enforced precondition, not an assumption about user behaviour.
    "AI & Adaptive/Bayesian_Drone_Weaver.praat": {
        "why": "chooseFolder$ is a fallback for a blank Folder field, which is now unpickable-empty",
    },
    "AI & Adaptive/Gesture-Based_Hard_Quantization.praat": {
        "why": "chooseFolder$ is a fallback for a blank Folder_path field, which is now unpickable-empty",
    },
    # Two more of the blank-field-falls-back-to-a-dialog shape, identical to the two above.
    # `OT_CORPUS_CONCATENATOR`'s own changelog names the idiom: "the typed path is whitespace-
    # and trailing-slash-trimmed, a blank field falls back to a chooseFolder$ dialog". Both
    # guard the call with `if directory$ == ""`, and both fields become `ParamKind::FolderPath`,
    # which blocks Apply until a folder is picked -- so blank is unreachable and so is the
    # dialog.
    "AI & Adaptive/OT_CORPUS_CONCATENATOR.praat": {
        "why": "chooseFolder$ is a fallback for a blank Folder field, which cannot be blank",
    },
    "AI & Adaptive/Timbral_Similarity_Browser.praat": {
        "why": "chooseFolder$ is a fallback for a blank Folder field, which cannot be blank",
    },
    # The same shape again, one spelling along: `chooseDirectory$` rather than `chooseFolder$`,
    # and so caught by `folder_chooser` rather than `gui_blocking`. Both already ship -- the call
    # was simply never detected, the regex having missed this spelling -- and in both the chooser
    # is the fallback for a blank folder field that `cdp_validate_fields` will not let be blank.
    # `Sound_atom_composer` names the idiom in its own comment ("use the typed Folder path, or
    # fall back to a dialog when it is left blank"); `KL_Divergence` guards each of its two
    # corpora with `if dirA$ = ""`. Overridden rather than hoisted: there is nothing to hoist, the
    # folders are already ordinary form params.
    "AI & Adaptive/KL_Divergence_Corpus_Resynthesis.praat": {
        "why": "chooseDirectory$ is a fallback for blank Corpus_A/B_folder fields, which cannot be blank",
    },
    "Time & Granular/Sound_atom_composer.praat": {
        "why": "chooseDirectory$ is a fallback for a blank Folder field, which cannot be blank",
    },
    # The only `View & Edit` here, and it sits under one option of one menu: pan mode 8 is a
    # two-pass "draw the pan curve by hand" workflow that opens a RealTier editor and asks the
    # user to come back. The other seven modes never touch it. Dropping that option leaves a
    # 1248-line script with seven working modes rather than nothing.
    "Spatial & Surround/Advanced_Stereo_Panner.praat": {
        "drop_options": {
            "Pan_mode": ["Trajectory: DRAW YOUR OWN (two passes - see Info)"],
        },
        "why": "View & Edit is reached only under pan_mode = 8 (the hand-drawn trajectory pass)",
    },
}

# Scripts whose second (or only) settings page is a `beginPause` dialog, hoisted into the
# catalog as ordinary parameters. Keyed by path relative to the plugin root.
#
# An allowlist, not a rule applied wherever a pause is found: most of the remaining
# `gui_blocking` scripts have a *different* problem (an editor window, a folder chooser) that
# hoisting would not fix, and shipping an entry that still segfaults is worse than shipping
# none. Each entry here has been read.
#
# `praat::runner` rewrites a copy of the script per run, replacing each block with assignments
# to the variables it would have set. `split_on` additionally turns one script into several
# catalog entries, one per option of a hoisted `optionmenu`:
#
#   split_on  -- the hoisted optionmenu whose value selects which further block applies.
#   variants  -- option label -> the `beginPause` title of the block belonging to it. An
#                explicit map because the two do NOT match: the option reads "Bursts and Taps"
#                and its block is titled "Settings: Bursts & Taps".
PAUSE_HOISTS: dict[str, dict] = {
    # One unconditional block, and the author's own changelog explains exactly why it exists:
    # "an attempt to split the long form into two `form...endform` blocks failed ('Unknown
    # variable: random_seed') because Praat only supports one `form` per script run."
    "Distortion/Sidechain_Feedback_VCA.praat": {
        "why": "second settings page (Spatial/Output/Debug); Praat allows one form per script",
    },
    # Guarded by `if preset = 1` (Custom). Its `else` branch already assigns the same variables
    # to the same defaults, which is what makes the rewrite verifiable by inspection: the
    # hoisted assignments and the script's own placeholder block must agree.
    "Time & Granular/Polyphonic_Improviser.praat": {
        "why": "Voice Details page, reached on the Custom preset (option 1, the default)",
    },
    # Three of a shape: `boolean Show_advanced_settings 0` guarding a `beginPause` block of
    # advanced parameters. They shipped first with that toggle merely *locked off*, which made
    # them runnable but left 54 parameters at their script defaults with no way to touch them.
    #
    # Hoisting exposes all 54 in the ordinary dialog, but needs the guard to be true or the
    # assignments the rewrite emits sit inside an `if` that never runs. So the toggle is locked
    # **on** rather than off. That is not a behaviour change smuggled in: with the dialog
    # replaced by assignments the toggle no longer *shows* anything, it only decides whether the
    # advanced values apply -- and the dialog is now where they are set, so they always should.
    # The script assigns every one of them as a plain variable just above the `if`, and those
    # assignments remain as the defaults the catalog reports.
    # Was one of that trio until the 2026-08 Generative rewrite, which replaced its
    # `Show_Advanced_Settings` toggle with a three-page wizard. The stale `lock_on` then named a
    # parameter that no longer existed, the hoist failed, and the script fell through to
    # `gui_blocking` — excluded outright. No toggle to lock any more: the pages are simply the
    # rest of the settings.
    "Generative & Synthesis/FM_Texture_Generator.praat": {
        "lock_on": ["Edit_operator_details"],
        "why": "three-page wizard (Routing/Ops 1-2, Ops 3-4, Ops 5-6 and Timing)",
    },
    # The rest of the 2026-08 Generative rewrite, which gave twelve scripts the same shape:
    # the `form` became page one and every remaining setting moved into `beginPause` pages
    # ending in `endPause: "Run", 1` (or `"Next"` where there are several). Under `--run` that
    # segfaults praat, so without a hoist each of these is excluded — which is what took twelve
    # working generators out of the catalog on the first regeneration after the bump.
    #
    # None is gated by a toggle, unlike the trio above, so none needs `lock_on`: the pages are
    # unconditional and every variable they set is one the script goes on to use.
    "Generative & Synthesis/Coupled_Mesh_String.praat": {
        "lock_on": ["Edit_all_model_parameters"],
        "why": "two-page wizard (Physics, then Geometry & Pickup)",
    },
    "Generative & Synthesis/Dynamic_Stochastic_Synthesis.praat": {
        "lock_on": ["Edit_grain_details"],
        "why": "second settings page (Grain Details)",
    },
    "Generative & Synthesis/Dynamic_Vowel_Transitions.praat": {
        "lock_on": ["Edit_vowel_source_details"],
        "why": "second settings page (Vowel & Source Details)",
    },
    "Generative & Synthesis/Evolving_Grain_Mass.praat": {
        "lock_on": ["Edit_grain_statistics"],
        "why": "second settings page (Grain Statistics)",
    },
    "Generative & Synthesis/Flute_KlattGrid.praat": {
        "lock_on": ["Edit_voice_and_chiff"],
        "why": "second settings page (Voice & Chiff)",
    },
    "Generative & Synthesis/Formant_Grain_Texture.praat": {
        "lock_on": ["Edit_source_filter_details"],
        "why": "second settings page (Source / Filter Details)",
    },
    "Generative & Synthesis/Formant_Synthesis.praat": {
        "lock_on": ["Edit_formant_details", "Edit_source_details"],
        "why": "two settings pages (Resonance Details, then Source Details)",
    },
    "Generative & Synthesis/Formula_Markov_Synthesis.praat": {
        "lock_on": ["Edit_state_synthesis_details"],
        "why": "second settings page (State / Synthesis Details)",
    },
    "Generative & Synthesis/GENDYN_Synthesis.praat": {
        "lock_on": ["Edit_stochastic_boundaries"],
        "why": "second settings page (Stochastic / Boundary Details)",
    },
    "Generative & Synthesis/Karplus-Strong_Texture_Generator.praat": {
        "lock_on": ["Edit_texture_details"],
        "why": "second settings page (Scheduler / Spatial Details)",
    },
    "Generative & Synthesis/Koto\u0144ski_FSM_Event_Generator.praat": {
        "lock_on": ["Edit_state_sound_details"],
        "why": "second settings page (State / Sound Details)",
    },
    # The second wave of the same Generative sweep, `e2cbd5f`. Upstream is working through the
    # folder alphabetically, converting each script to "form is page one, the rest is a
    # `beginPause` page" — the first wave (`a769160`) took A-K, this one G-W. Same reasoning as
    # above: without a hoist each of these is `gui_blocking` and leaves the catalog.
    "Generative & Synthesis/Generative_Sound_System.praat": {
        "lock_on": ["Edit_generative_details"],
        "why": "second settings page (Generative Details)",
    },
    "Generative & Synthesis/Grisey_Spectral_Becoming_Engine.praat": {
        "lock_on": ["Edit_spectral_details", "Edit_process_details"],
        "why": "two settings pages (Spectrum / Threshold, then Process / Output)",
    },
    "Generative & Synthesis/Layered_Markov_Texture.praat": {
        "lock_on": ["Edit_markov_texture_details"],
        "why": "second settings page (Markov / Texture Details)",
    },
    "Generative & Synthesis/Logistic_Map_Synthesis.praat": {
        "lock_on": ["Edit_mapping_details"],
        "why": "second settings page (Mapping Details)",
    },
    "Generative & Synthesis/Lorenz_Deep_Analog.praat": {
        "lock_on": ["Edit_integration_mapping_details"],
        "why": "second settings page (Integration / Mapping Details)",
    },
    "Generative & Synthesis/Markov_Rhythm_Generator.praat": {
        "lock_on": ["Edit_markov_rhythm_details"],
        "why": "second settings page (Markov / Timing Details)",
    },
    "Generative & Synthesis/Poisson_Point_Process_Synthesis.praat": {
        "lock_on": ["Edit_details"],
        "why": "second settings page (Grain / Reproducibility Details)",
    },
    "Generative & Synthesis/Poisson_Rhythm_Synthesis.praat": {
        "lock_on": ["Edit_details"],
        "why": "second settings page (Technical / Reproducibility Details)",
    },
    "Generative & Synthesis/PolyrhythmsFromDots.praat": {
        "lock_on": ["Edit_details"],
        "why": "second settings page (Dot / Audio Details)",
    },
    "Generative & Synthesis/Pulsar_Synthesis_Engine.praat": {
        "lock_on": ["Edit_details"],
        "why": "second settings page (Details)",
    },
    "Generative & Synthesis/Random_Walk_Melody.praat": {
        "lock_on": ["Edit_details"],
        "why": "second settings page (Details)",
    },
    "Generative & Synthesis/Random_Walk_Rhythm.praat": {
        "lock_on": ["Edit_details"],
        "why": "second settings page (Details)",
    },
    "Generative & Synthesis/Rich_Formant_Grains.praat": {
        "lock_on": ["edit_details"],
        "why": "second settings page (Details)",
    },
    "Generative & Synthesis/Risset's_Mutations.praat": {
        "lock_on": ["Edit_details"],
        "why": "second settings page (Details)",
    },
    "Generative & Synthesis/Spectral_Image_Sonification.praat": {
        "lock_on": ["edit_details"],
        "why": "second settings page (Details)",
    },
    "Generative & Synthesis/Stockhausen_Studie_II_Generator.praat": {
        "lock_on": ["Edit_details"],
        "why": "second settings page (Model / Audio Details)",
    },
    "Generative & Synthesis/Subtractive_Synthesis_Generator.praat": {
        "lock_on": ["Edit_details"],
        "why": "second settings page (Details)",
    },
    "Generative & Synthesis/Vector_Synthesis.praat": {
        "lock_on": ["Edit_details"],
        "why": "second settings page (details)",
    },
    "Generative & Synthesis/Visual_Game_of_Life_Synthesis.praat": {
        "lock_on": ["Edit_details"],
        "why": "second settings page (Details)",
    },
    "Generative & Synthesis/Wave_Terrain_Synthesis.praat": {
        "lock_on": ["Edit_details"],
        "why": "second settings page (details)",
    },
    "Generative & Synthesis/Waveguide_Klangmaschine.praat": {
        "lock_on": ["Edit_reverb_render_details"],
        "why": "second settings page (Reverb / Render Details)",
    },
    "Generative & Synthesis/Waveguide_Modal_Synthesis.praat": {
        "lock_on": ["Edit_details"],
        "why": "second settings page (details)",
    },
    "Time & Granular/HFD-Driven_Time_Warping.praat": {
        "lock_on": ["Show_advanced_settings"],
        "why": "exposes the 17 Advanced HFD Parameters",
    },
    "Time & Granular/Magnetic_Tape_Degradation.praat": {
        "lock_on": ["Show_advanced_settings"],
        "why": "exposes the 15 Advanced Tape Physics parameters",
    },
    # Four that the a7f9583 bump reshaped into the same `boolean Edit_… 0` + `beginPause` shape
    # the Generative rewrite gave twelve scripts above. All four were working catalog entries
    # before that commit and were excluded as `gui_blocking` the moment it landed, so these
    # entries are what keep the bump from being a net capability loss rather than new reach.
    # Each is `lock_on`ed for the reason recorded at the top of this table: with the dialog
    # replaced by assignments the toggle no longer shows anything, it only decides whether the
    # values apply — and the dialog is now where they are set.
    "Spectral/Basic_Mirror.praat": {
        "lock_on": ["Edit_details"],
        "why": "second settings page (Spectral Mirror - Details)",
    },
    # Eight of its fourteen pause fields carry a label whose Praat-derived variable the script
    # reads nowhere — "Max frequency (Hz)" binds `max_frequency` where the script uses
    # `max_frequency_Hz` — so the page is inert in stock Praat as much as here. No table entry
    # is needed for that: every one passes its own variable as the field's default, which is
    # exactly the shape `rewrite::default_expression_variable` infers from.
    "Spectral/Beltrami_Inspired_Spectral_Melter.praat": {
        "lock_on": ["Edit_details"],
        "why": "second settings page (14 diffusion/ridge details, shown after preset loading)",
    },
    "Spectral/Self-Similarity_Spectral_Resynthesis.praat": {
        "lock_on": ["Edit_details"],
        "why": "second settings page (Self-Similarity Resynthesis - Details)",
    },
    "Analysis/Speech_to_MusicXML_Rhythm_Converter.praat": {
        "lock_on": ["Edit_advanced_settings"],
        "why": "second settings page (Speech Rhythm - Advanced settings)",
    },
    # No `form` at all -- the whole UI is a two-stage wizard. Step 1 picks an algorithm, then
    # exactly one of nine settings blocks runs, each behind `if algorithm$ = "..."`.
    "Reverb/Universal Convolution Generator.praat": {
        "why": "two-stage pause wizard; the script declares no form at all",
        "split_on": "Algorithm",
        "variants": {
            "Accelerando": "Settings: Accelerando",
            "Bouncing Ball": "Settings: Bouncing Ball",
            "Bursts and Taps": "Settings: Bursts & Taps",
            "Euclidean Rhythm": "Settings: Euclidean",
            "Fibonacci (Mono)": "Settings: Fibonacci",
            "Golden Angle Drift": "Settings: Golden Angle",
            "Random Walk": "Settings: Random Walk",
            "Stereo Fibonacci": "Settings: Stereo Fibonacci",
            "Swing": "Settings: Swing",
        },
    },
    # Eleven of a shape, arriving together in the 2026-08-22 bump, which reworked 221 scripts
    # across every category. Each grew a `boolean Advanced_settings 0` in its form guarding a
    # single `if advanced_settings` / `beginPause ... endPause` page of the settings that matter
    # most -- the compressor knee and lookahead, the reverb's early-reflection geometry, the
    # distortion band splits. Under `--run` that page segfaults praat, so all eleven fell out of
    # the catalog on the first regeneration after the bump.
    #
    # Same treatment and same reasoning as the `Show_advanced_settings` trio above: the page is
    # gated, so the toggle is locked **on** or the hoisted assignments sit inside an `if` that
    # never runs. Nothing is smuggled in by that -- with the dialog replaced by assignments the
    # toggle no longer shows anything, it only decides whether the advanced values apply, and
    # the dialog is now where they are set.
    #
    # `Artificial_Room`'s guard is `if advanced_settings or room_preset = 5`, so locking the
    # toggle makes it true whatever the preset -- which is what is wanted: the page is where its
    # room geometry lives, and reaching it only on preset 5 would hide those fields on the other
    # four. It is also the one whose block ends `endPause: "Run", 1` rather than "Continue".
    "Distortion/Multiband_Distortion.praat": {
        "lock_on": ["Advanced_settings"],
        "why": "Advanced settings page (band splits, per-band drive)",
    },
    "Distortion/Virtual_Subharmonic_Generator.praat": {
        "lock_on": ["Advanced_settings"],
        "why": "Advanced settings page (tracking and division detail)",
    },
    "Dynamics & Envelope/Compressor.praat": {
        "lock_on": ["Advanced_settings"],
        "why": "Advanced settings page (knee, lookahead, detector)",
    },
    "Dynamics & Envelope/Intensity_Envelope_Processor.praat": {
        "lock_on": ["Advanced_settings"],
        "why": "Advanced settings page (envelope extraction detail)",
    },
    "Dynamics & Envelope/Time-domain_RMS_envelope_follower.praat": {
        "lock_on": ["Advanced_settings"],
        "why": "Advanced settings page (window and smoothing detail)",
    },
    "Dynamics & Envelope/Vintage_Glue_Compressor.praat": {
        "lock_on": ["Advanced_settings"],
        "why": "Advanced settings page (glue character, sidechain)",
    },
    "Modulation/Hexaphonic Serial Audio Processor.praat": {
        "lock_on": ["Advanced_settings"],
        "why": "Advanced settings page (per-string processing detail)",
    },
    "Modulation/Phonetic_Tremolo_Glitch_Effect.praat": {
        "lock_on": ["Advanced_settings"],
        "why": "Advanced settings page (glitch rate and gate detail)",
    },
    "Modulation/Spectral-Driven Intensity Modulation.praat": {
        "lock_on": ["Advanced_settings"],
        "why": "Advanced settings page (spectral driver detail)",
    },
    "Pitch/Fractal_Pitch_Terrain.praat": {
        "lock_on": ["Advanced_settings"],
        "why": "Advanced settings page (fractal depth and terrain shaping)",
    },
    "Reverb/Artificial_Room.praat": {
        "lock_on": ["Advanced_settings"],
        "why": "Room details / Advanced page (early reflections, geometry)",
    },
    # The same bump's other shape: a second `form ... endform` rather than a `beginPause`, which
    # Praat allows even less (one form per run) and which the `gui_blocking` detector does not
    # see, so these three stayed in the catalog quietly missing their advanced fields instead of
    # being excluded. Hoisted through the identical machinery -- see `SECOND_FORM_RE`.
    #
    # Each preserves its pre-bump behaviour by assigning every advanced default as a plain
    # variable immediately above the guard ("Advanced defaults preserve the original v1.0
    # behavior"), which is what makes the hoist verifiable by inspection: those assignments and
    # the second form's own defaults agree, so a run with the dialog untouched is the run
    # upstream intended.
    "Filter & Color/Dynamic_Formant_Sweeper.praat": {
        "lock_on": ["Advanced_settings"],
        "why": "second form (formant analysis detail, fades, output level)",
    },
    "Filter & Color/Adaptive_Spectral_Resonance_Suppressor.praat": {
        "lock_on": ["Advanced_settings"],
        "why": "second form (analysis window, smoothing, band layout)",
    },
    "Filter & Color/Jitter-Shimmer_Formant_Mapping.praat": {
        "lock_on": ["Advanced_settings"],
        "why": "second form (manual pitch range, formant ceiling, output level)",
    },
    # Arrived 2026-08-24 with *two* pause pages, one per toggle, holding everything the five
    # texture presets otherwise choose for you -- the wavelet analysis grid on one, the grain
    # engine and output on the other. Its own `Custom` preset (option 5) already sets both
    # toggles to 1, so locking them on is what that preset does by hand; the other four presets
    # assign every hoisted variable themselves just above the guards, which is what makes the
    # rewrite verifiable by inspection.
    #
    # Both blocks end `endPause: "Cancel", "Continue", 2, 1` and `exitScript` on button 1, so the
    # captured default button (2) is load-bearing rather than cosmetic: assuming button 1 would
    # turn every run into a silent no-op. Same trap as `Polyphonic_Improviser`.
    "AI & Adaptive/CWT_Granular_Resampler.praat": {
        "lock_on": ["Edit_analysis_settings", "Edit_engine_settings"],
        "why": "two settings pages (wavelet analysis/triggers, then grain engine/output)",
    },
}

# Scripts that ask for a folder with `chooseDirectory$`, hoisted into a `ParamKind::FolderPath`
# param. Keyed by path relative to the plugin root.
#
# The same treatment `PAUSE_HOISTS` gives a `beginPause` block, and needed for the same reason: a
# modal under `--run` does not merely block, it segfaults. It is the *better* answer here than an
# exclusion, because this app can already answer the question the dialog asks -- a `FolderPath`
# param has a real folder picker, and `cdp_validate_fields` blocks Apply until one is chosen, so
# the value can never arrive empty.
#
#   vars  -- (variable as the script spells it, `$` included) -> the parameter label it becomes.
#            An ordered dict, so a script choosing two folders gets its two params in that order.
#
# Every variable must be assigned by a `chooseDirectory$` call in the script, checked below: a
# stale one means upstream moved the thing this points at, and shipping the entry anyway would
# ship a script that still opens the modal.
#
# Note what is *not* here. Four other scripts call `chooseDirectory$` only as a fallback for a
# blank folder field (`if dir$ == ""`), and their fields are already `FolderPath` -- unreachable,
# so they take a `GUI_BLOCKING_OVERRIDES` entry instead. Two more are excluded for reasons this
# does not touch, and `Wave_Gesture_Path_Performer` has live `beginPause` blocks besides.
DIRECTORY_HOISTS: dict[str, dict] = {
    # The one that already ships, and the reason this was worth building rather than adding
    # `chooseDirectory$` to the `gui_blocking` regex and moving on: the call sits in the `else`
    # of `optionmenu GEN_mode`, so the entry works in its default Neighbor-GEN mode and
    # segfaults the moment anyone picks Pair-corpus. A latent crash, not a missing feature.
    #
    # The folder is therefore required on *every* run, including the mode that never reads it.
    # Accepted deliberately: an unpicked `FolderPath` blocks Apply and there is no per-param
    # "may be empty", and inventing one would re-open the very hole this closes -- a blank field
    # reaches the chooser.
    "Analysis/OT_Grammar_Learning_from_Audio.praat": {
        "vars": {"pairRoot$": "Pair_corpus_folder"},
        "why": "Pair-corpus GEN mode picks a folder holding good/ and bad/ subfolders",
    },
    # Unconditional, on the main path -- every run reached it. Excluded by hand until now.
    # Also a `py`-group script, so it is the first to need the interpreter rewrite and a dialog
    # rewrite at once; `praat::runner` applies both passes to one copy.
    "py/Semantic_timbre_retrieval.praat": {
        "vars": {"corpusDir$": "Corpus_folder"},
        "why": "picks the corpus to retrieve from; unconditional, on the main path",
    },
}

# Parameters whose value must never be left on *by default*: `Play` blocks for the audio's
# real-time duration and cannot be suppressed from outside the process, and drawing costs time
# nobody asked for. Matched on the parameter name; the toggle stays visible and the user can
# turn it back on.
#
# Keep in step with `model::praat::plan::is_picture_toggle_name`, which is this pattern minus
# `play|demo|open_|export` and decides which toggles mean "capture Praat's Picture window and
# show it". A prefix added here needs a decision there about whether it draws.
SILENCE_RE = re.compile(r"^(play|draw|show|visuali|demo|open_|export)", re.I)

# --- static exclusion detectors -------------------------------------------------------
# Each maps a reason slug to a pattern that, if found anywhere in a script, disqualifies it.
#
# They are matched against `code_only(source)`, NOT the raw text -- see that function for why.


def code_only(source: str) -> str:
    """Blank out comment bodies and string contents, keeping every other byte in place.

    The exclusion patterns below describe *constructs the script executes*, so matching them
    against raw text reads prose as code. That was not hypothetical: five scripts were
    excluded as `gui_blocking` on the strength of a comment or a log message, among them
    `Dramaturgical_Structure_Composer`, whose matched line reads

        # - FORM: beginPause second-dialog removed. All settings

    -- i.e. it was excluded for saying it had removed the thing it was excluded for. The
    others matched `demo ` inside strings like "Generating melody demo (major arpeggio)...".

    Replaces with spaces rather than deleting, so offsets, line numbers and the `^`/`$`
    anchors in the patterns all still mean what they did. Handles Praat's `#` line comments
    and its double-quoted strings, whose only escape is a doubled quote (there is no
    backslash escaping), so `""` inside a string is two blanked characters and not a
    terminator. An unterminated string recovers at the newline instead of swallowing the
    rest of the file. `;`/`!` comments are deliberately left alone: no script in this
    plugin uses them, and blanking more than necessary risks letting a genuinely blocking
    script through, which is the expensive direction to be wrong in.
    """
    out: list[str] = []
    in_string = False
    i, n = 0, len(source)
    while i < n:
        c = source[i]
        if in_string:
            if c == '"':
                if i + 1 < n and source[i + 1] == '"':
                    out.append("  ")
                    i += 2
                    continue
                in_string = False
                out.append('"')
            elif c == "\n":
                in_string = False
                out.append(c)
            else:
                out.append(" ")
            i += 1
        elif c == '"':
            in_string = True
            out.append(c)
            i += 1
        elif c == "#":
            end = source.find("\n", i)
            end = n if end == -1 else end
            out.append(" " * (end - i))
            i = end
        else:
            out.append(c)
            i += 1
    return "".join(out)


# Whether a detector is matched against `code_only(source)` or the raw text. Not a detail:
# `gui_blocking` looks for constructs the script *executes*, so it must not read prose --
# while `hardcoded_path` and `non_sound_input` look for the contents of string *literals*
# (a path in a `Read from file:` argument, an `exitScript: "Please select a Photo object"`),
# which is precisely what `code_only` blanks. Running all three over `code_only` silently
# disarmed those two: the regenerated exclusion list lost `hardcoded_path` and
# `non_sound_input` entirely, and the seven scripts they had been catching fell through to
# whatever detector happened to fire next.
CODE = "code"    # match against code_only(source)
RAW = "raw"      # match against the source as written

# `<var>$ = chooseDirectory$` as a *statement*. Matched against `code_only`, so a script that
# discusses the call in its changelog -- `CorpusMap` does, twice -- is not read as making it.
# All three spellings the plugin uses (`: "x"`, `("x")`, ` ("x")`) share this prefix; the Rust
# side re-derives the same set in `rewrite::directory_chooser_target`.
DIRECTORY_CHOOSER_RE = re.compile(r"^[ \t]*(\w+\$)[ \t]*=[ \t]*chooseDirectory\$", re.M)


def build_directory_params(source: str, hoist: dict) -> list | str:
    """The `folder_path` params a directory-hoisted script gains, or a problem string.

    Appended *after* the form's own params, the order `build_hoisted_processes` already uses: the
    dialog reads as everything the form asked, then everything the chooser did.
    """
    assigned = set(DIRECTORY_CHOOSER_RE.findall(code_only(source)))
    out = []
    for variable, label in hoist["vars"].items():
        if variable not in assigned:
            return (f"{variable} is not assigned by a chooseDirectory$ call "
                    f"(upstream renamed or removed it?)")
        out.append(Param(name=label, kind="folder_path", default=0.0, directory_var=variable))
    return out


def build_hoisted_processes(rel, top: str, stem: str, source: str, form_params: list,
                            hoist: dict, inputs: int, description: str = ""):
    """Turn one pause-hoisted script into the catalog entries it should become.

    Returns a list of `Process`, or a problem string. Every name in the hoist config must
    resolve: a stale one means upstream moved the thing the config points at, and shipping the
    entry anyway would ship a script that still opens a dialog and segfaults.

    Hoisted params are appended *after* the form's own, so the dialog reads in the order the
    two Praat windows would have: everything the first asked, then everything the second did.
    """
    blocks = find_pause_blocks(source)
    if not blocks:
        return "no beginPause block found (upstream may have removed it)"

    # A block guarded by `if show_advanced_settings` only runs when that toggle is true, and the
    # assignments the rewrite emits sit *inside* the guard — so the toggle has to be forced on
    # or the hoisted values are silently ignored. Locked, because with the dialog gone it no
    # longer shows anything: it only decides whether the fields below take effect.
    form_locks: list[tuple[str, bool]] = []
    for name in hoist.get("lock_on", []):
        param = next((p for p in form_params if p.name == name), None)
        if param is None:
            return f"no parameter named {name!r} to lock on (upstream renamed or removed it?)"
        if param.kind != "toggle":
            return f"parameter {name!r} is a {param.kind}, not a toggle"
        # Dropped from the catalog entirely and assigned in the rewritten script instead. A
        # switch that gates parameters now shown in the same dialog is not a setting any more:
        # it cannot be turned off without those parameters silently ceasing to apply, and
        # turning it on reveals nothing. Shown as a locked checkbox it read as a broken control.
        # Its notes are not the toggle's own property -- a `=== Output ===` heading declared
        # above it introduces the fields that follow, and would vanish with it -- so they move
        # onto whatever field now leads the section.
        dropped = form_params.index(param)
        carried = param.notes + param.notes_after
        form_params = [p for p in form_params if p is not param]
        if carried:
            if dropped < len(form_params):
                form_params[dropped].notes[:0] = carried
            elif form_params:
                form_params[-1].notes_after.extend(carried)
        form_locks.append((name, True))
    for i, block in enumerate(blocks):
        if isinstance(block["fields"], str):
            return f"block {i} ({block['title']!r}): {block['fields']}"

    def tag(fields, index):
        out = []
        for param in fields:
            copied = replace(param)
            copied.pause_block = index
            # A hoisted `Play`/`Draw` toggle is the same hazard as a form one -- `Play` blocks
            # for the audio's real duration -- so it gets the same forced-off default.
            if copied.kind == "toggle" and SILENCE_RE.match(copied.name):
                copied.default = False
            out.append(copied)
        return out

    split_on = hoist.get("split_on")
    if not split_on:
        params = list(form_params)
        for i, block in enumerate(blocks):
            params.extend(tag(block["fields"], i))
        return [Process(key=key_for(top, stem), bin=str(rel).replace("\\", "/"),
                        title=title_for(stem), group=GROUP_DIRS[top], params=params,
                        inputs=inputs, description=description,
                        short_description=short_description_from(description, title_for(stem)),
                        # Same rule as the generic path below -- a pause-hoisted script is still
                        # whatever kind of process it was, and omitting this is how
                        # `FM_Texture_Generator` alone stayed `wav` when the folder went
                        # zero-input.
                        input_kind=zero_or_photo_input_kind(str(rel).replace("\\", "/")),
                        form_locks=form_locks)]

    # --- split: one entry per option of a hoisted optionmenu ------------------------------
    variants = hoist.get("variants", {})
    by_title = {b["title"]: (i, b) for i, b in enumerate(blocks)}
    selector = None
    selector_block = None
    for i, block in enumerate(blocks):
        for param in block["fields"]:
            if param.name == split_on:
                selector, selector_block = param, i
    if selector is None:
        return f"no hoisted parameter named {split_on!r} to split on"
    if selector.kind != "choice":
        return f"{split_on!r} is a {selector.kind}, not an optionmenu"
    for label in variants:
        if label not in selector.options:
            return f"{split_on!r} has no option {label!r} (upstream reworded it?)"
    for label, title in variants.items():
        if title not in by_title:
            return f"no pause block titled {title!r} for option {label!r}"
    missing = [o for o in selector.options if o not in variants]
    if missing:
        return f"{split_on!r} options with no block mapped: {missing}"

    # Blocks that belong to no variant are shared by every entry -- the wizard's first page.
    variant_block_indices = {by_title[t][0] for t in variants.values()}
    built = []
    for label in selector.options:
        params = list(form_params)
        for i, block in enumerate(blocks):
            if i in variant_block_indices and i != by_title[variants[label]][0]:
                continue
            tagged = tag(block["fields"], i)
            if i == selector_block:
                # The selector survives as a one-option choice rather than being dropped: the
                # rewrite reads it to assign `algorithm$`, which is what every one of the
                # script's own `if algorithm$ = "..."` guards tests. A locked field also says
                # plainly that this entry *is* that algorithm.
                for param in tagged:
                    if param.name == split_on:
                        param.options = [label]
                        param.default = 0
            params.extend(tagged)
        built.append(Process(
            key=f"{key_for(top, stem)}_{slugify(label)}",
            bin=str(rel).replace("\\", "/"),
            title=f"{title_for(stem)} ({label})",
            group=GROUP_DIRS[top],
            params=params,
            inputs=inputs,
            description=description,
            # Names the variant, since nine entries otherwise share one description.
            short_description=f"{label} — "
                              + short_description_from(description, title_for(stem)),
            input_kind=zero_or_photo_input_kind(str(rel).replace("\\", "/")),
        ))
    return built


def slugify(text: str) -> str:
    return re.sub(r"[^a-z0-9]+", "_", text.lower()).strip("_")


def apply_gui_override(params: list, override: dict) -> str | None:
    """Apply one `GUI_BLOCKING_OVERRIDES` entry to a parsed form. Returns a problem, or None.

    Every name in the override must resolve. A silent miss would leave the blocking construct
    reachable while the entry claims otherwise, which is the one failure mode that costs a
    segfault rather than an error message -- so a stale override excludes the script instead.
    """
    by_name = {p.name: p for p in params}

    for name, labels in override.get("drop_options", {}).items():
        param = by_name.get(name)
        if param is None:
            return f"no parameter named {name!r} (upstream renamed or removed it?)"
        if param.kind != "choice":
            return f"parameter {name!r} is a {param.kind}, not an optionmenu"
        for label in labels:
            if label not in param.options:
                return f"{name!r} has no option {label!r} (upstream reworded it?)"
        kept = [o for o in param.options if o not in labels]
        if not kept:
            return f"dropping every option of {name!r} would leave nothing to pick"
        # The default is an *index* into the old list, so it has to be re-resolved against the
        # new one rather than carried across. A default that was itself dropped falls back to
        # the first survivor.
        previous = param.options[int(param.default)] if int(param.default) < len(param.options) else None
        param.options = kept
        param.default = kept.index(previous) if previous in kept else 0

    return None


EXCLUSIONS: list[tuple[str, re.Pattern, str, str]] = [
    (
        "gui_blocking",
        # `View & Edit` and a bare `Edit` open an editor window, which batch mode refuses
        # outright ("Cannot edit a TextGrid from batch."). Anchored so the word `Edit` inside a
        # comment or a longer command name does not match.
        #
        # `demo` is anchored to *statement position* rather than matched as a bare word.
        # Praat's `demo` is a command prefix (`demo Erase all`), so it only ever starts a
        # statement; `\bdemo\s` also matched the English word, which is how three synthesis
        # generators came to be excluded over their own log messages. `demoShow` and
        # `demoWaitForInput` are still matched by name, wherever they appear.
        re.compile(r"\b(beginPause|pauseScript|demoShow|demoWaitForInput|chooseReadFile\$|"
                   r"chooseWriteFile\$|chooseFolder\$)|^\s*demo\s|"
                   r"^\s*(View & Edit|Edit)\s*$", re.M),
        "uses an interactive/GUI construct that segfaults or hangs under --run",
        CODE,
    ),
    (
        # A folder chooser is exactly as fatal as a `beginPause` -- a modal under `--run`
        # segfaults -- but it gets a slug of its own rather than joining the regex above, and the
        # separation is what keeps the two exemptions honest. A `DIRECTORY_HOISTS` entry rewrites
        # this call out of the copy that runs and says nothing about anything else, so a script
        # that *also* opens an editor window is still caught by `gui_blocking`. Folded into one
        # slug, hoisting one construct would have excused the other.
        #
        # Listed after `gui_blocking` so a script tripping both reports the harder obstacle: the
        # first match wins, and a live `beginPause` is not something a folder param answers.
        "folder_chooser",
        re.compile(r"\bchooseDirectory\$", re.M),
        "asks for a folder with chooseDirectory$, a modal that cannot be answered under --run",
        CODE,
    ),
    (
        "non_sound_input",
        re.compile(r"select a (Photo|TextGrid|Table|Matrix) object|"
                   r"Please select .*(Photo|TextGrid)", re.I),
        "operates on a non-Sound object",
        RAW,
    ),
    (
        "hardcoded_path",
        re.compile(r"\"[A-Za-z]:[\\/]|/home/[a-z]+/|\.praat-dir"),
        "contains a hardcoded absolute path that only resolves on its author's machine",
        RAW,
    ),
]

# Scripts that want two Sound objects selected rather than one -- morphing, concatenative
# synthesis, DTW alignment, pitch-contour transfer. They read them positionally, as
# `selected("Sound", 1)` and `selected("Sound", 2)`, which the driver satisfies by reading both
# inputs in order and selecting them together. Catalogued as `input = "dual_wav"`, reusing the
# CDP kind that already means "this process needs a second buffer".
#
# Detected from an **unindented** guard, which is what separates a script that always needs two
# from one that needs two only in a particular mode. Both write the same check; the conditional
# one writes it inside an `if`, so it is indented:
#
#     nSelected = numberOfSelected("Sound")        numSounds = numberOfSelected("Sound")
#     if nSelected <> 2                            if mod_source = 5
#         exitScript: "...select exactly 2..."         if numSounds <> 2
#     endif                                                exitScript: "...exactly 2..."
#     ^ always needs two                               ^ needs two only in mode 5
#
# Matching the prose instead ("select 2 sounds") was wrong in the other direction: that phrase
# appears in `comment` lines and `option` labels of the conditional ones, which then failed at
# run time with `Please select exactly ONE Sound object.`
COUNT_ASSIGN_RE = re.compile(
    r"^[ \t]*(\w+)\s*=\s*numberOfSelected\s*\(\s*\"Sound\"\s*\)", re.M
)


def needs_two_sounds(source: str) -> bool:
    # The direct form, with no intermediate variable.
    if re.search(r"^if\s+numberOfSelected\s*\(\s*\"Sound\"\s*\)\s*(<>|!=|<)\s*2\b", source, re.M):
        return True
    for var in set(COUNT_ASSIGN_RE.findall(source)):
        if re.search(rf"^if\s+{re.escape(var)}\s*(<>|!=|<)\s*2\b", source, re.M):
            return True
    return False

# A script's own presets live *inside* it, as an `if preset = 2 ... elsif preset = 3 ...` chain
# that overwrites the other form variables:
#
#     elsif preset = 2
#         presetName$ = "WarmTape"
#         drive = 1.5
#         hysteresis_Memory = 0.25
#
# So choosing a preset already changes the sound. What it did *not* do is tell the dialog, which
# went on showing the manual values -- the user could neither see what a preset had chosen nor
# adjust it. Reading the chain back out lets the form fill those fields in and then switch the
# preset menu to its own Custom entry, so what runs is exactly what is displayed.
PRESET_NAME_RE = re.compile(r"^preset", re.I)
CUSTOM_OPTION_RE = re.compile(r"custom|manual|none|user", re.I)
ASSIGNMENT_RE = re.compile(r"^\s*([A-Za-z_]\w*)\s*=\s*(-?\d+(?:\.\d+)?)\s*$")


# A unit or range in parentheses at the end of a form label: `(%)`, `(Hz)`, `(0-1)`,
# `(0_=_original)`. Praat drops it when deriving the variable name, along with the `_` before it.
UNIT_SUFFIX_RE = re.compile(r"_?\([^)]*\)\s*$")


def praat_variable(label: str) -> str:
    """The script variable Praat derives from a form label.

    Two rules, and only the first is obvious. The first letter is lowercased, so
    `Hysteresis_Memory` is read back as `hysteresis_Memory` -- note that *only* the first letter
    moves. And a trailing unit or range in parentheses is **dropped**: `real Lock_strength_(%) 35`
    declares `lock_strength`, which is what `Harmonic_Formant_Locking` goes on to read.

    The second rule is not a guess. Across every catalog param whose label carries a
    parenthetical, the stripped name is the one its own script uses -- 92 of 92, the raw name
    never -- and `no_form_label_derives_a_variable_its_script_never_reads` keeps it that way.

    Getting it wrong was silent rather than fatal, because nothing passes *arguments* by name:
    Praat fills a form positionally. What it broke was everything that matches a label back to
    the script's own code. `extract_script_presets` could not see `lock_strength = 20` inside a
    preset branch, so 24 processes shipped preset tables listing only the fields whose labels
    happened to carry no unit -- picking "Strong Metal (85%)" moved `Max_shape_dB` and left the
    strength field sitting at a value the run would not use.
    """
    label = UNIT_SUFFIX_RE.sub("", label)
    return label[:1].lower() + label[1:] if label else label


def extract_script_presets(source: str, params: list[Param]):
    """Return `(preset_index, custom_option, {option_index: {param_index: value}})`, or None.

    Conservative by construction: a chain that does not parse simply yields nothing and the
    process keeps today's behaviour, rather than the form being filled with values that might
    not match what the script actually does.
    """
    preset_index = next(
        (i for i, p in enumerate(params) if p.kind == "choice" and PRESET_NAME_RE.match(p.name)),
        None,
    )
    if preset_index is None:
        return None

    variable = praat_variable(params[preset_index].name)
    by_variable = {praat_variable(p.name): i for i, p in enumerate(params)}
    # Only whole-line `if`/`elsif` comparisons against a literal, so a compound condition
    # (`if preset = 2 and stereo`) is skipped rather than half-understood.
    head = re.compile(rf"^(?:els)?if\s+{re.escape(variable)}\s*=\s*(\d+)\s*$")

    blocks: dict[int, dict[int, float]] = {}
    current: int | None = None
    for line in source.split("\n"):
        match = head.match(line)
        if match:
            current = int(match.group(1))
            blocks.setdefault(current, {})
            continue
        if current is None:
            continue
        stripped = line.strip()
        if stripped.startswith(("else", "endif")) and not stripped.startswith("elsif"):
            current = None
            continue
        assignment = ASSIGNMENT_RE.match(line)
        if assignment and assignment.group(1) in by_variable:
            target = by_variable[assignment.group(1)]
            # A preset that reassigns the preset menu itself would fight the Custom switch.
            if target != preset_index:
                blocks[current][target] = float(assignment.group(2))

    blocks = {option: values for option, values in blocks.items() if values}
    if not blocks:
        return None

    options = params[preset_index].options
    custom = next(
        (i for i, label in enumerate(options) if CUSTOM_OPTION_RE.search(label)),
        0,
    )
    return preset_index, custom, blocks


NUMERIC_KEYWORDS = {"real", "positive", "integer", "natural"}
TEXT_KEYWORDS = {"sentence", "word", "text"}
CHOICE_KEYWORDS = {"optionmenu", "choice"}
# Declared in a form but meaningless as an *argument* -- a `comment` carries no value. It is
# not meaningless to the user, though: see `classify_comment`, which turns it into a catalog
# note rather than dropping it.
IGNORED_KEYWORDS = {"comment"}

# Characters a script uses purely to decorate a section heading (`=== Preset ===`,
# `--- Output ---`, `── Mode ──────`). Stripping them from both ends is also how a heading is
# *recognised*: text that loses characters to this strip was decorated, and therefore a heading.
# Deliberately excludes `.` -- a prose note ending in a full stop would otherwise be promoted to
# a heading -- and excludes `(`/`)`, since a parenthesised note is prose by definition.
HEADING_DECORATION = "=-–—─═━_~*# \t"
# A heading that only names the preset picker. tui-wave already puts a `Preset` row at the top
# of every params dialog, immediately above the script's own `Internal Preset` field, so a
# heading between the two labels the thing it sits under twice.
PRESET_HEADING_RE = re.compile(r"presets?", re.I)
# Praat's standing instruction to select a Sound in the object list before pressing Run. There
# is no object list here -- tui-wave hands the process the current selection -- so the line is
# not merely redundant but wrong. Only the bare instruction is dropped: `Select a Sound object
# - it CONTROLS the feedback circuit` says something real and is kept.
SELECT_SOUND_RE = re.compile(r"select (?:a|an|the) sound(?: object)?(?: first)?[.!]?", re.I)


@dataclass
class Note:
    """One `comment` line from a form, kept for display in the params dialog."""
    text: str
    section: bool


def classify_comment(text: str, colon_form: bool) -> Note | None:
    """Turn a `comment` line's operand into a `Note`, or None if it carries no information."""
    body = text.strip()
    # The modern `comment: "..."` syntax quotes its operand; the classic `comment ...` does not.
    if colon_form and len(body) >= 2 and body[0] == '"' and body[-1] == '"':
        body = body[1:-1].strip()
    bare = body.strip(HEADING_DECORATION)
    # Nothing but decoration: a horizontal rule with no title, or an empty `comment`. The
    # dialog draws its own rule under every heading, so a free-standing one adds nothing.
    if not bare:
        return None
    if SELECT_SOUND_RE.fullmatch(bare):
        return None
    if bare != body:
        # The script decorated it, so it is a heading.
        return None if PRESET_HEADING_RE.fullmatch(bare) else Note(bare, True)
    # `comment Output:` -- undecorated, but a trailing colon on a short label is a heading too.
    if body.endswith(":") and len(body) > 1:
        title = body[:-1].strip()
        if title:
            return None if PRESET_HEADING_RE.fullmatch(title) else Note(title, True)
    return Note(body, False)


@dataclass
class Param:
    name: str
    kind: str  # number | toggle | choice
    default: float | bool | int
    options: list[str] = field(default_factory=list)
    integer: bool = False
    minimum: float = 0.0
    maximum: float = 0.0
    step: float = 0.0
    # Set for kind == "number_list": the script's own delimiter, written back verbatim, and
    # the default entries parsed out of its form declaration.
    list_separator: str = ""
    list_default: list[float] = field(default_factory=list)
    # kind == "text": the script's own declared default, verbatim.
    text_default: str = ""
    # Set for a number split out of a `key=value` field: which field it rejoins, and under which
    # key. See KEY_VALUE_RE.
    key_value_group: str = ""
    key_value_key: str = ""
    # Index of the `beginPause` block this param was hoisted out of, in the *original* script's
    # source order. None for an ordinary `form` param, which travels as a runScript: argument.
    pause_block: int | None = None
    # The `$`-variable a `chooseDirectory$` call assigns, for a param hoisted out of that call.
    # Like `pause_block`, it means "not a runScript: argument" -- the script asked for this with
    # a dialog, so its form has no slot for it. See DIRECTORY_HOISTS.
    directory_var: str = ""
    # `comment` lines the form placed immediately *before* this field, in source order, and --
    # for the last field of a body only -- the ones placed after it. See `note_rows`, which
    # flattens both into the (index, note) pairs the catalog stores; keeping "after" separate
    # until then is what lets a trailing note stay attached to its own field when a hoisted
    # pause block appends more params behind it.
    notes: list[Note] = field(default_factory=list)
    notes_after: list[Note] = field(default_factory=list)


@dataclass
class Process:
    key: str
    bin: str
    title: str
    group: str
    params: list[Param]
    inputs: int = 1
    # Opens its own window and waits: the runner runs it unbounded. See PY_INTERACTIVE_IMPORTS.
    interactive: bool = False
    # `boolean` form fields deleted from the script and assigned instead — see `lock_on`.
    form_locks: list = field(default_factory=list)
    # Read out of the script's own `# Description:` header block -- see `extract_description`.
    # Empty for the handful of scripts that carry no such header; the emitter falls back to the
    # title, which is what every entry used to get.
    description: str = ""
    short_description: str = ""
    preset_param: int | None = None
    preset_custom_option: int = 0
    # option index -> {param index: value}
    script_presets: dict[int, dict[int, float]] = field(default_factory=dict)
    # Ships with tui-wave rather than living in the submodule -- see BUILTINS below and
    # `src/model/praat/builtin.rs`. Emits `praat_builtin = true`, which is what tells the
    # planner not to resolve `bin` against the plugin directory.
    builtin: bool = False
    # `"none"` for a process that creates its Sound instead of transforming one.
    input_kind: str = ""
    # The script assigns a Python interpreter somewhere -- see `counts_python_assignments`.
    # Emits `praat_python_rewrite = true`, which tells the app to run a copy of the script with
    # every one of those assignments repointed at the interpreter it chose.
    python_rewrite: bool = False


def unquote(text: str, colon_form: bool) -> str:
    """Strip Praat string quoting, but *only* for the colon syntax.

    The plugin mixes two syntaxes and they differ in exactly this: the modern
    `option: "Normal (1.0)"` names an option `Normal (1.0)`, while the classic
    `option "Normal (1.0)"` takes everything after the keyword **literally**, quotes included,
    and so names an option whose text really is `"Normal (1.0)"`.

    Unquoting both was wrong in each direction in turn. Leaving the colon form quoted was the
    largest source of failures in the research probe; stripping the classic form's quotes then
    broke the handful of scripts that write them, with `Unknown value "Normal (1.0)" for option
    menu "Preset"` -- the same message, from the opposite mistake. The keyword's own trailing
    colon is the only thing that distinguishes them.
    """
    text = text.strip()
    if colon_form and len(text) >= 2 and text[0] == '"' and text[-1] == '"':
        text = text[1:-1]
    return text.strip()


def read_script(path: Path) -> str:
    # The plugin's files are CRLF; newline="" would leave \r on every token.
    return path.read_text(encoding="utf-8", errors="replace").replace("\r\n", "\n")


FORM_RE = re.compile(r"^[ \t]*form\b[^\n]*\n(.*?)^[ \t]*endform", re.S | re.M)


# A `beginPause` block. Praat allows only ONE `form` per script run, so an author needing a
# second page of settings has to use a pause dialog instead -- which is precisely why these
# scripts were unusable headlessly (`beginPause` under `--run` segfaults). The fields inside
# are declared with the same syntax as a form's, which is why `parse_fields` serves both.
#
# The `endPause:` line's first *numeric* argument is the dialog's default button, captured so
# a rewrite can behave as though the user pressed it. That matters: `Polyphonic_Improviser`
# ends `endPause: "Cancel", "OK", 2, 1` and then exits the script on `clicked = 1`, so
# assuming button 1 would silently turn every run into a no-op.
PAUSE_RE = re.compile(
    r"^[ \t]*beginPause:[ \t]*(?P<title>\"[^\"]*\"|[^\n]*)\n"
    r"(?P<body>.*?)"
    # The assignment target allows a leading `.`: Praat's procedure-local variables are spelled
    # `.clicked`, and `\w+` alone silently failed to match `.clicked = endPause: …` — the block
    # was then not found at all, so the script fell through to `gui_blocking` and was excluded
    # with "no beginPause block found" even though it plainly has one
    # (`AI & Adaptive/CWT_Granular_Resampler.praat`, 2026-08-24). The Rust matcher splits on `=`
    # and takes the left side verbatim, so it never had this gap; this is the two sides agreeing
    # again.
    r"^[ \t]*(?:[.\w]+[ \t]*=[ \t]*)?endPause:(?P<tail>[^\n]*)$",
    re.S | re.M,
)


# A **second `form ... endform`**, which is the same idea spelled differently and just as fatal.
# Praat allows one form per script run, so an author reaching for a second one has written
# something that cannot work -- and unlike `beginPause` it does not segfault, it simply opens a
# dialog and waits, or is skipped entirely when its `if` guard is false. Upstream shipped three
# of these in the 2026-08-22 bump (`Dynamic_Formant_Sweeper`,
# `Adaptive_Spectral_Resonance_Suppressor`, `Jitter-Shimmer_Formant_Mapping`), each guarded by
# `if advanced_settings`, each hiding between 8 and 10 parameters that had been ordinary form
# fields the day before.
#
# They are treated as pause blocks rather than as a category of their own because everything
# downstream -- tagging fields with their block index, `lock_on` forcing the guard true, the
# catalog's `praat_pause_block`, the runner's replace-with-assignments -- is identical. Only the
# delimiters differ. The **first** form is the script's own and is never a block: it is the one
# Praat fills positionally, and `apply_form_locks` edits it in place.
SECOND_FORM_RE = re.compile(
    r"^[ \t]*form[ \t:][^\n]*\n"
    r"(?P<body>.*?)"
    r"^[ \t]*endform[ \t]*$",
    re.S | re.M,
)


def find_pause_blocks(source: str) -> list[dict]:
    """Every secondary settings block -- `beginPause`/`endPause` or a second `form`/`endform`
    -- in source order.

    Each entry carries the span to replace, the dialog title (used to attach a block to a
    catalog entry -- see PAUSE_SPLITS), its parsed fields, and the default button number.
    """
    blocks = []
    # Skip the first form: it is the script's own, not a hoistable block.
    for n, m in enumerate(SECOND_FORM_RE.finditer(source)):
        if n == 0:
            continue
        title = source[m.start():source.index("\n", m.start())].strip()
        title = title.removeprefix("form").lstrip(": ").strip().strip('"')
        blocks.append({
            "title": title,
            "fields": parse_fields(m.group("body"), script_variables(source, m.start())),
            # A form has no buttons, so nothing branches on a click. 1 is inert here.
            "default_button": 1,
            "span": (m.start(), m.end()),
        })
    for m in PAUSE_RE.finditer(source):
        tail = m.group("tail")
        numbers = re.findall(r"(?<![\w.])(\d+)(?![\w.])", tail)
        blocks.append({
            "title": m.group("title").strip().strip('"'),
            "fields": parse_fields(m.group("body"), script_variables(source, m.start())),
            # No number at all would be a malformed endPause; 1 is the only sane guess and the
            # commonest real value.
            "default_button": int(numbers[0]) if numbers else 1,
            "span": (m.start(), m.end()),
        })
    # Both loops append independently, so order by position: the block index the catalog records
    # must be the one the runner arrives at counting down the file, or a hoisted field would be
    # written into the wrong block.
    blocks.sort(key=lambda b: b["span"][0])
    return blocks


# The `# Description:` block every one of these scripts carries in its header, and the only
# place any of them says what it actually does. Until this was read, every Praat entry's
# description in the browser was its own title repeated back -- `description = "chain 1"` -- so
# the description panel told the user nothing about 397 processes.
#
# Shape, consistent across the plugin: `# Description:` followed by continuation lines indented
# under the `#`, ending at the next *unindented* header (`# Changelog v0.3:`) or the `# =====`
# rule that closes the banner. The indent is what separates them -- a changelog entry like
# `#   - FIX: ...` also contains a colon, so a colon cannot be the terminator.
DESCRIPTION_START_RE = re.compile(r"^#\s*Description:\s*(.*)$")
# A comment line whose text starts immediately after `# `, i.e. a new banner heading.
DESCRIPTION_END_RE = re.compile(r"^#\s?(\S)")


def extract_description(source: str) -> str:
    """The script's own description, unwrapped into paragraphs. Empty if it has none."""
    lines = source.splitlines()
    start = None
    first = ""
    for i, line in enumerate(lines):
        match = DESCRIPTION_START_RE.match(line.strip())
        if match:
            start, first = i, match.group(1).strip()
            break
    if start is None:
        return extract_titled_description(lines)

    paragraphs: list[list[str]] = [[first]] if first else [[]]
    for line in lines[start + 1:]:
        stripped = line.rstrip()
        if not stripped.startswith("#"):
            break
        body = stripped[1:]
        if set(body.strip()) <= {"=", "-"} and body.strip():
            break                       # the banner rule that closes the header
        if not body.strip():
            paragraphs.append([])       # blank comment line: paragraph break
            continue
        if DESCRIPTION_END_RE.match(stripped):
            break                       # unindented heading: a new section
        paragraphs[-1].append(body.strip())

    # Rewrapping is the point: these are hand-wrapped to ~60 columns inside the comment, and the
    # description panel does its own wrapping at whatever width it has.
    joined = [" ".join(p) for p in paragraphs if p]
    return "\n\n".join(joined).strip()


# The header shape the 2026-08 Generative rewrite introduced, which carries no
# `# Description:` line at all:
#
#     # Repository: https://github.com/...
#     #
#     # PHOTO SONIFICATION: RGB SPECTRAL COLUMN SCAN
#     #
#     # CONCEPTUAL SCOPE
#     # ----------------
#     # This engine is intentionally a 1-D COLUMN-PROJECTION sonification of a 2-D
#     # Photo. Horizontal image position is preserved as musical time...
#
# An ALL-CAPS restatement of the title, then either prose or a section heading and its rule
# before the prose. Ten scripts had already moved to it when this was written, and every one of
# them fell back to showing its own title as its description — which is the exact hole
# `extract_description` was written to close in the first place.
TITLE_LINE_RE = re.compile(r"^#\s*([A-Z][A-Z0-9 &/,'\u2013\u2014:.()-]{6,})\s*$")
RULE_LINE_RE = re.compile(r"^#\s*[-=]{3,}\s*$")


def extract_titled_description(lines: list[str]) -> str:
    """The first prose paragraph under an ALL-CAPS header line. Empty if there is none.

    Confined to the file's **leading comment block**. Every script is full of section banners
    (`# INPUT CHECK`, `# SYNTHESIS`) that read exactly like a title, and `Vector Chain/chain_7`
    proved the point: it has no header title at all, so the scan ran on into the body, took
    `# INPUT CHECK` for a title and returned the path comments beneath it — displacing the step
    list `describe_chain` produces for a chain, which is the one thing a chain's description is
    for.
    """
    seen_title = False
    paragraph: list[str] = []
    for line in lines:
        stripped = line.rstrip()
        if not stripped.startswith("#"):
            if stripped.strip():
                break               # first line of code: the header is over
            if seen_title and paragraph:
                break
            continue
        body = stripped[1:].strip()
        if not seen_title:
            # The banner rules and the Author/Version/Repository block come first; the title is
            # the first all-caps line after them.
            if TITLE_LINE_RE.match(stripped) and ":" not in body.split(" ")[0]:
                seen_title = True
            continue
        if not body or RULE_LINE_RE.match(stripped):
            if paragraph:
                break                   # the paragraph ended
            continue                    # still in the gap before it
        if TITLE_LINE_RE.match(stripped):
            if paragraph:
                break                   # a further section heading closes it
            continue                    # a section heading before the prose
        paragraph.append(body)
    return " ".join(paragraph).strip()


# A chain script's own step list. The `Vector Chain` scripts are fixed pipelines of other
# processes, and five of the seven carry no `# Description:` block at all -- but every one of
# them numbers its stages in comments, which is precisely what a user needs to know about a
# chain: which processes it runs, in what order. Some also open with a one-line `# Flow:`
# summary.
STEP_RE = re.compile(r"^#\s*STEP\s*(\d+)\s*:\s*(.+?)\s*$")
FLOW_RE = re.compile(r"^#\s*Flow:\s*(.+?)\s*$")


def describe_chain(source: str) -> str:
    """A chain's stages, as a description. Empty when the script lists none."""
    flow = ""
    steps: list[str] = []
    for line in source.splitlines():
        if not steps:
            match = FLOW_RE.match(line.strip())
            if match:
                flow = match.group(1)
        match = STEP_RE.match(line.strip())
        if match:
            steps.append(f"{match.group(1)}. {match.group(2)}")
    if not steps:
        return flow
    header = f"{flow}\n\n" if flow else ""
    return f"{header}Runs these processes in order:\n\n" + "\n".join(steps)


def short_description_from(text: str, fallback: str) -> str:
    """One line for the browser list: the description's first sentence."""
    if not text:
        return fallback
    first = text.split("\n\n")[0]
    # A full stop after a *digit* is a numbered list item, not a sentence end: a chain reads
    # "...using four AudioTools modules: 1. MDS Space Navigator - ...", and splitting naively
    # cut it at "modules: 1".
    match = re.search(r"(?<=[^\d\s])[.;]\s", first)
    if match:
        first = first[: match.start() + 1]
    first = first.rstrip(".").strip()
    return first[:160] if first else fallback


def parse_form(source: str) -> list[Param] | str:
    """Parse a script's form block into parameters, or return a reason string on failure."""
    match = FORM_RE.search(source)
    if not match:
        return []
    return parse_fields(match.group(1))


# A bare numeric literal, so a default that is anything else can be recognised as a variable
# reference and looked up.
NUMBER_RE = re.compile(r"^[-+]?\d+(?:\.\d+)?(?:[eE][-+]?\d+)?$")

# `name = <literal>` at the top level of a script. Pause blocks in the "advanced settings" shape
# declare their defaults as these variables, assigned immediately above the `if` that guards the
# block, so this is where the real values live.
ASSIGNMENT_RE = re.compile(r"^\s*(\w+\$?)\s*=\s*(\"[^\"]*\"|[-+]?[\d.]+(?:[eE][-+]?\d+)?)\s*$", re.M)


def script_variables(source: str, before: int | None = None) -> dict[str, str]:
    """`name = <literal>` assignments, optionally only those before offset `before`.

    The cutoff is not optional in practice, it is the whole correctness of this. These scripts
    assign a pause field's default once near the top and then *again* inside every branch of an
    `if preset = N` chain further down — `HFD-Driven_Time_Warping` sets
    `use_percentile_mapping` six times. Reading the whole file and letting the last win picks up
    whichever preset happens to be written last, so the catalog would advertise one preset's
    value as the parameter's default. Only the assignments *preceding* the block are the
    defaults in force when it runs.
    """
    text = source if before is None else source[:before]
    out: dict[str, str] = {}
    for name, value in ASSIGNMENT_RE.findall(text):
        out[name] = value.strip('"')
    return out


def split_label_and_value(rest: str, colon_form: bool) -> tuple[str | None, str]:
    """Split a field declaration's operands into its label and its default.

    The modern colon syntax quotes the label, and that label may contain spaces --
    `positive: "Frame length s", frame_length_s`. Splitting on whitespace, which is right for
    the classic syntax, cuts that into `"Frame` and the rest, and the field is then reported as
    having a non-numeric default. That is what kept three "advanced settings" blocks
    unparseable, and with them 54 parameters locked at their script defaults.
    """
    rest = rest.strip()
    # A **multi-line** text field puts its height first: `text: 6, "Pbind", "Pbind(...)"`
    # declares a six-line editor named `Pbind`. The height is a property of the widget, not an
    # operand, and Praat still passes exactly one argument for the field — so it has to come off
    # before the label is read. Without this the label parses as `6` and the real label is
    # swallowed into the default, which arrives as `Pbind", "Pbind(...)`: the field count stays
    # right, so the script still *runs*, and the dialog simply offers a control called "6"
    # holding a mangled string. (`py/PraatPbind.praat`, rewritten into the colon syntax
    # upstream on 2026-08-19.) Recognised by shape rather than by keyword because a bare
    # integer followed by a comma and a quoted label is unique to this declaration.
    if colon_form:
        lines = re.match(r'(\d+)\s*,\s*(?=")', rest)
        if lines:
            rest = rest[lines.end() :]
    if colon_form and rest.startswith('"'):
        end = rest.find('"', 1)
        if end == -1:
            return None, ""
        # The value still needs unquoting — the colon syntax quotes numeric defaults too
        # (`positive: "Interaural_delay_ms", "0.68"`), and returning `"0.68"` with its quotes
        # reads as a non-numeric default and drops the whole script.
        value = rest[end + 1 :].lstrip().lstrip(",").strip()
        return rest[1:end], unquote(value, colon_form)
    bits = rest.split(None, 1)
    if not bits:
        return None, ""
    name = unquote(bits[0].rstrip(",").rstrip(":"), colon_form)
    value = unquote(bits[1].strip().lstrip(",").strip(), colon_form) if len(bits) > 1 else ""
    return name, value


STRING_CAST_RE = re.compile(r"^string\$\(\s*([A-Za-z_][A-Za-z_0-9]*)\s*\)$")


def unwrap_string_cast(value: str) -> str:
    """`string$(minimum_frequency_Hz)` -> `minimum_frequency_Hz`, else the value unchanged.

    A pause field seeds its default with the variable the script assigned just above it, and for
    a *numeric* field Praat wants that as text -- so the idiomatic spelling is
    `positive: "Minimum_frequency_Hz", string$(minimum_frequency_Hz)`. Resolving only the bare
    name missed every one of those, and a whole script fell out of the catalog because one field
    reported a "non-numeric default" (`AI & Adaptive/CWT_Granular_Resampler.praat`, whose two
    pages spell all seventeen numeric defaults this way).

    Deliberately narrow: only a cast wrapping a single bare identifier. Anything else -- an
    arithmetic expression, a nested call, `fixed$(x, 2)` -- is left alone to fail loudly as a
    non-numeric default, which is the honest outcome for a value this cannot know.
    """
    match = STRING_CAST_RE.match(value.strip())
    return match.group(1) if match else value.strip()


def parse_fields(body: str, variables: dict[str, str] | None = None) -> list[Param] | str:
    """Parse a run of Praat field declarations -- a `form` body or a `beginPause` body.

    `variables` supplies the script's top-level assignments, so a pause field whose default is a
    variable reference resolves to the value the script actually uses.
    """
    params: list[Param] = []
    pending: Param | None = None
    # (index of the param the note precedes, note). Recorded as `len(params)` at the moment the
    # `comment` is read, which is exactly the index the *next* field will take -- so the notes
    # need no bookkeeping at each of the half-dozen places below that append a param.
    notes: list[tuple[int, Note]] = []

    for raw in body.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        head, _, rest = line.partition(" ")
        # A trailing colon on the *keyword* marks the modern syntax, which quotes its operands.
        colon_form = head.endswith(":")
        keyword = head.rstrip(":").lower()

        if keyword in IGNORED_KEYWORDS:
            note = classify_comment(rest, colon_form)
            if note is not None:
                notes.append((len(params), note))
            continue

        if keyword in ("option", "button"):
            if pending is not None:
                pending.options.append(unquote(rest, colon_form))
            continue

        if keyword in CHOICE_KEYWORDS:
            # Same quoted-label handling as every other field — `optionmenu: "Mapping curve",
            # mapping_curve` splits into `"Mapping` and junk otherwise, and the malformed entry
            # then swallows the `option:` lines that follow and shifts every later field's value
            # by one.
            name, index_text = split_label_and_value(rest, colon_form)
            if name is None:
                return f"malformed {keyword} declaration: {line!r}"
            index_text = index_text or "1"
            if not NUMBER_RE.match(index_text.strip()) and variables:
                index_text = variables.get(index_text.strip(), index_text)
            try:
                index = int(float(index_text))
            except ValueError:
                index = 1
            pending = Param(name=name, kind="choice", default=index, options=[])
            params.append(pending)
            continue

        pending = None
        name, value_text = split_label_and_value(rest, colon_form)
        if name is None:
            return f"malformed declaration: {line!r}"
        # A pause block often gives its default as the *variable* the script assigned just
        # above it (`positive: "Frame length s", frame_length_s`) rather than as a literal.
        # Resolve it from those assignments; an unresolvable one falls through and is reported
        # by the caller as a non-numeric default, which is the honest outcome.
        if value_text and not NUMBER_RE.match(value_text.strip()):
            resolved = variables.get(unwrap_string_cast(value_text), None) if variables else None
            if resolved is not None:
                value_text = resolved

        if keyword == "boolean":
            on = value_text.strip() in ("1", "yes", "on")
            if SILENCE_RE.match(name):
                on = False
            params.append(Param(name=name, kind="toggle", default=on))
        elif keyword in NUMERIC_KEYWORDS:
            token = value_text.split()[0] if value_text else "0"
            try:
                default = float(token)
            except ValueError:
                return f"non-numeric default for {name!r}: {value_text!r}"
            params.append(make_number(keyword, name, default))
        elif keyword in TEXT_KEYWORDS:
            # Most of these are not prose. A `sentence` whose default is a delimited run of
            # numbers is a *list* -- a twelve-tone row, a resonator's frequency bank, a rhythm
            # pattern -- and is very often the substance of the process rather than a label.
            # Those become an editable list; see `parse_number_list`, which is deliberately
            # strict so a genuine text field is never mistaken for one.
            recognised = parse_number_list(value_text)
            if recognised:
                separator, values = recognised
                params.append(make_number_list(name, separator, values))
                continue
            pairs = parse_key_values(value_text)
            if pairs:
                for key, value in pairs:
                    # Deliberately never inferred as integer, unlike a number list: a single
                    # whole default says nothing about the field. `start_y=0.0` is a coordinate
                    # that wants fractions, and reading it as an integer would silently forbid
                    # them.
                    params.append(Param(
                        name=f"{name}_{key}",
                        kind="number",
                        default=value,
                        integer=False,
                        minimum=-math.inf,
                        maximum=math.inf,
                        step=0.01,
                        key_value_group=name,
                        key_value_key=key,
                    ))
                continue
            if FOLDER_NAME_RE.search(name):
                params.append(Param(name=name, kind="folder_path", default=0.0))
                continue
            if FILE_PATH_NAME_RE.search(name):
                suffix = value_text.rsplit(".", 1)[-1].lower() if "." in value_text else ""
                if suffix.isalnum() and 1 <= len(suffix) <= 5:
                    params.append(Param(name=name, kind="file_path", default=0.0,
                                        text_default=suffix))
                    continue
            # Everything left really is free text: an L-system rule, a note name, an output
            # prefix, a Praat formula. It gets a plain typed field.
            params.append(Param(name=name, kind="text", default=0.0, text_default=value_text))
            continue
            # What is left really is free text -- a folder path, an L-system alphabet, a Praat
            # formula, an output-name prefix -- and has no bounded editor in this app yet.
            return f"parameter {name!r} is a free-text {keyword} field"
        else:
            return f"unsupported form field {keyword!r}"

    for param in params:
        if param.kind == "choice":
            if not param.options:
                return f"optionmenu {param.name!r} declares no options"
            param.default = min(max(int(param.default), 1), len(param.options)) - 1
    for index, note in notes:
        if index < len(params):
            params[index].notes.append(note)
        elif params:
            params[-1].notes_after.append(note)
        # A body whose only content is a comment has no field to hang it on, and no row in the
        # dialog either; there is nothing to preserve.
    return params


# Delimiters a `sentence` field's number list is tried against, most specific first. Order
# matters: "1.0, 1.5, 2.0" splits cleanly on ", " and would leave a leading space on every
# entry but the first if split on "," alone, so the two-character form has to win.
#
# Deliberately does NOT include the empty delimiter. `BPM_Panning`'s `Accent_grid`
# ("1010100110101001", one digit per sixteenth) is a real number list with no separator at
# all, but auto-detecting that is indistinguishable from a plain sixteen-digit number, so it
# stays excluded until it is worth an explicit per-script override. Guessing there would turn
# any large integer default into a list of digits.
LIST_SEPARATORS = (", ", ",", " ", "_")

# A `sentence` field naming a directory rather than holding prose. Matched on the parameter's
# own name, which is how these scripts consistently label them (`Folder`, `Folder_path`,
# `Corpus_A_folder`). Deliberately not `*_file`/`*_path` in general: `Reference_filename` names a
# file *inside* the chosen folder and is ordinary text, and `File_path` on the SPEAR parser is a
# single .txt whose picker filters by extension.
FOLDER_NAME_RE = re.compile(r"(^|_)(folder|dir|directory)(_|$)", re.I)

# A `sentence` field holding `key=value` pairs, which the receiving script picks apart with
# Praat's `extractNumber(field$, "key=")`. Because `extractNumber` searches for the key rather
# than counting positions, the pairs can be rebuilt in any order with any spacing -- so each key
# can become its own numeric field in the dialog and be rejoined on the way out. That turns
# `h0=1.2 v0=6.0 grav=9.8 rest=0.75 bounces=8` into five labelled numbers instead of one string
# to retype without typos.
#
# Detection needs no per-script table: the default *is* the schema. Every token must be
# `key=<number>`, which rejects `sin(1/x)`, `psi, phi, theta` and every other free-text field.
KEY_VALUE_RE = re.compile(r"^[A-Za-z_]\w*=-?\d+(?:\.\d+)?$")

# Below two pairs it is not worth splitting a field into its own row per key.
MIN_KEY_VALUE_PAIRS = 2


def parse_key_values(default: str) -> list[tuple[str, float]] | None:
    """Recognise a `key=value key=value` field, or None."""
    tokens = default.split()
    if len(tokens) < MIN_KEY_VALUE_PAIRS or not all(KEY_VALUE_RE.match(t) for t in tokens):
        return None
    pairs = []
    for token in tokens:
        key, _, value = token.partition("=")
        pairs.append((key, float(value)))
    # A repeated key would collapse two dialog rows onto one argv value.
    if len({k for k, _ in pairs}) != len(pairs):
        return None
    return pairs


# A `sentence` field naming one file to read. Narrow on purpose: `Reference_filename` names a
# file *inside* an already-chosen folder and is ordinary text, so only the explicit
# "file path" spelling counts. The extension to filter the browser by is read off the field's
# own default -- the one script with this shape defaults to
# `C:/Users/User/Desktop/sounds/Cello.txt`, an author's own machine, which is exactly the value
# a picker should replace.
FILE_PATH_NAME_RE = re.compile(r"(^|_)file_?path(_|$)", re.I)

# Scripts whose only remaining blocker is a free-text field naming somewhere to *write*, or an
# external asset library this app does not manage. Kept out deliberately rather than by
# accident: a process whose output never comes back into the editor does not belong in it --
# the same call `ts oscil` got on the CDP side. Listed with reasons so the decision is
# reviewable rather than invisible.
# Scripts that legitimately never read the selected Sound because they *build* one from other
# material, and are wanted anyway. The generic detector below rejects anything that does not
# mention `selected` and `"Sound"`, which is the right default -- a script that ignores the
# selection is usually a utility, a report, or a folder walker, none of which belong in a
# waveform editor.
#
# An entry here is a claim that the process is a generator whose result is worth having, and
# that its output can actually be found: this app's driver takes the highest-numbered Sound
# left when the script returns, so a script leaving a pile of intermediates would hand back the
# wrong one. Both halves were checked by running it.
GENERATORS: dict[str, str] = {
    # Weaves a drone out of a folder of clips. Verified by running it against six test clips:
    # it ends with `selectObject: final_sound` after removing every clip it read, leaving
    # exactly one Sound object, so the driver cannot pick up the wrong thing. Its trailing
    # unconditional `Play` -- the hazard that costs 32 other scripts real time -- returns
    # immediately here, so the whole run took 5.6s wall clock.
    "AI & Adaptive/Bayesian_Drone_Weaver.praat":
        "builds a drone from a folder of clips; the selection is not its material",
    # A pure generator: builds a Risset-style mutation study from its own parameters. Verified
    # by running it -- ends `selectObject: "Sound " + master_name$` and leaves exactly one Sound
    # object (`Master_Mix_...`, the requested duration), in 0.6s. Its `Play` is guarded by
    # `show_visuals`, which SILENCE_RE already forces off. `chain_7` runs it as its third stage,
    # so it was already reachable through a chain but not on its own.
    "Generative & Synthesis/Risset\'s_Mutations.praat":
        "generates a mutation study from its own parameters; reads no input sound",
    # Was an ordinary transform until upstream reworked it (2026-08-08, commit 0de18db) into
    # multi-bank source-filter granular synthesis: it now builds its Sound from its own
    # parameters and mentions neither `selected` nor `"Sound"`, so the generic detector dropped
    # it. Verified by running it -- it ends on `selectObject: output_sound`, calls `removeObject`
    # on all nine of its intermediates, and leaves exactly that one Sound, so the driver's
    # highest-numbered-Sound rule cannot pick up the wrong object. Its `Play` is guarded by
    # `play_result`, which SILENCE_RE already forces off.
    "Generative & Synthesis/Rich_Formant_Grains.praat":
        "synthesises formant grains from its own parameters; reads no input sound",
}

# The plugin folder whose scripts *synthesise* rather than transform: they build a Sound from
# their own parameters and read no input at all. Emitted as `input = "none"`, which is what lets
# them run with **no buffer open**, the way Record already does -- and they are already sent to a
# new buffer by `App::praat_opens_new_buffer`, which files the whole group that way.
#
# Catalogued as ordinary `wav` processes until 2026-08-09, which meant every one of them needed
# some unrelated file loaded before it would do anything: with nothing open the app's submit path
# hit its "no document to read" return and gave up in silence (user report, against Formant
# Synthesis -- whose own header says "Run this script (no input sound required)"). The mistake was
# invisible in the catalog because the default is applied by omission: nothing said "wav", the
# emitter simply had nowhere else to fall back to.
#
# A directory rule rather than a per-script allowlist: this folder *is* the claim, upstream adds
# generators to it regularly, and a new one should work on arrival rather than wait for someone to
# list it. The cost is the inverse mistake -- a transforming script dropped in here would ignore
# its input rather than erroring -- which is what the exception table below is for, and why every
# script in the folder was read before this shipped rather than the group being taken on trust.
ZERO_INPUT_DIR = "Generative & Synthesis"

# The scripts in `ZERO_INPUT_DIR` that genuinely read the selected Sound, and so must keep
# requiring a buffer. Verified by reading every script in the folder for a form infile, a
# `Read from file`, or a `numberOfSelected("Sound")` guard: 47 of the 50 non-photo scripts
# reference an input nowhere at all, and these are the ones that do.
def zero_or_photo_input_kind(rel_key: str) -> str:
    """The `input =` value for a script that reads something other than the selected Sound.

    `""` for everything else, which lets the emitter fall back to its wav/dual_wav default.
    Photo wins over the directory rule: the four image sonifiers live in `ZERO_INPUT_DIR` too,
    and they *do* take an input -- a picture -- which the driver has to select before the run.
    """
    if rel_key in PHOTO_INPUTS:
        return "photo"
    in_zero_input_dir = rel_key.split("/")[0] == ZERO_INPUT_DIR
    if in_zero_input_dir and rel_key not in ZERO_INPUT_EXCEPTIONS:
        return "none"
    return ""


ZERO_INPUT_EXCEPTIONS: dict[str, str] = {
    # Hard-requires exactly one Sound and `exitScript`s without it -- the Sound *is* the
    # pulsaret, the convolution kernel every grain is made of, so there is no run without one.
    "Generative & Synthesis/Pulsar_Synthesis_Engine.praat":
        "requires exactly one selected Sound as the convolution kernel; exitScripts without it",
    # Reads a selected Sound only when its own `use_selected_sound` toggle is on, and logs
    # "[Analysis] No Sound selected -- using manual parameters." otherwise, so it *would* run
    # bufferless. Kept requiring one anyway: the toggle is a real feature of the process, and a
    # zero-input declaration would make the app never hand it the Sound the toggle asks for.
    "Generative & Synthesis/Waveguide_Klangmaschine.praat":
        "optionally analyses a selected Sound (`use_selected_sound`); keeping the input reachable",
}

# Scripts that read a Praat **Photo** object rather than a Sound -- the image sonifiers. They
# trip `non_sound_input` by design (they say "Please select a Photo object first."), and that
# exclusion is still right for every other script it catches: a TextGrid or Table process has no
# input this app can supply. An image does. The app opens a file picker for it, reads the PNG
# into a Photo and selects that before `runScript:` -- see `IoKind::Photo` and
# `praat::driver`'s `DriverOptions::photo_input`.
#
# Emitted as `input = "photo"`, which also means **zero Sound inputs**: these generate audio from
# a picture, so they run with no document open at all, exactly as Record does. Their `Generative`
# group already routes the result to a new buffer (`App::praat_opens_new_buffer`).
#
# **PNG only**, and that is Praat's limit rather than a choice made here: it links `libpng` and
# nothing else, so a JPEG or TIFF fails with `Error reading PNG file` and a BMP/GIF with `not
# recognized`. All four verified by running them against a generated PNG through a driver of
# exactly the shape the app builds.
PHOTO_INPUTS: dict[str, str] = {
    "Generative & Synthesis/Percussive_Image_Sonification.praat":
        "scans image columns left-to-right into clicks; brightness drives rate, pitch and volume",
    "Generative & Synthesis/Photo_sonification.praat":
        "maps R/G/B to low/mid/high frequency bands, brightness to amplitude",
    "Generative & Synthesis/Photo_Brightness-Controlled_Pitch_Sonification.praat":
        "maps brightness to a phase-continuous pitch contour",
    "Generative & Synthesis/Spectral_Image_Sonification.praat":
        "additive synthesis; R/G/B drive interleaved harmonic groups",
}

# Scripts this app will not offer, as a decision rather than as an obstacle.
#
# Distinct from `OUT_OF_SCOPE`, which reads as "this does not fit the shape of the app" and
# invites a re-look whenever the app's shape changes -- a folder-of-files writer becomes
# thinkable the day batch export exists. These will not, whatever gets built: the reason is in
# the *script*, not in what tui-wave happens to support this month.
#
# Being here also settles the dependency question. A library kept for a process nobody can reach
# is pure install cost, so anything imported by these helpers and nothing else comes back out of
# `PY_ALLOWED_IMPORTS` -- see `cv2` there.
NEVER_PLANNED: dict[str, str] = {
    # Opens the webcam, calibrates for 2 seconds against the background, then records 10 seconds
    # of free-hand motion and derives three control channels from frame differencing. It is a
    # live performance instrument that happens to write a CSV: the transformation afterwards is
    # ordinary offline Praat, but the input is a person waving at a camera for ten seconds.
    # There is no version of a keyboard-driven terminal editor that asks for that, and no camera
    # in a batch `praat --run`. Arrived in the catalog on 2026-08-08 with the OpenCV allowlist
    # entry and was pulled the same day; `cv2` went with it, being imported by nothing else.
    "py/MotionControl.praat":
        "captures ten seconds of webcam hand motion as its control input",
    # The other live-capture one, and the same reason in a different sense: it opens the
    # microphone and records for a fixed number of seconds *before* processing. Its input is not
    # the buffer you have open, so there is nothing for a process to apply to.
    "py/Live_1.praat":
        "records from the microphone as its input; the open selection is never read",
    # These four write files somewhere other than the editor, or need something the editor has
    # no way to hold. That is not the same as `out_of_scope`'s "does not fit the app as it
    # stands": a process whose product is a folder on disk has no result to splice back into a
    # buffer, whatever else gets built, and one that wants an HRIR library or a VST host is
    # asking this app to manage an installation it has no business managing.
    "Analysis/Batch_Channel_Format_Exporter.praat":
        "writes a folder of files elsewhere; nothing returns to the editor",
    "Spatial & Surround/22_2_Stem_Renderer.praat":
        "needs an external HRIR library on disk, which this app does not manage",
    "Spatial & Surround/Higher-Order_Ambisonic_Encoder.praat":
        "reads a folder of B-format WAVs and writes another; neither is the open selection",
    # Excluded outright rather than by its dependency, so that installing `pedalboard` cannot
    # bring it back: wheel 0.9.24 aborts the interpreter with SIGILL on import (reproducible, on
    # a 13th-gen Intel Core i7-1355U). Hosting VST plugins is also not something a terminal
    # editor can offer a UI for -- the plugin's own window is the whole interface.
    "py/VST_Effect_from_Praat.praat":
        "hosts VST plugins, whose own windows are the entire interface",
    # Its `chooseDirectory$` call is hoistable and hoisted for its sibling
    # `Semantic_timbre_retrieval` -- but the chooser was never this one's real obstacle. It
    # writes a launch JSON, starts `corpus_map.py` **detached** (`runSystem_nocheck ... &`) and
    # returns, so the Qt window it opens outlives the run and no Sound object ever comes back:
    # the process would report "produced no Sound object" every time, correctly. Excluded for
    # what it is rather than for the call, which is why PySide6/pyqtgraph/sounddevice stay out
    # of PY_ALLOWED_IMPORTS -- installing them cannot bring this back.
    "py/CorpusMap.praat":
        "launches a detached Qt window and returns no Sound; nothing comes back to the editor",
}

# Scripts that are *defective upstream* — not blocked by their own nature (`NEVER_PLANNED`)
# nor by the app's current shape (`OUT_OF_SCOPE`), but simply broken in the checkout in front
# of us. The distinction earns its own table because the expected lifetime is different: these
# are meant to come back, so each entry carries a `guard` that re-derives the defect from the
# script and `broken_upstream_reason` drops the entry the moment the guard stops finding it. A
# fix upstream therefore restores the process with no edit here, and `stale_tables` still
# catches a rename.
#
# `guard` takes the script source and returns True while the defect is present.
def _brightness_classifier_is_broken(source: str) -> bool:
    """`HF_split_Hz`'s derived variable is read nowhere, and no branch assigns it for Custom.

    Praat lowercases only the *first* letter of a form label, so `positive HF_split_Hz` binds
    `hF_split_Hz` — while the script reads `hf_split_Hz` throughout. The five non-Custom preset
    branches assign `hf_split_Hz` themselves, which makes the form field inert there; the
    `else` (Custom, and anything past the listed presets) does not, so the first read of it
    hits an undefined variable and Praat aborts. Verified against the 7.0 binary via plain
    `runScript`, so this is upstream's bug rather than anything this app does.
    """
    if "positive HF_split_Hz" not in source:
        return False  # upstream renamed the label — re-test it
    if re.search(r"\bhF_split_Hz\b", source):
        return False  # upstream now reads the variable the label really binds
    return bool(re.search(r"\bhf_split_Hz\b", source))


BROKEN_UPSTREAM: dict[str, dict] = {
    "Analysis/BrightnessClassifier.praat": {
        "guard": _brightness_classifier_is_broken,
        "reason": "form field HF_split_Hz binds hF_split_Hz but the script reads hf_split_Hz, "
                  "so the control is inert on five presets and Custom aborts on an undefined "
                  "variable",
    },
}


def broken_upstream_reason(rel_key: str, source: str) -> str | None:
    """The exclusion reason while the defect is still present, else None."""
    entry = BROKEN_UPSTREAM.get(rel_key)
    if entry is None:
        return None
    return entry["reason"] if entry["guard"](source) else None


OUT_OF_SCOPE: dict[str, str] = {
    # Deliberately near-empty now: everything it used to hold moved to `NEVER_PLANNED`, this
    # table being for scripts blocked by *the app's current shape* rather than by their own.
    # What is left is reached dynamically rather than listed here -- `Max-MSP/` is skipped as a
    # directory, and `py/SSMComposer` and `py/Composition_1` fall out of the py-group rule that
    # a script there must drive a Python helper. Both of those are recoverable without anything
    # being built: upstream has simply never shipped `ssm_morph_engine.py`, and `Composition_1`
    # is pure Praat that reads the selected Sound and would work today but for living in `py/`.
    # Keeping them here rather than in `NEVER_PLANNED` is what keeps them findable.
}

# Below this many entries, a "list" is more likely a word that happens to be numeric ("8") or
# a filename fragment. Every real one in this plugin has at least three.
MIN_LIST_ENTRIES = 2


def parse_number_list(default: str) -> tuple[str, list[float]] | None:
    """Recognise a `sentence` default that is really a delimited list of numbers.

    Returns `(separator, values)`, or None when the field is genuinely free text. The
    converter used to reject every text field on the stated grounds that they are "labels and
    filenames"; across this plugin eleven are lists that ARE the process -- a twelve-tone row,
    a resonator's frequencies, a rhythm pattern -- and only two are labels.

    Conservative on purpose, since a false positive turns a working free-text field into a
    numeric editor that cannot express what the script wants: every token must parse as a
    float, and there must be at least MIN_LIST_ENTRIES of them. That rejects `sin(1/x)`,
    `psi, phi, theta`, `Kemar_HRIR/`, `start_x=-1.0 start_y=0.0` and the L-system alphabets,
    all of which contain a candidate delimiter but no numbers.
    """
    text = default.strip()
    if not text:
        return None
    for separator in LIST_SEPARATORS:
        parts = [p for p in text.split(separator)]
        if len(parts) < MIN_LIST_ENTRIES:
            continue
        values = []
        for part in parts:
            part = part.strip()
            if not part:
                break
            try:
                values.append(float(part))
            except ValueError:
                break
        else:
            return separator, values
    return None


def make_number_list(name: str, separator: str, values: list[float]) -> Param:
    """Build a number-list param from the entries its script declares.

    Unbounded, for exactly the reason `make_number` is: the script states these values and
    nothing about their permitted range, so there is nothing to state. The list editor clamps
    to `min`/`max`, and clamping to an infinity is a no-op.

    Integer-ness *is* inferred, because it is not a guess about range: a twelve-tone row and a
    rhythm pattern are whole numbers and edit far better as such, while a ratio list is not.
    It also cannot silently reject anything the script ships, since it is read off those very
    values.
    """
    integer = all(float(v).is_integer() for v in values)
    minimum, maximum = -math.inf, math.inf
    # Only feeds the envelope editor, which no Praat param can open (`automatable` is always
    # false); the arrow keys nudge by a whole unit regardless. See `make_number`.
    step = 1.0 if integer else 0.01

    return Param(
        name=name,
        kind="number_list",
        default=0.0,
        integer=integer,
        minimum=minimum,
        maximum=maximum,
        step=step,
        list_separator=separator,
        list_default=values,
    )


# A range stated in the parameter's own name, which is the only range these scripts ever
# actually declare. Praat form names carry underscores where the author typed spaces, so
# `Elevation (degrees -90 to 90)` arrives as `Elevation_(degrees_-90_to_90)`.
#
# Deliberately strict, and anchored to the whole parenthesised group. Most parentheses in
# these names are *units*, not ranges -- (s), (Hz), (dB), (BPM), (degrees), (grains/sec) --
# and two are legends that look numeric but are not: `Duration_(0_=_original)` and
# `Symmetry_(0_off_1_palindrome)`. Reading either of those as a range would clamp the field to
# a span its author never meant.
NAME_RANGE_RE = re.compile(
    r"\("
    r"(?:[A-Za-z]+_)?"                      # optional unit prefix: (degrees_0-360)
    r"(-?\d+(?:\.\d+)?)"                    # low
    r"(?:_to_|-)"                           # separator: `_to_` or a bare hyphen
    r"(-?\d+(?:\.\d+)?)"                    # high
    r"\)$"
)


def range_from_name(name: str) -> tuple[float, float] | None:
    """The (low, high) a parameter's own name declares, or None if it declares none.

    Only about 20 of this plugin's ~3300 numeric parameters carry one. Everything else states
    a default and nothing more, which is why `make_number` no longer invents bounds for them.
    """
    match = NAME_RANGE_RE.search(name)
    if not match:
        return None
    low, high = float(match.group(1)), float(match.group(2))
    return (low, high) if low < high else None


def make_number(keyword: str, name: str, default: float) -> Param:
    """Build a numeric param, taking bounds ONLY from what is actually declared.

    A Praat `form` states a default and, in about twenty cases out of ~3300, a range inside
    the parameter's own name. It states nothing else. This used to synthesise a range anyway --
    ten times the default, straddling zero when the default was zero or negative -- which was
    invention presented to the user as fact, and wrong in both directions: it capped
    parameters whose useful values ran far higher, and it offered negative values to
    parameters that are meaningless below zero.

    So there are now exactly two sources of a bound:

      * the name, via `range_from_name` -- a real declaration, used verbatim; and
      * the Praat *type*, whose floor the form parser itself enforces (`positive` rejects <= 0,
        `natural` rejects < 1). That is not our guess either, so it stays.

    Everything else is unbounded, spelled `inf` / `-inf`. Those survive TOML, keep `min <= max`
    true, make `clamp` a no-op, and pass the catalog's own float-noise check, so no new field
    or special case is needed anywhere to carry "no limit".

    The one thing an open interval cannot be: `positive` means strictly greater than zero, and
    `clamp` takes a closed bound. The floor is therefore 0.0 and exactly zero can reach Praat,
    which rejects it by name ("Value must be greater than 0"). One value passes through to a
    precise error rather than being silently rounded up to an epsilon nobody chose.
    """
    integer = keyword in ("integer", "natural")

    declared = range_from_name(name)
    if declared:
        minimum, maximum = declared
    elif keyword == "positive":
        minimum, maximum = 0.0, math.inf
    elif keyword == "natural":
        minimum, maximum = 1.0, math.inf
    else:
        minimum, maximum = -math.inf, math.inf

    minimum = strip_float_noise(minimum)
    maximum = strip_float_noise(maximum)

    if integer:
        # `int()` on an infinity raises, so only finite bounds are squared off.
        if math.isfinite(minimum):
            minimum = float(int(minimum))
        if math.isfinite(maximum):
            maximum = float(int(maximum))
        step = 1.0
    elif math.isfinite(maximum - minimum):
        step = round_step(maximum - minimum)
    else:
        # No span to derive a step from. The arrow keys nudge by a whole unit (0.1 with the
        # fine modifier) regardless of `step` -- see `cdp_nudge_number` -- so this only feeds
        # the envelope editor, which no Praat param can open (`automatable` is always false).
        step = 0.01

    # A declared range that does not contain the script's own default means one of the two was
    # misread, and clamping would silently change what the script ships with. Trust the
    # default -- it is unambiguous -- and widen to admit it.
    if default < minimum:
        minimum = default
    if default > maximum:
        maximum = default

    return Param(
        name=name,
        kind="number",
        default=default,
        integer=integer,
        minimum=minimum,
        maximum=maximum,
        step=step,
    )


def strip_float_noise(value: float) -> float:
    """Drop the binary-representation noise from a bound.

    Bounds used to be arithmetic on decimal defaults, and neither operand nor result is exactly
    representable in binary: `0.06 * 11` is `0.6599999999999999` and `0.06 / 1000` is
    `5.9999999999999995e-05`, so a form field advertised a range of
    `[0.000059999999999999995-0.6599999999999999]` for a parameter whose default is `0.06`
    (user report). The digits are real, they are just not information.

    Mostly moot now that bounds are read from a parameter's own name rather than synthesised
    (see `make_number`) -- a declared `(0-1)` has no noise to strip. Kept because a declared
    bound still passes through here, and because `math.inf` must survive it untouched.

    Ten significant digits is deliberately far more than any bound here carries, but the point
    is to cut only the noise, which starts around the fifteenth digit. Rounding harder would also quietly move bounds that were
    computed exactly, e.g. an integer maximum of 11264. This is the same fix `round6` applies on
    the Rust side to values a user nudges.
    """
    if value == 0.0 or not math.isfinite(value):
        return value
    exact = Decimal(repr(value))
    return float(round(exact, -exact.adjusted() + 9))


def round_step(span: float) -> float:
    """A step roughly 1/1000 of the range, snapped to a power of ten so the UI shows round
    numbers rather than values like 0.0037."""
    if span <= 0:
        return 0.01
    exponent = math.floor(math.log10(span / 1000.0)) if span > 0 else -2
    return float(10.0 ** min(max(exponent, -6), 3))


def title_for(stem: str) -> str:
    """Turn a filename into a display title.

    Upstream filenames use underscores for spaces *and* for stripped punctuation, so a name
    like `Wavefolder__Foldback_` has to collapse runs and drop the trailing separator.
    """
    text = stem.replace("_", " ")
    text = re.sub(r"\s+", " ", text).strip()
    return text or stem


def key_for(group_dir: str, stem: str) -> str:
    slug = re.sub(r"[^a-z0-9]+", "_", f"{group_dir} {stem}".lower()).strip("_")
    return f"praat_{slug}"


def disambiguate(processes: list[Process]) -> list[str]:
    """Give colliding keys and titles a numeric suffix, in place. Returns what it renamed.

    Upstream ships eight pairs of scripts whose names differ only in punctuation -- `Whisper
    Morph.praat` and `Whisper_Morph.praat`, `Undertone Field.praat` and
    `Undertone_Field.praat`, `Stereo_Shimmer.praat` and `stereo_shimmer.praat` -- and
    `key_for` slugifies both halves of each pair to the same string. That was not harmless:
    `CdpCatalog::load` treats a repeated key as an *override* (which is how a user catalog
    replaces a built-in entry), so the second silently replaced the first and eight real
    processes never reached the browser. They are not duplicate files either; every pair
    differs by hundreds of lines, so each is a genuinely different process.

    Suffixes are assigned in sorted-path order, which `collect()` already iterates in, so a
    given script keeps the same key across runs. The title gets the same treatment: two
    identical rows in the browser would be no more usable than one missing one.
    """
    renamed: list[str] = []
    seen: dict[str, int] = {}
    for proc in processes:
        count = seen.get(proc.key, 0) + 1
        seen[proc.key] = count
        if count > 1:
            renamed.append(f"{proc.bin} -> {proc.key}_{count}")
            proc.key = f"{proc.key}_{count}"
            proc.title = f"{proc.title} ({count})"
    return renamed


def toml_string(value: str) -> str:
    escaped = value.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n")
    return f'"{escaped}"'


def toml_number(value: float) -> str:
    # TOML spells infinity `inf` / `-inf`, with no decimal point -- and it must not get one,
    # since `inf.0` is not a float to any parser. An unbounded parameter (see `make_number`)
    # carries exactly this.
    value = float(value)
    if math.isinf(value):
        return "-inf" if value < 0 else "inf"
    # Every numeric field on the Rust side is f64; emitting a bare `1` would deserialize as an
    # integer and fail. Always carry a decimal point.
    text = repr(value)
    return text if ("." in text or "e" in text or "E" in text) else text + ".0"


def submodule_sha() -> str:
    try:
        return subprocess.run(
            ["git", "-C", str(PLUGIN), "rev-parse", "HEAD"],
            capture_output=True, text=True, check=True,
        ).stdout.strip()
    except Exception:
        return "unknown"


# Regex the near-miss report below looks for: a script that reads a Photo object. Both spellings
# the four known ones use -- the runtime check they gate themselves with, and the message they
# print when it fails.
PHOTO_HINT_RE = re.compile(r'numberOfSelected\s*\(\s*"Photo"\s*\)|select a Photo object', re.I)


def check_stale_keys() -> list[str]:
    """Names in a path-keyed table that no longer match any script in the checkout.

    Every one of these tables makes a *claim* about a specific file -- "this one hoists a pause
    dialog", "this one reads a Photo", "this one generates rather than transforms". Upstream
    renames constantly (`Creative Formant Manipulations.praat` ->
    `Creative_Formant_Manipulations.praat` in one update), and a key that stops matching does not
    fail: the script quietly falls back to the generic path, which for `PHOTO_INPUTS` and
    `GENERATORS` means being *excluded* instead. 439 processes become 438 with nothing said.

    Warned rather than fatal, so a routine `update-praat-scripts.sh` still produces a catalog --
    but named loudly, because the update script's own "gone:" diff blames upstream for what is
    really a stale table here.
    """
    present = {str(p.relative_to(PLUGIN)).replace("\\", "/") for p in PLUGIN.rglob("*.praat")}
    stale = []
    for label, table in (
        ("PAUSE_HOISTS", PAUSE_HOISTS),
        ("DIRECTORY_HOISTS", DIRECTORY_HOISTS),
        ("GENERATORS", GENERATORS),
        ("PHOTO_INPUTS", PHOTO_INPUTS),
        # Stale here is the dangerous direction: a renamed exception stops matching, the
        # directory rule then claims the script reads nothing, and a process that needs a
        # Sound would run without one instead of failing.
        ("ZERO_INPUT_EXCEPTIONS", ZERO_INPUT_EXCEPTIONS),
        ("OUT_OF_SCOPE", OUT_OF_SCOPE),
        ("NEVER_PLANNED", NEVER_PLANNED),
        # A stale entry here is the benign direction — the guard would stop matching anyway and
        # the process would simply return — but a renamed script should still be reported rather
        # than sitting in the table forever pointing at nothing.
        ("BROKEN_UPSTREAM", BROKEN_UPSTREAM),
    ):
        for key in table:
            if key not in present:
                stale.append(f"{label}: {key}")
    return sorted(stale)


def collect() -> tuple[list[Process], list[tuple[str, str, str]]]:
    processes: list[Process] = []
    excluded: list[tuple[str, str, str]] = []  # (relative path, reason slug, detail)

    for path in sorted(PLUGIN.rglob("*.praat")):
        rel = path.relative_to(PLUGIN)
        if rel.name == "setup.praat":
            continue
        top = rel.parts[0]
        if top in SKIP_DIRS:
            excluded.append((str(rel), "out_of_scope", f"{top}/ is out of scope"))
            continue
        if top not in GROUP_DIRS:
            excluded.append((str(rel), "out_of_scope", f"unrecognised directory {top}/"))
            continue

        source = read_script(path)

        # Checked before `OUT_OF_SCOPE` only so the two tables cannot both claim a script and
        # have the answer depend on order; nothing is in both.
        never_planned = NEVER_PLANNED.get(str(rel).replace("\\", "/"))
        if never_planned:
            excluded.append((str(rel), "never_planned", never_planned))
            continue

        # Before the parse: a script broken upstream would otherwise produce a plausible entry
        # with a dead control, which is worse than an honest absence. The guard re-reads the
        # defect from `source`, so an upstream fix brings the process back on the next run.
        broken = broken_upstream_reason(str(rel).replace("\\", "/"), source)
        if broken:
            excluded.append((str(rel), "broken_upstream", broken))
            continue

        out_of_scope = OUT_OF_SCOPE.get(str(rel).replace("\\", "/"))
        if out_of_scope:
            excluded.append((str(rel), "out_of_scope", out_of_scope))
            continue

        # A `py/` script is only usable if the Python helper it drives needs nothing beyond
        # numpy/scipy/soundfile. Checked before the generic detectors so the reason names the
        # real obstacle -- "needs torch" is more use than "uses an interactive construct".
        py_interactive = False
        if top == "py":
            requirement = python_helper_requirements(path)
            if requirement is None:
                excluded.append((str(rel), "out_of_scope",
                                 "drives no Python helper; nothing here for this app to run"))
                continue
            helpers, extras, interactive = requirement
            if extras:
                excluded.append((str(rel), "out_of_scope",
                                 f"{helpers[0]} needs {', '.join(sorted(extras))}"))
                continue
            py_interactive = interactive

        # Each detector says which text it reads (see `CODE`/`RAW` above): a construct
        # detector must not read prose, and a string-contents detector must not have its
        # strings blanked out from under it.
        scannable = {CODE: code_only(source), RAW: source}
        rel_key = str(rel).replace("\\", "/")
        override = GUI_BLOCKING_OVERRIDES.get(rel_key)
        # A pause hoist answers `gui_blocking` the same way an override does, by removing the
        # construct rather than by arguing it is unreachable -- the block is rewritten out of
        # the copy that actually runs. See PAUSE_HOISTS.
        hoist = PAUSE_HOISTS.get(rel_key)
        # And a directory hoist answers it the same way again, for `chooseDirectory$`: the call
        # is rewritten out of the copy that runs. See DIRECTORY_HOISTS.
        dir_hoist = DIRECTORY_HOISTS.get(rel_key)
        hit = next(
            ((slug, why) for slug, pattern, why, over in EXCLUSIONS
             if pattern.search(scannable[over])
             # An override answers `gui_blocking` only. A script that also trips
             # `hardcoded_path` or `non_sound_input` is still excluded for that.
             and not (slug == "gui_blocking" and (override or hoist))
             # ...and a directory hoist answers `folder_chooser` alone, the same narrow way: the
             # call is rewritten out of the copy that runs, which says nothing about a `demo`
             # window or an editor. An override may answer it too -- four scripts reach the
             # chooser only as a fallback for a blank `FolderPath` field, which cannot be blank.
             and not (slug == "folder_chooser" and (dir_hoist or override))
             # ...except a listed image sonifier, whose Photo input this app *can* supply.
             # Deliberately narrow: it answers `non_sound_input` alone, so one of these that
             # also opened a GUI or hardcoded a path would still be excluded for that.
             and not (slug == "non_sound_input" and rel_key in PHOTO_INPUTS)),
            None,
        )
        if hit:
            detail = hit[1]
            # A script excluded as `non_sound_input` that reads a *Photo* is a candidate for
            # `PHOTO_INPUTS` -- the app can supply an image, unlike a TextGrid or a Table. Said
            # here so a new one upstream shows up in the report as something to look at, rather
            # than disappearing into a category of things nothing can be done about. Deliberately
            # not automatic: every photo entry that ships has been run against the real binary,
            # which is the same bar `GENERATORS` and `PAUSE_HOISTS` set.
            if hit[0] == "non_sound_input" and PHOTO_HINT_RE.search(source):
                detail += " -- reads a Photo; a candidate for PHOTO_INPUTS once verified"
            excluded.append((str(rel), hit[0], detail))
            continue

        # A script that never mentions the selected Sound is not a process on our input --
        # unless it is a listed generator, which builds one from material of its own.
        if rel_key not in GENERATORS and ("selected" not in source or '"Sound"' not in source):
            excluded.append((str(rel), "not_a_sound_process",
                             "never reads the selected Sound object"))
            continue

        params = parse_form(source)
        if isinstance(params, str):
            excluded.append((str(rel), "unparseable_form", params))
            continue

        # Appended before anything downstream reads `params`, so the folder lands after the
        # form's own fields on both the ordinary and the pause-hoisted path. No script needs both
        # hoists today; if one ever does, its folder sits between the form's params and the
        # pause block's, which is the order the two dialogs would have asked in anyway.
        if dir_hoist:
            built = build_directory_params(source, dir_hoist)
            # Loud, exactly as a stale override is: a variable that no longer exists means the
            # call this param stands in for has moved, and the entry would ship the modal.
            if isinstance(built, str):
                excluded.append((str(rel), "folder_chooser", f"directory hoist failed: {built}"))
                continue
            params.extend(built)

        if override:
            problem = apply_gui_override(params, override)
            # Loud, not silent: an override naming a param the script no longer has means
            # upstream renamed or removed it, and the reachability argument the override rests
            # on no longer holds. Shipping the entry anyway would ship the segfault.
            if problem:
                excluded.append((str(rel), "gui_blocking",
                                 f"override is stale: {problem}"))
                continue

        # Marked once per *script*, and applied below to every entry it produced -- a hoisted
        # optionmenu can turn one script into nine, and they all run the same source.
        needs_python_rewrite = counts_python_assignments(source) > 0
        first_new = len(processes)

        if hoist:
            built = build_hoisted_processes(rel, top, path.stem, source, params, hoist,
                                            2 if needs_two_sounds(source) else 1,
                                            extract_description(source))
            if isinstance(built, str):
                excluded.append((str(rel), "gui_blocking", f"pause hoist failed: {built}"))
                continue
            processes.extend(built)
        else:
            described = extract_description(source)
            # A chain rarely carries a Description block, but always numbers its stages.
            if not described:
                described = describe_chain(source)
            processes.append(Process(
                key=key_for(top, path.stem),
                bin=str(rel).replace("\\", "/"),
                title=title_for(path.stem),
                group=GROUP_DIRS[top],
                params=params,
                inputs=2 if needs_two_sounds(source) else 1,
                description=described,
                short_description=short_description_from(described, title_for(path.stem)),
                interactive=py_interactive,
                # An image sonifier reads a Photo and no Sound at all -- see PHOTO_INPUTS; a
                # synthesis script reads neither -- see ZERO_INPUT_DIR. `""` elsewhere lets the
                # emitter fall back to its wav/dual_wav default.
                input_kind=zero_or_photo_input_kind(rel_key),
            ))
        for proc in processes[first_new:]:
            proc.python_rewrite = needs_python_rewrite

        presets = extract_script_presets(source, params)
        if presets:
            index, custom, blocks = presets
            processes[-1].preset_param = index
            processes[-1].preset_custom_option = custom
            processes[-1].script_presets = blocks
            # Renamed *after* extraction, which maps assignments back to fields by their
            # original label. The dialog already has a Preset row of its own -- tui-wave's saved
            # parameter sets -- so two rows called "Preset" sat one above the other with no way
            # to tell which was which. This one is the script's own, hence "Internal".
            params[index].name = "Internal Preset"

    return processes, excluded


# Processes tui-wave ships itself, appended to every generated catalog.
#
# They are emitted here rather than kept in a hand-authored file so that re-running this
# converter after a submodule bump cannot drop them: this file is regenerated wholesale, and
# anything not emitted here simply stops existing. The scripts they name live in `assets/praat/`
# in this repository and are compiled into the binary (`src/model/praat/builtin.rs`) -- nothing
# is ever added to the submodule, which a `git submodule update` would discard.
#
# **Record** exists because praatAudioTools captures from the microphone in ten scripts -- every
# `Vector Chain/Live_*` -- and never on its own: the capture is always welded to the processing
# chain that follows it, so there is no way to reach it without also getting a Neural Drone and
# a Crystalline Reverb. The capture itself is one statement, identical in all ten.
#
# Its `bin` names a path that does not exist in the submodule and is never resolved against it
# (`praat_builtin = true` is what says so). The directory segment is load-bearing all the same:
# `cdp_group` derives a Praat process's browser group from it, so this is what files Record under
# **Generative** -- whose processes `App::praat_opens_new_buffer` already sends to a new buffer,
# which is what a recording should be. It is new material, not an edit of the selection.
#
# `input = "none"` is what lets it run with no document open, which is the whole point: needing
# something already loaded before you could record would defeat the feature.
BUILTINS = [
    Process(
        key="praat_generative_synthesis_tui_wave_record",
        bin="Generative & Synthesis/record.praat",
        title="Record",
        group="Generative",
        builtin=True,
        input_kind="none",
        short_description="Record from the system's selected input device into a new buffer",
        description=(
            "Records from the microphone for a fixed time and opens the result as a new buffer. "
            "Captures from whichever input device the system sound settings currently select. "
            "Needs no open file and ignores any selection -- a recording is new material, not an "
            "edit of what you had. Requires Praat, like every process in this group.\n\n"
            "Sample rate should usually match the session you intend to mix this into; a 44100 "
            "take dropped into a 96000 project plays at the wrong pitch and speed. Input gain is "
            "Praat's own capture gain, 1.0 being unattenuated."
        ),
        params=[
            Param(
                name="Duration_seconds_(0.1-3600)",
                kind="number",
                default=10.0,
                minimum=0.1,
                maximum=3600.0,
                step=0.1,
            ),
            # A choice, not a free-text field. Praat takes the rate as a string and the driver
            # passes a chosen option's *label* verbatim, so the two forms are identical on the
            # wire -- but a rate is picked from the handful a device actually runs at, not typed,
            # and a typo in a text field would surface as a failed run rather than as a wrong
            # value you can see. These are the standard rates; 44100 first, matching the rate the
            # Live_* scripts hardcode.
            Param(
                name="Sample_rate",
                kind="choice",
                default=0,
                options=["44100", "48000", "88200", "96000", "192000", "22050", "16000", "8000"],
            ),
            Param(
                name="Input_gain_(0-1)",
                kind="number",
                default=1.0,
                minimum=0.0,
                maximum=1.0,
                step=0.01,
            ),
        ],
    ),
]


# A Praat string literal that names a Python interpreter rather than merely mentioning one.
# The Rust side (`model::praat::python::is_interpreter_literal`) applies exactly this rule to
# exactly these scripts, and its `no_real_script_can_still_reach_a_hardcoded_interpreter` test is
# what keeps the two honest -- this only has to decide *whether* a script needs the treatment.
PYTHON_LITERAL_RE = re.compile(r'^(?:.*[/\\])?(?:py|python[0-9.]*)(?:\.exe)?$')
PYTHON_ASSIGN_RE = re.compile(r'^\s*[A-Za-z_][A-Za-z0-9_]*\$\s*=\s*"([^"]*)"\s*$')


def counts_python_assignments(source: str) -> int:
    """How many bare-literal interpreter assignments a script makes.

    Why any of this exists: these scripts pick their own interpreter, and on macOS they pick an
    *absolute* path (`/opt/homebrew/bin/python3`, `/Library/Frameworks/...`), which no amount of
    PATH manipulation can influence. tui-wave installs the `py` group's numpy/scipy/soundfile
    into a venv it owns and puts that venv on the child's PATH -- a mechanism that therefore
    works on Linux, where the fallback is a bare `python3`, and silently does nothing on a Mac.
    Flagging the scripts here is what lets the app repoint them at run time.
    """
    count = 0
    for line in source.splitlines():
        match = PYTHON_ASSIGN_RE.match(line)
        if not match:
            continue
        literal = match.group(1)
        if literal and not any(c.isspace() for c in literal) and PYTHON_LITERAL_RE.match(literal):
            count += 1
    return count


def note_rows(params: list[Param]) -> list[tuple[int, Note]]:
    """Flatten per-param notes into the (before, note) pairs the catalog stores.

    `before` is the index of the param the note renders above; `len(params)` means "below the
    last field" and 0 means "above the dialog's own Preset row". A `notes_after` entry becomes
    `before = i + 1`, which is the same statement -- the note sits above whatever comes next --
    so a form's trailing comment stays put even when a hoisted pause block appends fields
    behind it.
    """
    rows: list[tuple[int, Note]] = []
    for i, param in enumerate(params):
        rows.extend((i, note) for note in param.notes)
        rows.extend((i + 1, note) for note in param.notes_after)
    return rows


def render_catalog(processes: list[Process], sha: str) -> str:
    out: list[str] = []
    out.append("# Generated by scripts/convert_praat_audiotools.py from the praatAudioTools")
    out.append("# plugin (MIT license, (c) Shai Cohen) -- see THIRD_PARTY_NOTICES.md.")
    out.append(f"# Source submodule third_party/praat-audiotools at commit {sha}.")
    out.append("# Do not hand-edit; re-run the converter instead. To add or override a process")
    out.append("# without touching this file, add a *.toml with the same [[process]] schema to")
    out.append("# $XDG_CONFIG_HOME/tui-wave/cdp/.")
    out.append("")

    for proc in processes:
        out.append("[[process]]")
        out.append(f"key = {toml_string(proc.key)}")
        out.append(f"bin = {toml_string(proc.bin)}")
        out.append(f"title = {toml_string(proc.title)}")
        out.append('category = "praat"')
        out.append(f"subcategory = {toml_string(proc.group)}")
        out.append(f"short_description = {toml_string(proc.short_description or proc.title)}")
        out.append(f"description = {toml_string(proc.description or proc.title)}")
        if proc.interactive:
            out.append("interactive = true")
        if proc.builtin:
            out.append("praat_builtin = true")
        if proc.python_rewrite:
            out.append("praat_python_rewrite = true")
        if proc.form_locks:
            pairs = ", ".join(f"[{toml_string(n)}, {'true' if v else 'false'}]"
                              for n, v in proc.form_locks)
            out.append(f"praat_form_locks = [{pairs}]")
        input_kind = proc.input_kind or ("dual_wav" if proc.inputs == 2 else "wav")
        out.append(f'input = "{input_kind}"')
        out.append('output = "wav"')
        # Praat reads a multi-channel Sound natively, so there is never a reason to split a
        # stereo input into per-channel lanes the way a mono-only CDP binary needs.
        out.append("stereo_native = true")
        out.append("output_is_stereo = false")
        if proc.preset_param is not None:
            out.append(f"preset_param = {proc.preset_param}")
            out.append(f"preset_custom_option = {proc.preset_custom_option}")
        # One line per note rather than a `[[process.param_notes]]` block each: there are ~2400
        # of them across the catalog, and four lines apiece would roughly double a generated
        # file nobody hand-edits anyway.
        rows = note_rows(proc.params)
        if rows:
            out.append("param_notes = [")
            for before, note in rows:
                section = ", section = true" if note.section else ""
                out.append(f"  {{ before = {before}, text = {toml_string(note.text)}{section} }},")
            out.append("]")
        out.append("")

        # Praat option indices are 1-based; the catalog's are 0-based.
        for option in sorted(proc.script_presets):
            values = proc.script_presets[option]
            keys = sorted(values)
            out.append("[[process.script_presets]]")
            out.append(f"option = {option - 1}")
            out.append("params = [" + ", ".join(str(k) for k in keys) + "]")
            out.append("values = [" + ", ".join(toml_number(values[k]) for k in keys) + "]")
            out.append("")

        for param in proc.params:
            out.append("[[process.params]]")
            out.append(f"name = {toml_string(param.name)}")
            if param.pause_block is not None:
                out.append(f"praat_pause_block = {param.pause_block}")
            if param.directory_var:
                out.append(f"praat_directory_var = {toml_string(param.directory_var)}")
            if param.key_value_group:
                out.append(f"key_value_group = {toml_string(param.key_value_group)}")
                out.append(f"key_value_key = {toml_string(param.key_value_key)}")
            out.append('description = ""')
            out.append("automatable = false")
            if param.kind == "number":
                out.append('kind = "number"')
                out.append(f"min = {toml_number(param.minimum)}")
                out.append(f"max = {toml_number(param.maximum)}")
                out.append(f"step = {toml_number(param.step)}")
                out.append(f"default = {toml_number(param.default)}")
                out.append("exponential = false")
                out.append('scale = "plain"')
                if param.integer:
                    out.append("integer = true")
            elif param.kind == "text":
                out.append('kind = "text"')
                out.append(f"default = {toml_string(param.text_default)}")
            elif param.kind == "folder_path":
                out.append('kind = "folder_path"')
            elif param.kind == "file_path":
                out.append('kind = "file_path"')
                out.append(f"extension = {toml_string(param.text_default)}")
            elif param.kind == "number_list":
                out.append('kind = "number_list"')
                # The delimiter is written as a TOML string so a leading/trailing space
                # survives -- ", " and "," are different fields to the receiving script, and
                # stripping one into the other would silently reshape its input.
                out.append(f"separator = {toml_string(param.list_separator)}")
                out.append(f"min = {toml_number(param.minimum)}")
                out.append(f"max = {toml_number(param.maximum)}")
                out.append(f"step = {toml_number(param.step)}")
                joined = ", ".join(toml_number(v) for v in param.list_default)
                out.append(f"default = [{joined}]")
                if param.integer:
                    out.append("integer = true")
            elif param.kind == "toggle":
                out.append('kind = "toggle"')
                out.append(f"default = {'true' if param.default else 'false'}")
            else:
                out.append('kind = "choice"')
                joined = ", ".join(toml_string(o) for o in param.options)
                out.append(f"options = [{joined}]")
                out.append(f"default = {int(param.default)}")
            out.append("")

    return "\n".join(out).rstrip() + "\n"


def render_report(processes: list[Process], excluded: list[tuple[str, str, str]], sha: str) -> str:
    by_reason: dict[str, list[tuple[str, str]]] = {}
    for rel, slug, detail in excluded:
        by_reason.setdefault(slug, []).append((rel, detail))

    lines: list[str] = []
    lines.append("# praatAudioTools scripts excluded from the catalog")
    lines.append("")
    lines.append("Generated by `scripts/convert_praat_audiotools.py`; do not hand-edit.")
    lines.append("")
    lines.append(f"Source: `third_party/praat-audiotools` at `{sha}`.")
    lines.append("")
    lines.append(f"**{len(processes)} scripts included, {len(excluded)} excluded.**")
    lines.append("")
    lines.append("Recorded rather than silently dropped, so the exclusion set stays reviewable")
    lines.append("and so an upstream fix can be re-tested against a named list.")
    lines.append("")
    for slug in sorted(by_reason):
        entries = sorted(by_reason[slug])
        lines.append(f"## `{slug}` ({len(entries)})")
        lines.append("")
        # Per-script detail, not just the category's first one: within `unparseable_form`
        # especially, every script fails for its own reason, and collapsing them to one
        # example hides exactly the information that would let someone fix the converter.
        details = {detail for _, detail in entries}
        if len(details) == 1:
            lines.append(details.pop())
            lines.append("")
            for rel, _ in entries:
                lines.append(f"- `{rel}`")
        else:
            for rel, detail in entries:
                lines.append(f"- `{rel}` — {detail}")
        lines.append("")
    return "\n".join(lines).rstrip() + "\n"


def main() -> int:
    # The parser self-test runs on every conversion, not just when asked for. It costs
    # milliseconds, and the failure it guards against is silent: a parser that stops
    # understanding an idiom does not crash, it drops the affected scripts and writes a catalog
    # that looks fine. Both cases it covers reached the catalog as "excluded, reason misleading"
    # rather than as an error.
    if selftest() != 0:
        print("error: parser self-test failed -- refusing to write a catalog", file=sys.stderr)
        return 1

    if not PLUGIN.is_dir() or not any(PLUGIN.iterdir()):
        print(f"error: {PLUGIN} is missing or empty -- run: git submodule update --init",
              file=sys.stderr)
        return 1

    sha = submodule_sha()

    stale = check_stale_keys()
    if stale:
        print(f"warning: {len(stale)} hand-maintained key(s) no longer match any script -- "
              f"upstream probably renamed them. The scripts they name have fallen back to the "
              f"generic path, which for PHOTO_INPUTS/GENERATORS means being EXCLUDED:",
              file=sys.stderr)
        for line in stale:
            print(f"  {line}", file=sys.stderr)

    processes, excluded = collect()

    # Must happen before anything is written: a repeated key is an *override* to
    # `CdpCatalog::load`, not an error, so a collision silently deletes a process from the
    # browser rather than failing anywhere.
    renamed = disambiguate(processes)
    if renamed:
        print(f"note: {len(renamed)} script(s) renamed to avoid a key collision "
              f"(upstream ships near-identical filenames):", file=sys.stderr)
        for line in renamed:
            print(f"  {line}", file=sys.stderr)

    # After `disambiguate`, so a built-in's key is never rewritten to dodge a collision with an
    # upstream script — `builtin.rs` matches on the literal key, and a renamed one would compile
    # fine and then fail to run. A genuine collision should be loud, and the assert below is it.
    processes.extend(BUILTINS)

    keys = [p.key for p in processes]
    assert len(keys) == len(set(keys)), "disambiguate() left duplicate keys"

    OUT_CATALOG.write_text(render_catalog(processes, sha), encoding="utf-8")
    OUT_REPORT.parent.mkdir(parents=True, exist_ok=True)
    OUT_REPORT.write_text(render_report(processes, excluded, sha), encoding="utf-8")

    print(f"{len(processes)} processes -> {OUT_CATALOG.relative_to(REPO)}")
    print(f"{len(excluded)} excluded  -> {OUT_REPORT.relative_to(REPO)}")
    groups: dict[str, int] = {}
    for proc in processes:
        groups[proc.group] = groups.get(proc.group, 0) + 1
    for group in sorted(groups):
        print(f"  {groups[group]:4d}  {group}")
    return 0


def selftest() -> int:
    """Parser regression cases, run with `--selftest`.

    These pin the *idioms* rather than any one script. Both were found the hard way: a script
    arrived using them, the parser silently produced nothing usable, and the entry was excluded
    with a message that pointed at the symptom rather than the cause. Nothing else in this repo
    exercises the parser directly -- the catalog it writes is checked downstream by `cargo test`
    and the praat smoke sweep, which would catch a regression only as entries quietly vanishing
    from a catalog diff at the next bump.
    """
    failures: list[str] = []

    def check(label: str, got, want) -> None:
        if got != want:
            failures.append(f"{label}\n     got:  {got!r}\n     want: {want!r}")

    # --- endPause assignment targets -------------------------------------------------
    # Praat procedure-local variables carry a leading dot. Allowing only `\w+` matched no block
    # at all, so a script with two pause pages reported "no beginPause block found".
    for target, label in [
        (".clicked = ", "procedure-local (.clicked)"),
        ("clicked = ", "plain (clicked)"),
        ("", "bare endPause, no assignment"),
    ]:
        source = (
            'beginPause: "Page"\n'
            '    positive: "Grain_ms", 60\n'
            f'{target}endPause: "Cancel", "Continue", 2, 1\n'
        )
        blocks = find_pause_blocks(source)
        check(f"endPause target -- {label}: one block found", len(blocks), 1)
        if blocks:
            check(f"endPause target -- {label}: default button", blocks[0]["default_button"], 2)

    # --- string$() defaults ----------------------------------------------------------
    # A numeric pause field wants its default as text, so the idiom is
    # `positive: "X", string$(x)`. Resolving only a bare name rejected every such field as a
    # non-numeric default and took the whole script with it.
    check(
        "string$ unwrap -- bare identifier",
        unwrap_string_cast("string$(minimum_frequency_Hz)"),
        "minimum_frequency_Hz",
    )
    check("string$ unwrap -- whitespace tolerated", unwrap_string_cast("string$( x )"), "x")
    check("string$ unwrap -- plain name untouched", unwrap_string_cast("voices"), "voices")
    check("string$ unwrap -- literal untouched", unwrap_string_cast("60"), "60")
    # Deliberately NOT unwrapped: this parser cannot evaluate these, and guessing is worse than
    # the loud "non-numeric default" the caller reports.
    for expr in ("string$(a + b)", "fixed$(x, 2)", "string$(f(x))", "string$(x) + y"):
        check(f"string$ unwrap -- {expr} left alone", unwrap_string_cast(expr), expr)

    # --- the two together, which is the shape that actually arrived ------------------
    # The variable is assigned above the block, exactly as the real scripts do -- that is what
    # `script_variables` reads to resolve the seeded default.
    source = (
        "minimum_frequency_Hz = 80\n"
        "if edit_analysis_settings\n"
        '    beginPause: "Analysis"\n'
        '        positive: "Minimum_frequency_Hz", string$(minimum_frequency_Hz)\n'
        '    .clicked = endPause: "Cancel", "Continue", 2, 1\n'
        "endif\n"
    )
    blocks = find_pause_blocks(source)
    if len(blocks) != 1:
        failures.append(f"combined case: expected one block, got {len(blocks)}")
    else:
        fields = blocks[0]["fields"]
        if isinstance(fields, str):
            failures.append(f"combined case: the block refused to parse -- {fields}")
        else:
            check("combined case: one field", len(fields), 1)
            check("combined case: default resolved", fields[0].default if fields else None, 80.0)
            check("combined case: default button", blocks[0]["default_button"], 2)

    for failure in failures:
        print(f"  FAIL  {failure}")
    print(f"selftest: {'FAILED' if failures else 'ok'} ({len(failures)} failure(s))")
    return 1 if failures else 0


if __name__ == "__main__":
    if "--selftest" in sys.argv:
        raise SystemExit(selftest())
    raise SystemExit(main())
