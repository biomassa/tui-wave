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

import math
import re
import subprocess
import sys
from dataclasses import dataclass, field
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
}

# `py/` shells out to Python/IRCAM/VST3 tooling and is out of scope entirely. `Max-MSP` is a
# single Max/MSP helper rather than a sound process.
#
# `Vector Chain` is *not* skipped: those scripts chain sibling scripts located through
# `preferencesDirectory$`, which the runner satisfies by pointing Praat at an app-owned
# preferences directory holding a `plugin_AudioTools` symlink (see `prepare_prefs_dir`).
SKIP_DIRS = {"py", "Max-MSP"}

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
#   lock_off      -- name a `boolean` param that must never be ticked. It stays in the catalog
#                    and is still emitted, always as 0, and the app greys it (see
#                    `ParamDef.opens_praat_dialog`). It CANNOT simply be dropped: Praat fills a
#                    script's `form` positionally from `runScript:`'s arguments, so removing one
#                    shifts every argument after it and feeds each field its neighbour's value.
#   drop_options  -- name an `optionmenu` param and the option labels to remove. Safe to drop
#                    outright, because Praat matches an optionmenu by *label* and the catalog
#                    stores labels verbatim, so nothing is positional here.
GUI_BLOCKING_OVERRIDES: dict[str, dict] = {
    # Three scripts of the same shape: `boolean Show_advanced_settings 0` guarding a
    # `beginPause` block of advanced parameters, whose values are already assigned as plain
    # variables immediately above the `if`. With the box unticked -- which is the script's own
    # default, and what `SILENCE_RE` would force anyway -- the block is never entered and those
    # defaults stand. Locking it off makes that the only reachable state.
    "Generative & Synthesis/FM_Texture_Generator.praat": {
        "lock_off": ["Show_Advanced_Settings"],
        "why": "beginPause 'Advanced DX7 Parameters' is guarded by Show_Advanced_Settings, "
               "which the script itself defaults to 0",
    },
    "Time & Granular/HFD-Driven_Time_Warping.praat": {
        "lock_off": ["Show_advanced_settings"],
        "why": "beginPause 'Advanced HFD Parameters' is guarded by Show_advanced_settings, "
               "which the script itself defaults to 0",
    },
    "Time & Granular/Magnetic_Tape_Degradation.praat": {
        "lock_off": ["Show_advanced_settings"],
        "why": "beginPause 'Advanced Tape Physics' is guarded by Show_advanced_settings, "
               "which the script itself defaults to 0",
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

def apply_gui_override(params: list, override: dict) -> str | None:
    """Apply one `GUI_BLOCKING_OVERRIDES` entry to a parsed form. Returns a problem, or None.

    Every name in the override must resolve. A silent miss would leave the blocking construct
    reachable while the entry claims otherwise, which is the one failure mode that costs a
    segfault rather than an error message -- so a stale override excludes the script instead.
    """
    by_name = {p.name: p for p in params}

    for name in override.get("lock_off", []):
        param = by_name.get(name)
        if param is None:
            return f"no parameter named {name!r} (upstream renamed or removed it?)"
        if param.kind != "toggle":
            return f"parameter {name!r} is a {param.kind}, not a toggle"
        param.default = False
        param.locked_off = True

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


def praat_variable(label: str) -> str:
    """The script variable Praat derives from a form label: the label with its first letter
    lowercased. `Hysteresis_Memory` is read back as `hysteresis_Memory`."""
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
# Declared in a form but meaningless as an argument.
IGNORED_KEYWORDS = {"comment"}


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
    # Set from GUI_BLOCKING_OVERRIDES' `lock_off`: a toggle that must never be ticked because
    # doing so opens a Praat dialog. Emitted as `opens_praat_dialog = true`; the app greys the
    # row and refuses the key. Never dropped -- see the override table for why.
    locked_off: bool = False
    # Set for kind == "number_list": the script's own delimiter, written back verbatim, and
    # the default entries parsed out of its form declaration.
    list_separator: str = ""
    list_default: list[float] = field(default_factory=list)


@dataclass
class Process:
    key: str
    bin: str
    title: str
    group: str
    params: list[Param]
    inputs: int = 1
    preset_param: int | None = None
    preset_custom_option: int = 0
    # option index -> {param index: value}
    script_presets: dict[int, dict[int, float]] = field(default_factory=dict)


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


def parse_form(source: str) -> list[Param] | str:
    """Parse a script's form block into parameters, or return a reason string on failure."""
    match = FORM_RE.search(source)
    if not match:
        return []
    params: list[Param] = []
    pending: Param | None = None

    for raw in match.group(1).splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        head, _, rest = line.partition(" ")
        # A trailing colon on the *keyword* marks the modern syntax, which quotes its operands.
        colon_form = head.endswith(":")
        keyword = head.rstrip(":").lower()

        if keyword in IGNORED_KEYWORDS:
            continue

        if keyword in ("option", "button"):
            if pending is not None:
                pending.options.append(unquote(rest, colon_form))
            continue

        if keyword in CHOICE_KEYWORDS:
            bits = rest.split()
            if not bits:
                return f"malformed {keyword} declaration: {line!r}"
            name = unquote(bits[0].rstrip(",").rstrip(":"), colon_form)
            index_text = unquote(bits[1].rstrip(","), colon_form) if len(bits) > 1 else "1"
            try:
                index = int(float(index_text))
            except ValueError:
                index = 1
            pending = Param(name=name, kind="choice", default=index, options=[])
            params.append(pending)
            continue

        pending = None
        bits = rest.split(None, 1)
        if not bits:
            return f"malformed declaration: {line!r}"
        name = unquote(bits[0].rstrip(",").rstrip(":"), colon_form)
        value_text = unquote(bits[1].strip().lstrip(",").strip(), colon_form) if len(bits) > 1 else ""

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

        # Each detector says which text it reads (see `CODE`/`RAW` above): a construct
        # detector must not read prose, and a string-contents detector must not have its
        # strings blanked out from under it.
        scannable = {CODE: code_only(source), RAW: source}
        override = GUI_BLOCKING_OVERRIDES.get(str(rel).replace("\\", "/"))
        hit = next(
            ((slug, why) for slug, pattern, why, over in EXCLUSIONS
             if pattern.search(scannable[over])
             # An override answers `gui_blocking` only. A script that also trips
             # `hardcoded_path` or `non_sound_input` is still excluded for that.
             and not (slug == "gui_blocking" and override)),
            None,
        )
        if hit:
            excluded.append((str(rel), hit[0], hit[1]))
            continue

        # A script that never mentions the selected Sound is not a process on our input.
        if "selected" not in source or '"Sound"' not in source:
            excluded.append((str(rel), "not_a_sound_process",
                             "never reads the selected Sound object"))
            continue

        params = parse_form(source)
        if isinstance(params, str):
            excluded.append((str(rel), "unparseable_form", params))
            continue

        if override:
            problem = apply_gui_override(params, override)
            # Loud, not silent: an override naming a param the script no longer has means
            # upstream renamed or removed it, and the reachability argument the override rests
            # on no longer holds. Shipping the entry anyway would ship the segfault.
            if problem:
                excluded.append((str(rel), "gui_blocking",
                                 f"override is stale: {problem}"))
                continue

        processes.append(Process(
            key=key_for(top, path.stem),
            bin=str(rel).replace("\\", "/"),
            title=title_for(path.stem),
            group=GROUP_DIRS[top],
            params=params,
            inputs=2 if needs_two_sounds(source) else 1,
        ))
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
        out.append(f"short_description = {toml_string(proc.title)}")
        out.append(f"description = {toml_string(proc.title)}")
        out.append(f'input = "{"dual_wav" if proc.inputs == 2 else "wav"}"')
        out.append('output = "wav"')
        # Praat reads a multi-channel Sound natively, so there is never a reason to split a
        # stereo input into per-channel lanes the way a mono-only CDP binary needs.
        out.append("stereo_native = true")
        out.append("output_is_stereo = false")
        if proc.preset_param is not None:
            out.append(f"preset_param = {proc.preset_param}")
            out.append(f"preset_custom_option = {proc.preset_custom_option}")
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
                if param.locked_off:
                    out.append("opens_praat_dialog = true")
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
    if not PLUGIN.is_dir() or not any(PLUGIN.iterdir()):
        print(f"error: {PLUGIN} is missing or empty -- run: git submodule update --init",
              file=sys.stderr)
        return 1

    sha = submodule_sha()
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


if __name__ == "__main__":
    raise SystemExit(main())
