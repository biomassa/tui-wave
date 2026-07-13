#!/usr/bin/env python3
"""Converts SoundThread's process_help.json (MIT license, see THIRD_PARTY_NOTICES.md) into
this project's CDP process catalog format (src/model/cdp/catalog.toml).

SoundThread (https://github.com/j-p-higgins/SoundThread) ships a hand-curated set of ~120
CDP process definitions with parameter ranges/defaults/descriptions. cdparams/cdparse, CDP's
own metadata helpers, turned out to be a Sound Loom-internal dead end with no documented
protocol -- this conversion is why we don't have to hand-author that metadata ourselves.

Usage:
    python3 scripts/convert_soundthread_catalog.py \\
        --input /path/to/SoundThread/scenes/main/process_help.json \\
        --output src/model/cdp/catalog.toml

Run manually whenever re-syncing with an updated SoundThread catalog; the output is
committed, not generated at build time.
"""

import argparse
import json
import sys
from pathlib import Path

# Pure-UI SoundThread nodes with no corresponding CDP binary -- calculators, file pickers,
# unit converters. Not real processes.
UI_NODE_KEYS = {
    "inputfile",
    "outputfile",
    "calculator",
    "note_to_hz",
    "convert_time",
    "notes",
    "preview",
}

# pvoc_anal_1/pvoc_synth are the analysis/resynthesis steps themselves. In this editor
# they're never chosen directly from the process browser -- pipeline.rs wraps any process
# whose IoKind is Ana in an automatic anal/synth pair, so the user just picks a spectral
# process on ordinary audio. Keeping them out of the catalog avoids a redundant/confusing
# pair of "Analyse"/"Resynthesise" browser entries that don't behave like other processes.
PVOC_WRAP_KEYS = {"pvoc_anal_1", "pvoc_synth"}

EXCLUDED_KEYS = UI_NODE_KEYS | PVOC_WRAP_KEYS

INPUT_OUTPUT_KIND = {
    "[0]": "wav",
    "[1]": "ana",
    "[0, 0]": "dual_wav",
    "[1, 1]": "dual_ana",
    "[]": "none",
    "": "none",
}

CATEGORY = {"time": "time", "pvoc": "pvoc"}

# Reconciles SoundThread's own `subcategory` values with the (larger, independently
# hand-authored) set introduced in catalog_extra.toml, so the CDP browser's process-grouping
# feature (Ctrl+P — see CDP-Ext-Plan.md Phase 7) shows one clean taxonomy instead of two
# near-duplicate ones (e.g. this file's "granulate" next to catalog_extra.toml's "texture"
# for the same kind of process). Two layers:
#   SUBCATEGORY_REMAP   — a blanket string rename, for cases where SoundThread's own value
#                          unambiguously means the same thing everywhere it's used.
#   SUBCATEGORY_OVERRIDE — a few individual process keys needing per-process judgment,
#                          applied after the blanket remap. Every one of these was
#                          SoundThread's own catch-all "misc" (verified against
#                          process_help.json directly — SoundThread never actually
#                          distinguishes *why* something is misc, so there's no way to do
#                          this generically; each of the 14 "misc" entries was read and
#                          reclassified by hand).
SUBCATEGORY_REMAP = {
    "granulate": "texture",
    "extend": "texture",
    "reverb": "delay",
}
SUBCATEGORY_OVERRIDE = {
    "envel_replace_1": "envelope",
    "housekeep_extract_4": "utility",
    "modify_loudness_1": "amplitude",
    "modify_radical_1": "distort",
    "modify_radical_5": "distort",
    "modify_radical_6": "distort",
    "modify_speed_2": "time",
    "modify_speed_5": "time",
    "modify_stack": "combine",
    "phase_phase_1": "utility",
    "sfedit_cut_1": "segment",
    "sfedit_excise_1": "segment",
    "sfedit_join": "segment",
    "silend_silend_1": "segment",
}


def resolve_subcategory(key, raw_subcategory):
    if key in SUBCATEGORY_OVERRIDE:
        return SUBCATEGORY_OVERRIDE[key]
    return SUBCATEGORY_REMAP.get(raw_subcategory, raw_subcategory)


# Processes whose binary can't correctly parse the WAVE_FORMAT_EXTENSIBLE WAV header this
# app's runner normally writes for its 32-bit-float working format -- `write_inputs`
# (src/cdp/runner.rs) writes plain 16-bit integer PCM instead for these, trading a small
# amount of precision for correctness. See ProcessDef.requires_simple_wav_input's doc
# comment in src/model/cdp/def.rs for the full story: found via `rmverb` silently
# misreading float sample bytes as integers and producing wildly distorted (not erroring)
# output -- a user manually testing the process caught it, since neither the smoke test
# (checks exit code + non-empty file, not audio *content*) nor a routine listen would
# obviously reveal a "successful" job with garbage samples.
REQUIRES_SIMPLE_WAV_INPUT = {"rmverb"}

# Per-(process key, param name) overrides for cases where SoundThread's own static
# min/max/default is simply wrong -- the real constraint depends on runtime state this
# catalog format can't express as a fixed range, and needed a new NumberScale variant
# rather than a corrected literal. See NumberScale::HzCappedToAnalysisRange's doc comment
# (src/model/cdp/def.rs) for the strange_glis_2 finding: a user manually testing the
# process at its unchanged default hit "Value (50.0) out of range (93.75 to 24000.0)" --
# SoundThread's catalog declares a fixed 50-200 Hz range, but the real range is
# [sample_rate/analysis_points, sample_rate/4], confirmed against the binary's own usage
# text ("Range: FROM channel-frq-width TO nyquist/2").
PARAM_OVERRIDE = {
    ("strange_glis_2", "Spacing"): {
        "scale": "hz_capped_to_analysis_range",
        "min": 1.0,
        "max": 24000.0,
        "default": 200.0,
    },
}


def split_key(key, known_bins):
    """Splits a SoundThread key like "modify_speed_2" into (bin, subprog, mode).

    The first underscore-delimited token is always the CDP binary name (verified against
    every binary in the CDP install this converter was written against); a trailing purely
    numeric token is the mode number; anything remaining in between is the subprog.
    "rmverb" (no underscore) has no subprog/mode -- it's invoked as `rmverb infile outfile
    params...` directly.
    """
    parts = key.split("_")
    bin_name = parts[0]
    if bin_name not in known_bins:
        raise ValueError(f"key {key!r}: {bin_name!r} is not a known CDP binary")
    rest = parts[1:]
    mode = None
    if rest and rest[-1].isdigit():
        mode = rest[-1]
        rest = rest[:-1]
    subprog = "_".join(rest) if rest else None
    return bin_name, subprog, mode


def param_scale(param):
    if param.get("outputduration"):
        return "output_duration_seconds"
    if param.get("fftwindowsize"):
        return "percent_of_fft_size"
    if param.get("fftwindowcount"):
        return "percent_of_ana_window_count"
    if param.get("time"):
        return "percent_of_input_duration"
    return "plain"


def convert_param(param):
    uitype = param.get("uitype")
    flag = param.get("flag") or None
    common = {
        "name": param["paramname"],
        "description": param.get("paramdescription", ""),
        "flag": flag,
        "automatable": bool(param.get("automatable", False)),
    }

    if uitype == "hslider":
        common["kind"] = "number"
        common["min"] = float(param["minrange"])
        common["max"] = float(param["maxrange"])
        common["step"] = float(param["step"])
        common["default"] = float(param["value"])
        common["exponential"] = bool(param.get("exponential", False))
        common["scale"] = param_scale(param)
        return common

    if uitype == "checkbutton":
        common["kind"] = "toggle"
        common["default"] = bool(param.get("value", False))
        return common

    if uitype == "optionbutton":
        # SoundThread stashes the option list as a stringified array in `step`, e.g.
        # "[44100, 48000, 88200, 96000]"; `value` is the 0-based default index.
        raw_options = param.get("step", "[]")
        options = json.loads(raw_options.replace("'", '"'))
        common["kind"] = "choice"
        common["options"] = [str(o) for o in options]
        common["default"] = int(param.get("value", 0))
        return common

    if uitype == "addremoveinlets":
        # SoundThread's variable-arity multi-input widget (join/interleave/max-style
        # processes). These processes are already DualWav/DualAna in our model and excluded
        # from v1 (see pipeline::PlanError::UnsupportedInV1) -- there's no v1 UI concept for
        # this parameter, so it's dropped rather than converted. A future dual/multi-input
        # phase will need its own representation for it.
        return None

    raise ValueError(f"unhandled uitype {uitype!r} for param {param.get('paramname')!r}")


def convert_process(key, entry, known_bins):
    bin_name, subprog, mode = split_key(key, known_bins)
    params_in_order = [
        entry["parameters"][pk]
        for pk in sorted(
            entry.get("parameters", {}).keys(),
            key=lambda name: int(name.replace("param", "")),
        )
    ]
    params = [c for p in params_in_order if (c := convert_param(p)) is not None]
    for p in params:
        override = PARAM_OVERRIDE.get((key, p["name"]))
        if override:
            p.update(override)
    return {
        "key": key,
        "bin": bin_name,
        "subprog": subprog,
        "mode": mode,
        "title": entry["title"],
        "category": CATEGORY[entry["category"]],
        "subcategory": resolve_subcategory(key, entry.get("subcategory", "")),
        "short_description": entry.get("short_description", ""),
        "description": entry.get("description", ""),
        "input": INPUT_OUTPUT_KIND[entry.get("inputtype", "")],
        "output": INPUT_OUTPUT_KIND[entry.get("outputtype", "")],
        "stereo_native": bool(entry.get("stereo", False)),
        "output_is_stereo": bool(entry.get("outputisstereo", False)),
        "requires_simple_wav_input": key in REQUIRES_SIMPLE_WAV_INPUT,
        "params": params,
    }


def toml_escape(s):
    return s.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n")


def toml_string(s):
    return f'"{toml_escape(s)}"'


def toml_value(v):
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, (int, float)):
        return repr(float(v)) if isinstance(v, float) else str(v)
    if isinstance(v, str):
        return toml_string(v)
    if v is None:
        return None
    if isinstance(v, list):
        return "[" + ", ".join(toml_value(x) for x in v) + "]"
    raise TypeError(f"unhandled TOML value type: {type(v)}")


def write_param_table(lines, param):
    lines.append("[[process.params]]")
    for field in ("name", "description"):
        lines.append(f"{field} = {toml_value(param[field])}")
    if param["flag"] is not None:
        lines.append(f"flag = {toml_value(param['flag'])}")
    lines.append(f"automatable = {toml_value(param['automatable'])}")
    lines.append(f"kind = {toml_value(param['kind'])}")
    if param["kind"] == "number":
        for field in ("min", "max", "step", "default", "exponential", "scale"):
            lines.append(f"{field} = {toml_value(param[field])}")
    elif param["kind"] == "toggle":
        lines.append(f"default = {toml_value(param['default'])}")
    elif param["kind"] == "choice":
        lines.append(f"options = {toml_value(param['options'])}")
        lines.append(f"default = {toml_value(param['default'])}")
    lines.append("")


def write_process_table(lines, proc):
    lines.append("[[process]]")
    for field in (
        "key",
        "bin",
        "subprog",
        "mode",
        "title",
        "category",
        "subcategory",
        "short_description",
        "description",
        "input",
        "output",
        "stereo_native",
        "output_is_stereo",
    ):
        value = proc[field]
        if value is None:
            continue
        lines.append(f"{field} = {toml_value(value)}")
    # Rust's #[serde(default)] means omitting this (false) is equivalent to writing it --
    # only emit it for the handful of processes that actually need it, so the generated
    # TOML doesn't grow a `requires_simple_wav_input = false` line on every one of the other
    # ~120 entries.
    if proc["requires_simple_wav_input"]:
        lines.append(f"requires_simple_wav_input = {toml_value(True)}")
    lines.append("")
    for param in proc["params"]:
        write_param_table(lines, param)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", required=True, type=Path, help="path to process_help.json")
    parser.add_argument("--output", required=True, type=Path, help="path to write catalog.toml")
    parser.add_argument(
        "--cdp-bin-dir",
        type=Path,
        default=None,
        help="directory of CDP binaries, used to validate each key's binary name exists "
        "(defaults to skipping validation)",
    )
    args = parser.parse_args()

    data = json.loads(args.input.read_text())

    known_bins = None
    if args.cdp_bin_dir:
        known_bins = {p.name for p in args.cdp_bin_dir.iterdir()}

    processes = []
    errors = []
    for key, entry in sorted(data.items()):
        if key in EXCLUDED_KEYS:
            continue
        try:
            bins_for_split = known_bins if known_bins is not None else {key.split("_")[0]}
            processes.append(convert_process(key, entry, bins_for_split))
        except Exception as exc:  # noqa: BLE001 - report and continue, don't abort the batch
            errors.append(f"{key}: {exc}")

    if errors:
        print(f"{len(errors)} entries failed to convert:", file=sys.stderr)
        for e in errors:
            print(f"  {e}", file=sys.stderr)
        sys.exit(1)

    lines = [
        "# Generated by scripts/convert_soundthread_catalog.py from SoundThread's",
        "# process_help.json (MIT license, (c) Jonathan Higgins) -- see THIRD_PARTY_NOTICES.md.",
        "# Do not hand-edit; re-run the converter instead. To add or override a process",
        "# without touching this file, add a *.toml with the same [[process]] schema to",
        "# $XDG_CONFIG_HOME/tui-wave/cdp/.",
        "",
    ]
    for proc in processes:
        write_process_table(lines, proc)

    args.output.write_text("\n".join(lines))
    print(f"wrote {len(processes)} process definitions to {args.output}")


if __name__ == "__main__":
    main()
