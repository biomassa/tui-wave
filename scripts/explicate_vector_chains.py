#!/usr/bin/env python3
"""Explicate the praatAudioTools Vector Chain scripts, and recreate them as tui-wave chains.

    python3 scripts/explicate_vector_chains.py                 # write the docs report
    python3 scripts/explicate_vector_chains.py --chains DIR     # ...and chain presets into DIR
    python3 scripts/explicate_vector_chains.py --no-report --chains DIR   # presets only

A Vector Chain script is a fixed pipeline: it runs three or four other AudioTools scripts in
sequence via `runScript:`, each with a hardcoded argument list. Praat fills a `form`
**positionally**, so those lists are the substance of the chain and also its main hazard -- an
argument list that does not match its target's form does not fail, it binds values to the wrong
fields. The report pairs every argument with the field it lands in and flags the mismatches; two
chains (`chain_5`, `Live_5`) turn out to carry exactly that defect, and it is the same one the
Praat smoke sweep reports as "Found 17 arguments but expected more".

`--chains` writes the same pipelines as `model::cdp::chain_preset` TOML, so they appear in the
Process Chain dialog. Not every chain can be expressed, and each refusal is a real property of
the chain rather than a gap here:

* the `*_Random` variants compute their arguments at run time, so there are no fixed values to
  save -- a preset would be a snapshot of the one thing about them that is meant to vary;
* a stage whose argument count disagrees with its target's form is the positional-binding defect
  above, and writing a preset from it would bake the misalignment in rather than report it;
* a stage that reads something a chain step cannot supply.

What a chain step can be: an ordinary process; a **generator** (a process that reads no input,
whose own sound becomes the running audio); and a **multi-input** process such as Stereo Mixer,
whose first input is the running audio and whose further inputs are branches. The script says
which sounds a stage reads with `selectObject:`/`plusObject:` before its `runScript:`, and that
is how a generator that feeds a mixer becomes a branch of the mixer rather than a step of its own.

The model is the converter's own (`convert_praat_audiotools`), not a second reading of the
catalog, so a script's `form:` spelling, the toggles the converter locks, and the settings-page
rows it hoists are all seen exactly as the catalog sees them. A locked toggle's argument is
dropped (the app forces it on), and a hoisted row takes the value of the preset the chain picks,
or the script's own base value for Custom -- never the dialog's default preset.

Both outputs are derived from the submodule, so re-run this after a bump.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import convert_praat_audiotools as conv  # noqa: E402  (the converter is the one model of a script)

REPO = Path(__file__).resolve().parent.parent
PLUGIN = REPO / "third_party" / "praat-audiotools"
CHAINS = PLUGIN / "Vector Chain"
CATALOG = REPO / "src" / "model" / "cdp" / "praat_catalog.toml"
REPORT = REPO / "docs" / "vector-chain-stages.txt"

FIELD_RE = re.compile(
    r"^\s*(real|positive|integer|natural|boolean|word|sentence|text|optionmenu|choice)\s+(\S+)",
    re.I,
)
SKIP_RE = re.compile(r"^\s*(option|button|comment)\b", re.I)
NUMBER_RE = re.compile(r"^-?\d+(\.\d+)?$")

# Praat's own spellings of a boolean argument. A `form` boolean is declared 0/1 but a
# `runScript:` call may pass either, and the chains use both.
TRUE_WORDS = ("yes", "on", "true", "1")
FALSE_WORDS = ("no", "off", "false", "0")


def logical_lines(text: str) -> str:
    """Join Praat's `...` continuation lines back onto the statement they continue.

    Load-bearing, not cosmetic: a `runScript:` argument list is routinely split across three
    lines, and reading only the first gives a short argument count that is indistinguishable
    from a real arity bug. Six chains looked broken until this existed.
    """
    out: list[str] = []
    for raw in text.split("\n"):
        stripped = raw.strip()
        if stripped.startswith("...") and out:
            out[-1] = out[-1].rstrip().rstrip(",") + ", " + stripped[3:].strip()
        else:
            out.append(raw)
    return "\n".join(out)


def split_args(text: str) -> list[str]:
    """Split an argument list on commas outside double quotes."""
    args: list[str] = []
    current = ""
    quoted = False
    for char in text:
        if char == '"':
            quoted = not quoted
            current += char
        elif char == "," and not quoted:
            args.append(current.strip())
            current = ""
        else:
            current += char
    if current.strip():
        args.append(current.strip())
    return args


# The words the report has always used for a field's kind, so it reads the same for the
# scripts that were already parsed correctly.
_REPORT_KIND = {
    "number": "real", "toggle": "boolean", "choice": "optionmenu", "text": "sentence",
    "number_list": "sentence", "folder_path": "sentence", "file_path": "sentence",
}


def positional_fields(text: str) -> list[list] | None:
    """The script's first `form`, one entry per positional `runScript:` argument.

    Each entry is a list of the converter's `Param`s: normally one, but a `key=value` text field
    is a single argument that the converter splits into several numbers, so those travel
    together. `None` when the form cannot be read at all.
    """
    params = conv.parse_form(text)
    if isinstance(params, str):
        return None
    fields: list[list] = []
    for param in params:
        if param.key_value_group and fields and fields[-1][0].key_value_group == param.key_value_group:
            fields[-1].append(param)
        else:
            fields.append([param])
    return fields


def form_fields(path: Path) -> list[tuple[str, str]] | None:
    """(kind, name) for each field of the script's first `form`, in declaration order."""
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None
    fields = positional_fields(text)
    if fields is None:
        return None
    return [(_REPORT_KIND.get(group[0].kind, group[0].kind), group[0].key_value_group or group[0].name)
            for group in fields]


def load_model() -> dict[str, "conv.Process"]:
    """`bin` -> the converter's `Process` for it.

    A script the converter splits into several entries (a hoisted option menu that becomes one
    process per option) is left out: a chain stage names a script, not one of its variants.
    """
    processes, _excluded = conv.collect()
    errors, _renamed = conv.finalize(processes)
    if errors:
        raise SystemExit("error: " + "; ".join(errors))
    by_bin: dict[str, list] = {}
    for process in processes:
        by_bin.setdefault(process.bin, []).append(process)
    return {path: found[0] for path, found in by_bin.items() if len(found) == 1}


def stage_paths(text: str) -> dict[str, str]:
    """Stage-path variables, mapped to the script each one names.

    The chains build a stage path three ways -- a bare relative literal, a `pluginPath$ +`
    concatenation, and a `defaultDirectory$` fallback for when the first is not readable. All of
    them end in the quoted script name, and a fallback always names the same target as the
    assignment it backs up, so the first assignment seen is the answer.
    """
    paths: dict[str, str] = {}
    for match in re.finditer(r"^\s*(\w+\$)\s*=\s*(.+)$", text, re.M):
        variable, expression = match.group(1), match.group(2)
        if variable in paths:
            continue
        literals = re.findall(r'"([^"]*\.praat)"', expression)
        if literals:
            paths[variable] = literals[-1]
    return paths


def resolve(rel: str) -> Path:
    """`../Reverb/x.praat` is relative to the chains folder; `Reverb/x.praat`, which comes from a
    `pluginPath$ +` concatenation, is relative to the plugin root."""
    base = CHAINS if rel.startswith("../") else PLUGIN
    return (base / rel).resolve()


def stages(chain: Path) -> list[tuple[str, Path, list[str]]]:
    """(relative bin, resolved path, arguments) for each `runScript:` stage, in order."""
    text = logical_lines(chain.read_text(encoding="utf-8", errors="replace"))
    paths = stage_paths(text)
    found = []
    for match in re.finditer(r"^\s*runScript:\s*([^\n]+)$", text, re.M):
        parts = split_args(match.group(1))
        token, args = parts[0], parts[1:]
        rel = paths.get(token, token.strip('"'))
        target = resolve(rel)
        try:
            shown = str(target.relative_to(PLUGIN.resolve()))
        except ValueError:
            shown = rel
        found.append((shown, target, args))
    return found


# --- the report -----------------------------------------------------------------------


def explicate(chain: Path) -> str:
    lines = ["=" * 78, chain.name, "=" * 78]
    found = stages(chain)
    if not found:
        lines.append("  (no runScript: stages)")
        return "\n".join(lines)
    for number, (shown, target, args) in enumerate(found, 1):
        lines += ["", f"  Stage {number}: {shown}"]
        fields = form_fields(target)
        if fields is None:
            lines.append(f"    !! script not found: {target}")
            continue
        if len(fields) != len(args):
            lines.append(
                f"    !! ARITY MISMATCH: chain passes {len(args)} argument(s), "
                f"form declares {len(fields)} field(s)"
            )
        width = max((len(name) for _, name in fields), default=0)
        for i in range(max(len(fields), len(args))):
            kind, name = fields[i] if i < len(fields) else ("?", "(no such field)")
            value = args[i] if i < len(args) else "(not supplied)"
            lines.append(f"    {i + 1:>2}. {name:<{width}}  = {value}   [{kind}]")
    return "\n".join(lines)


def write_report() -> None:
    out = [
        "Vector Chain scripts -- stage-by-stage explication",
        "",
        "Generated by scripts/explicate_vector_chains.py from third_party/praat-audiotools.",
        "Do not hand-edit; re-run the generator instead.",
        "",
        "Each chain runs a sequence of other AudioTools scripts via `runScript:`, which Praat",
        "fills POSITIONALLY -- so an argument list that does not match its target's form does",
        "not fail, it binds values to the wrong fields. Mismatches are flagged ARITY MISMATCH.",
        "",
    ]
    for chain in sorted(CHAINS.glob("*.praat")):
        out += [explicate(chain), ""]
    REPORT.parent.mkdir(parents=True, exist_ok=True)
    REPORT.write_text("\n".join(out), encoding="utf-8")
    print(f"report -> {REPORT.relative_to(REPO)}")


# --- the chain presets ----------------------------------------------------------------


def stage_flow(chain: Path) -> list[list[int]]:
    """For each `runScript:` stage, which stages (or -1, the chain's own input) it reads.

    The script says it in the lines before the call: `selectObject: a` then `plusObject: b`
    select the sounds the stage receives, and `a = selected("Sound")` after a stage names its
    output. A stage with no readable selection reads the stage before it, which is what a
    plain linear chain means and what every chain here did before multi-input stages existed.
    """
    text = logical_lines(chain.read_text(encoding="utf-8", errors="replace"))
    source_of: dict[str, int] = {}
    selection: list[str] = []
    awaiting: int | None = None
    flows: list[list[int]] = []
    for line in text.split("\n"):
        code = line.strip()
        if (m := re.match(r"selectObject:\s*(\w+)\s*$", code)):
            selection = [m.group(1)]
        elif (m := re.match(r"plusObject:\s*(\w+)\s*$", code)):
            selection.append(m.group(1))
        elif (m := re.match(r'(\w+)\s*=\s*selected\s*\(\s*"Sound"\s*\)', code)):
            source_of[m.group(1)] = -1 if awaiting is None else awaiting
            selection, awaiting = [m.group(1)], None
        elif re.match(r"runScript:", code):
            inputs = [source_of[v] for v in selection if v in source_of]
            if len(inputs) != len(selection) or not inputs:
                inputs = [len(flows) - 1]
            flows.append(inputs)
            awaiting, selection = len(flows) - 1, []
    return flows


TRUE_TOML = ("true", "false")


def param_value(param, arg: str) -> tuple[tuple[str, object] | None, str | None]:
    """A `ParamValue` TOML fragment for `arg`, or a reason it cannot be expressed.

    `param` is the converter's `Param`.
    """
    if arg.startswith('"') and arg.endswith('"'):
        text = arg[1:-1]
        if param.kind == "choice":
            if text not in param.options:
                return None, f"{param.name}: option {text!r} is not in the catalog's options"
            return ("Choice", param.options.index(text)), None
        if param.kind == "toggle":
            low = text.strip().lower()
            if low in TRUE_WORDS:
                return ("Toggle", "true"), None
            if low in FALSE_WORDS:
                return ("Toggle", "false"), None
            return None, f"{param.name}: {text!r} is not a boolean"
        if param.kind in ("text", "folder_path"):
            escaped = text.replace("\\", "\\\\").replace('"', '\\"')
            return ("Text", f'"{escaped}"'), None
        if param.kind == "number_list":
            # The script's own delimiter, else whitespace or commas; every entry a number.
            pieces = [t for t in re.split(r"[\s,;]+", text.strip()) if t]
            if pieces and all(NUMBER_RE.match(t) for t in pieces):
                return ("List", "[" + ", ".join(repr(float(t)) for t in pieces) + "]"), None
            return None, f"{param.name}: {text!r} is not a list of numbers"
        return None, f"{param.name}: a string argument for a {param.kind} parameter"
    if not NUMBER_RE.match(arg):
        return None, f"{param.name}: {arg!r} is computed at run time, not a literal"
    number = float(arg)
    if param.kind == "toggle":
        return ("Toggle", "true" if number != 0 else "false"), None
    if param.kind == "choice":
        # A numeric optionmenu argument is Praat's 1-based option index.
        index = int(number) - 1
        if not 0 <= index < len(param.options):
            return None, f"{param.name}: option index {int(number)} is out of range"
        return ("Choice", index), None
    if param.kind == "number_list":
        return ("List", f"[{number!r}]"), None
    return ("Number", repr(number)), None


def default_value(param, value=None) -> tuple[str, object]:
    """A `ParamValue` fragment for a row the chain gives no argument for (a hoisted row)."""
    value = param.default if value is None else value
    if param.kind == "toggle":
        return ("Toggle", "true" if value else "false")
    if param.kind == "choice":
        return ("Choice", int(value))
    if param.kind == "number_list":
        return ("List", "[" + ", ".join(repr(float(v)) for v in param.list_default) + "]")
    return ("Number", repr(float(value)))


def stage_step(shown: str, text_path: Path, args: list[str], model: dict):
    """One chain stage as `(process key, values)`, or `(None, [reasons])`."""
    proc = model.get(shown)
    if proc is None:
        return None, [f"{shown}: not in the catalog"]
    kind = proc.input_kind or ("dual_wav" if proc.inputs == 2 else "wav")
    if kind not in ("wav", "none", "variadic_wav"):
        return None, [f"{proc.key}: input = {kind!r}, not chainable"]
    fields = positional_fields(text_path.read_text(encoding="utf-8", errors="replace"))
    if fields is None:
        return None, [f"{proc.key}: its form cannot be read"]
    if len(args) != len(fields):
        return None, [f"{proc.key}: chain passes {len(args)}, its form declares {len(fields)}"]
    # The positional arguments belong to the form's fields; the catalog's rows are those fields
    # minus the toggles the converter locked, plus the rows it hoisted out of settings pages.
    locked = {name for name, _ in proc.form_locks}
    form_rows = [i for i, p in enumerate(proc.params) if p.pause_block is None and not p.directory_var]
    kept = [(group, arg) for group, arg in zip(fields, args) if group[0].name not in locked]
    if len(kept) != len(form_rows):
        return None, [f"{proc.key}: {len(kept)} form field(s) against {len(form_rows)} catalog row(s)"]
    values: dict[int, tuple] = {}
    for (group, arg), row in zip(kept, form_rows):
        if len(group) > 1:
            return None, [f"{proc.key}: {group[0].key_value_group} is a key=value field"]
        value, why = param_value(proc.params[row], arg)
        if value is None:
            return None, [f"{proc.key}: {why}"]
        values[row] = value
    # Hoisted rows: the picked preset's table, else the script's own base value. The dialog's
    # defaults are the *default* preset's values, which is the wrong answer for any other pick.
    chosen = None
    if proc.preset_param is not None and proc.preset_param in values:
        kind_, picked = values[proc.preset_param]
        if kind_ == "Choice" and picked != proc.preset_custom_option:
            chosen = proc.script_presets.get(picked + 1)
    for index, param in enumerate(proc.params):
        if param.pause_block is None and not param.directory_var:
            continue
        if param.directory_var:
            return None, [f"{proc.key}: needs a folder picked ({param.name})"]
        if chosen and index in chosen:
            raw = chosen[index]
            values[index] = default_value(
                param, (raw != 0) if param.kind == "toggle" else (int(raw) - 1 if param.kind == "choice" else raw)
            )
        else:
            base = param.base_default if param.base_default is not None else param.default
            values[index] = default_value(param, base)
    return (proc.key, [values[i] for i in range(len(proc.params))]), None


def build_chain(chain: Path, model: dict):
    """The chain as a list of steps, each `(key, values, branches)`, or the reasons it cannot be."""
    found = stages(chain)
    flows = stage_flow(chain)
    if not found:
        return [], ["no runScript: stages"]
    if len(flows) != len(found):
        return [], ["the stage selections could not be followed"]
    problems: list[str] = []
    made: list = []
    for (shown, target, args) in found:
        step, why = stage_step(shown, target, args, model)
        if step is None:
            problems += why
        made.append(step)
    if problems:
        return [], problems

    # A chain whose every stage reads one sound is taken as the plain sequence it has always
    # been taken as. (The `Live_*` and `Composition_*` chains are not strictly that -- each runs
    # its stages on the original sound and joins the results afterwards -- but their presets
    # exist, and reading the selections more strictly would change them.) Only a stage that
    # really reads several sounds makes the translator follow who feeds whom.
    if all(len(flow) == 1 for flow in flows):
        return [(made[k][0], made[k][1], []) for k in range(len(found))], []

    # The spine is what the last stage reads, back to the chain's input. Anything off it must be
    # a generator feeding a multi-input stage, and then it becomes that stage's branch.
    spine: list[int] = []
    at = len(found) - 1
    while at != -1:
        if at in spine:
            return [], ["the stage selections loop"]
        spine.append(at)
        at = flows[at][0]
    spine.reverse()
    consumed: set[int] = set()
    steps = []
    for k in spine:
        branches = []
        for extra in flows[k][1:]:
            shown, _target, _args = found[extra]
            proc = model[shown]
            kind = proc.input_kind or ("dual_wav" if proc.inputs == 2 else "wav")
            if extra in spine or kind != "none":
                return [], [f"{proc.key}: feeds {made[k][0]} but is not a generator"]
            branches.append(("Tap", [(made[extra][0], made[extra][1], [])]))
            consumed.add(extra)
        steps.append((made[k][0], made[k][1], branches))
    stray = [i for i in range(len(found)) if i not in spine and i not in consumed]
    if stray:
        return [], [f"{made[i][0]}: its output is never used" for i in stray]
    return steps, []


def write_step(lines: list[str], step, prefix: str) -> None:
    key, values, branches = step
    lines += [f"[[{prefix}]]", f'process_key = "{key}"', "side_chain = []", ""]
    for kind, value in values:
        lines += [f"[[{prefix}.values]]", f"{kind} = {value}", ""]
    for source, inner in branches:
        lines += [f"[[{prefix}.branches]]", f'source = "{source}"', ""]
        for inner_step in inner:
            write_step(lines, inner_step, f"{prefix}.branches.steps")


def write_chains(out_dir: Path) -> None:
    model = load_model()
    out_dir.mkdir(parents=True, exist_ok=True)
    made, skipped = [], []
    for chain in sorted(CHAINS.glob("*.praat")):
        steps, problems = build_chain(chain, model)
        if problems or not steps:
            skipped.append((chain.name, problems or ["no runScript: stages"]))
            continue
        name = f"AudioTools {chain.stem}"
        lines = [f'name = "{name}"', ""]
        for step in steps:
            write_step(lines, step, "steps")
        # Matches `chain_preset::sanitize_name`, so the app finds the file by its own rule.
        safe = "".join(c if (c.isalnum() or c in "-_") else "_" for c in name)
        (out_dir / f"{safe}.toml").write_text("\n".join(lines).rstrip() + "\n", encoding="utf-8")
        made.append((name, len(steps)))

    for name, count in made:
        print(f"  ok    {name}  ({count} steps)")
    for name, problems in skipped:
        print(f"  skip  {name}")
        for problem in problems:
            print(f"          {problem}")
    print(f"{len(made)} chain preset(s) -> {out_dir}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--chains", metavar="DIR", help="also write chain presets into DIR")
    parser.add_argument(
        "--no-report", action="store_true",
        help="do not rewrite docs/vector-chain-stages.txt (for tests and dry runs)",
    )
    args = parser.parse_args()
    if not CHAINS.is_dir():
        print(f"error: {CHAINS} is missing -- run: git submodule update --init", file=sys.stderr)
        return 1
    if not args.no_report:
        write_report()
    if args.chains:
        write_chains(Path(args.chains).expanduser())
    return 0


if __name__ == "__main__":
    sys.exit(main())
