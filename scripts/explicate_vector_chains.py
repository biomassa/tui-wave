#!/usr/bin/env python3
"""Explicate the praatAudioTools Vector Chain scripts, and recreate them as tui-wave chains.

    python3 scripts/explicate_vector_chains.py                 # write the docs report
    python3 scripts/explicate_vector_chains.py --chains DIR     # ...and chain presets into DIR

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
* a stage whose target declares `input = "none"` (a generator) is not chainable, because a chain
  step has to consume the previous step's audio;
* a stage whose argument count disagrees with its target's form is the positional-binding defect
  above, and writing a preset from it would bake the misalignment in rather than report it.

Both outputs are derived from the submodule, so re-run this after a bump.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

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


def form_fields(path: Path) -> list[tuple[str, str]] | None:
    """(kind, name) for each field of the script's first `form`, in declaration order."""
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None
    fields: list[tuple[str, str]] = []
    inside = False
    for line in text.split("\n"):
        if not inside:
            if re.match(r"^\s*form\b", line, re.I):
                inside = True
            continue
        if re.match(r"^\s*endform\b", line, re.I):
            break
        if SKIP_RE.match(line):
            continue
        match = FIELD_RE.match(line)
        if match:
            fields.append((match.group(1).lower(), match.group(2)))
    return fields


def load_catalog() -> dict[str, dict]:
    """`bin` -> the entry's key, input kind and parameter list, read from the shipped catalog."""
    catalog: dict[str, dict] = {}
    for block in CATALOG.read_text(encoding="utf-8").split("[[process]]")[1:]:
        head = block.split("[[process.params]]")[0]
        key = re.search(r'key = "(.+)"', head)
        binary = re.search(r'bin = "(.+)"', head)
        if not (key and binary):
            continue
        kind = re.search(r'input = "(.+)"', head)
        params = []
        for part in block.split("[[process.params]]")[1:]:
            options = re.search(r"^options = \[(.*)\]$", part, re.M)
            params.append({
                "name": re.search(r'name = "(.+)"', part).group(1),
                "kind": re.search(r'kind = "(.+)"', part).group(1),
                "options": re.findall(r'"((?:[^"\\]|\\.)*)"', options.group(1)) if options else [],
            })
        catalog[binary.group(1)] = {
            "key": key.group(1),
            "input": kind.group(1) if kind else "wav",
            "params": params,
        }
    return catalog


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


def param_value(param: dict, arg: str) -> tuple[tuple[str, object] | None, str | None]:
    """A `ParamValue` TOML fragment for `arg`, or a reason it cannot be expressed."""
    if arg.startswith('"') and arg.endswith('"'):
        text = arg[1:-1]
        if param["kind"] == "choice":
            if text not in param["options"]:
                return None, f"{param['name']}: option {text!r} is not in the catalog's options"
            return ("Choice", param["options"].index(text)), None
        if param["kind"] == "toggle":
            low = text.strip().lower()
            if low in TRUE_WORDS:
                return ("Toggle", "true"), None
            if low in FALSE_WORDS:
                return ("Toggle", "false"), None
            return None, f"{param['name']}: {text!r} is not a boolean"
        if param["kind"] in ("text", "folder_path"):
            escaped = text.replace("\\", "\\\\").replace('"', '\\"')
            return ("Text", f'"{escaped}"'), None
        return None, f"{param['name']}: a string argument for a {param['kind']} parameter"
    if not NUMBER_RE.match(arg):
        return None, f"{param['name']}: {arg!r} is computed at run time, not a literal"
    number = float(arg)
    if param["kind"] == "toggle":
        return ("Toggle", "true" if number != 0 else "false"), None
    if param["kind"] == "choice":
        # A numeric optionmenu argument is Praat's 1-based option index.
        index = int(number) - 1
        if not 0 <= index < len(param["options"]):
            return None, f"{param['name']}: option index {int(number)} is out of range"
        return ("Choice", index), None
    return ("Number", repr(number)), None


def build_chain(chain: Path, catalog: dict) -> tuple[list, list[str]]:
    steps, problems = [], []
    for shown, _target, args in stages(chain):
        entry = catalog.get(shown)
        if entry is None:
            problems.append(f"{shown}: not in the catalog")
            continue
        if entry["input"] != "wav":
            problems.append(f"{entry['key']}: input = {entry['input']!r}, not chainable")
            continue
        if len(args) != len(entry["params"]):
            problems.append(
                f"{entry['key']}: chain passes {len(args)}, catalog declares "
                f"{len(entry['params'])}"
            )
            continue
        values, refusal = [], None
        for param, arg in zip(entry["params"], args):
            value, why = param_value(param, arg)
            if value is None:
                refusal = why
                break
            values.append(value)
        if refusal:
            problems.append(f"{entry['key']}: {refusal}")
            continue
        steps.append((entry["key"], values))
    return steps, problems


def write_chains(out_dir: Path) -> None:
    catalog = load_catalog()
    out_dir.mkdir(parents=True, exist_ok=True)
    made, skipped = [], []
    for chain in sorted(CHAINS.glob("*.praat")):
        steps, problems = build_chain(chain, catalog)
        if problems or not steps:
            skipped.append((chain.name, problems or ["no runScript: stages"]))
            continue
        name = f"AudioTools {chain.stem}"
        lines = [f'name = "{name}"', ""]
        for key, values in steps:
            lines += ["[[steps]]", f'process_key = "{key}"', "side_chain = []", ""]
            for kind, value in values:
                lines += ["[[steps.values]]", f"{kind} = {value}", ""]
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
    args = parser.parse_args()
    if not CHAINS.is_dir():
        print(f"error: {CHAINS} is missing -- run: git submodule update --init", file=sys.stderr)
        return 1
    write_report()
    if args.chains:
        write_chains(Path(args.chains).expanduser())
    return 0


if __name__ == "__main__":
    sys.exit(main())
