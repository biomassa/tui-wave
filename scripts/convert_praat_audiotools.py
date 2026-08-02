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

* **Ranges.** A Praat `form` declares a default and nothing else -- only 19 of 3286 numeric
  parameters carry a `(0-1)`-style hint. Bounds are therefore synthesised (see RANGE_FACTOR).
* **Which scripts work.** Roughly one in six cannot be driven headlessly at all. They are
  detected statically here (see EXCLUSIONS) rather than being discovered at runtime by the
  user.
"""

from __future__ import annotations

import re
import subprocess
import sys
from dataclasses import dataclass, field
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

# How much wider than its declared default a synthesised range runs. Deliberately generous:
# these are experimental-music processes and pushing a parameter well past its intended value
# is a legitimate use, while an out-of-range value costs only a clean Praat error.
RANGE_FACTOR = 10.0

# Parameters whose value must never be left on: `Play` blocks for the audio's real-time
# duration and cannot be suppressed from outside the process, and the Picture window a `Draw`
# writes to is unreachable from a TUI. Matched on the parameter name.
SILENCE_RE = re.compile(r"^(play|draw|show|visuali|demo|open_|export)", re.I)

# --- static exclusion detectors -------------------------------------------------------
# Each maps a reason slug to a pattern that, if found anywhere in a script, disqualifies it.

EXCLUSIONS: list[tuple[str, re.Pattern, str]] = [
    (
        "gui_blocking",
        # `View & Edit` and a bare `Edit` open an editor window, which batch mode refuses
        # outright ("Cannot edit a TextGrid from batch."). Anchored so the word `Edit` inside a
        # comment or a longer command name does not match.
        re.compile(r"\b(beginPause|pauseScript|demoShow|demoWaitForInput|chooseReadFile\$|"
                   r"chooseWriteFile\$|chooseFolder\$)|\bdemo\s|"
                   r"^\s*(View & Edit|Edit)\s*$", re.M),
        "uses an interactive/GUI construct that segfaults or hangs under --run",
    ),
    (
        "non_sound_input",
        re.compile(r"select a (Photo|TextGrid|Table|Matrix) object|"
                   r"Please select .*(Photo|TextGrid)", re.I),
        "operates on a non-Sound object",
    ),
    (
        "hardcoded_path",
        re.compile(r"\"[A-Za-z]:[\\/]|/home/[a-z]+/|\.praat-dir"),
        "contains a hardcoded absolute path that only resolves on its author's machine",
    ),
]

# Scripts that want two Sound objects selected rather than one -- morphing, concatenative
# synthesis, DTW alignment, pitch-contour transfer. They read them positionally, as
# `selected("Sound", 1)` and `selected("Sound", 2)`, which the driver satisfies by reading both
# inputs in order and selecting them together. Catalogued as `input = "dual_wav"`, reusing the
# CDP kind that already means "this process needs a second buffer".
#
# Detected from an **unindented** guard only. A looser prose match ("select 2 sounds", "at
# least two Sound") caught scripts that take one *or* two depending on a mode, because the
# phrase appears in a `comment` line or an `option` label -- and those then failed at run time
# with `Please select exactly ONE Sound object.`, since their default mode wants one. A guard at
# column zero is unconditional by construction: anything inside an `if` is indented.
DUAL_SOUND_RE = re.compile(
    r"^if\s+numberOfSelected\s*\(\s*\"Sound\"\s*\)\s*(<>|!=)\s*2\b|"
    r"^\s{0,2}numberOfSelected\s*\(\s*\"Sound\"\s*\)\s*(<>|!=)\s*2\b",
    re.M,
)

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


@dataclass
class Process:
    key: str
    bin: str
    title: str
    group: str
    params: list[Param]
    inputs: int = 1


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
            # A free-text field has no bounded editor in this app, and none of these carry a
            # value a user would want to vary anyway (they are labels and filenames).
            return f"parameter {name!r} is a free-text {keyword} field"
        else:
            return f"unsupported form field {keyword!r}"

    for param in params:
        if param.kind == "choice":
            if not param.options:
                return f"optionmenu {param.name!r} declares no options"
            param.default = min(max(int(param.default), 1), len(param.options)) - 1
    return params


def make_number(keyword: str, name: str, default: float) -> Param:
    """Synthesise a range around a declared default.

    Praat itself enforces only the type's floor -- `positive` is > 0 and `natural` is >= 1 --
    so those become the minimum. Everything else is invention, and is kept wide on purpose.
    A symmetric range around zero is used when the default is zero or negative, since such a
    parameter is almost always a bipolar offset (asymmetry, detune, pan) rather than a
    magnitude.
    """
    integer = keyword in ("integer", "natural")
    magnitude = abs(default)
    span = magnitude * RANGE_FACTOR
    if span == 0.0:
        # A zero default says nothing about scale. 1.0 covers the normalised 0-1 parameters
        # that dominate this collection, and stays sane for the rest.
        span = 10.0 if integer else 1.0

    if keyword == "positive":
        # Praat rejects anything <= 0 outright, so the floor must stay above it -- but it also
        # has to stay *below the default*, and some of these defaults are tiny (a silence floor
        # of 1.1e-9). A fixed 0.001 floor put min above max for those.
        minimum = min(0.001, magnitude / 1000.0) if magnitude > 0 else 1e-6
    elif keyword == "natural":
        minimum = 1.0
    elif integer and default >= 0:
        minimum = 0.0
    else:
        # A zero or negative default almost always marks a bipolar control (asymmetry, detune,
        # pan) rather than a magnitude, so the range straddles zero.
        minimum = -span

    maximum = max(span, default + span)
    if integer:
        minimum = float(int(minimum))
        maximum = float(int(maximum))
        step = 1.0
    else:
        step = round_step(maximum - minimum)

    # Enforce what `builtin_number_params_have_sane_ranges` checks, whatever the heuristic
    # above produced. The heuristic is invention; this invariant is not negotiable, and a
    # generated catalog must not be able to violate it for some input nobody anticipated.
    if maximum <= minimum:
        maximum = minimum + (1.0 if integer else max(abs(minimum), 1.0))
    default = min(max(default, minimum), maximum)

    return Param(
        name=name,
        kind="number",
        default=default,
        integer=integer,
        minimum=minimum,
        maximum=maximum,
        step=step,
    )


def round_step(span: float) -> float:
    """A step roughly 1/1000 of the range, snapped to a power of ten so the UI shows round
    numbers rather than values like 0.0037."""
    if span <= 0:
        return 0.01
    import math

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


def toml_string(value: str) -> str:
    escaped = value.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n")
    return f'"{escaped}"'


def toml_number(value: float) -> str:
    # Every numeric field on the Rust side is f64; emitting a bare `1` would deserialize as an
    # integer and fail. Always carry a decimal point.
    text = repr(float(value))
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

        hit = next(((slug, why) for slug, pattern, why in EXCLUSIONS if pattern.search(source)), None)
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

        processes.append(Process(
            key=key_for(top, path.stem),
            bin=str(rel).replace("\\", "/"),
            title=title_for(path.stem),
            group=GROUP_DIRS[top],
            params=params,
            inputs=2 if DUAL_SOUND_RE.search(source) else 1,
        ))

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
    if not PLUGIN.is_dir() or not any(PLUGIN.iterdir()):
        print(f"error: {PLUGIN} is missing or empty -- run: git submodule update --init",
              file=sys.stderr)
        return 1

    sha = submodule_sha()
    processes, excluded = collect()

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
