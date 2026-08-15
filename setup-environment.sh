#!/usr/bin/env bash
# Set up everything a *downloaded* tui-wave needs to run Praat processes.
#
#     ./setup-environment.sh              # set everything up
#     ./setup-environment.sh --dry-run    # print every command, change nothing
#     ./setup-environment.sh --yes        # take every prompt as yes
#     ./setup-environment.sh --no-python  # skip the Python venv (the 'py' group stays off)
#
# ## Why this exists
#
# The release packages contain the tui-wave binary and nothing else. The ~439 Praat processes
# in its catalog are *scripts*, and those live in a separate project (praatAudioTools) which no
# package bundles — so a freshly downloaded tui-wave lists every process and can run none of
# them, and instead asks where the scripts are. This fetches them and tells tui-wave where they
# went.
#
# Building from source does not need this: `install.sh` does the same work, and a source
# checkout already has the scripts as a git submodule.
#
# ## What it does
#
#   1. Clones praatAudioTools at the exact commit this build's catalog was generated from
#      (see PINNED_COMMIT) into ~/.config/tui-wave/praat/audiotools
#   2. Writes praat_audiotools_dir into ~/.config/tui-wave/config.toml
#   3. Creates a Python virtualenv with numpy/scipy/soundfile for the 34 'py' processes
#   4. Checks that `praat` itself is installed, and says where to get it if not
#
# It does **not** install CDP: that is a separate manual download with no package anywhere, and
# tui-wave asks for its directory the first time you run a CDP process.
#
# ## The commit pin is not optional
#
# tui-wave's process catalog is generated from a specific praatAudioTools commit and compiled
# into the binary — parameter names, types, order and count. Upstream rewrites scripts
# constantly and without warning ("Add files via upload" is the entire commit history). Checking
# out anything other than PINNED_COMMIT would hand each script arguments in an order it no
# longer expects, which Praat does not reject: it fills the form positionally and produces
# plausible, wrong audio. `praat_setup_commit_matches_the_catalog` in the test suite keeps this
# value in step with the catalog.

set -euo pipefail

# The praatAudioTools commit this build's catalog was generated from. Kept in step with
# src/model/cdp/praat_catalog.toml's header by a test — see the note above.
PINNED_COMMIT="e2cbd5fc1573a4f5837c7360cddacc2ceff5668f"
UPSTREAM="https://github.com/ShaiCohen-ops/Praat-plugin_AudioTools"

CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
STATE="$CONFIG_HOME/tui-wave/praat"
SCRIPTS="$STATE/audiotools"
VENV="$STATE/pyenv"
CONFIG="$CONFIG_HOME/tui-wave/config.toml"

ASSUME_YES=0
DRY_RUN=0
WANT_PYTHON=1

for arg in "$@"; do
  case "$arg" in
    -y|--yes)     ASSUME_YES=1 ;;
    --dry-run)    DRY_RUN=1 ;;
    --no-python)  WANT_PYTHON=0 ;;
    # Stops at line 29: what follows is the commit-pin rationale, which is a note to whoever
    # maintains this file rather than to whoever runs it. Ending at 31 printed its heading with
    # nothing under it.
    -h|--help)    sed -n '2,29p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *)            echo "unknown option: $arg (try --help)" >&2; exit 2 ;;
  esac
done

if [ -t 1 ]; then
  BOLD=$(printf '\033[1m'); DIM=$(printf '\033[2m'); RED=$(printf '\033[31m')
  GREEN=$(printf '\033[32m'); YELLOW=$(printf '\033[33m'); RESET=$(printf '\033[0m')
  # Python package names, as GREEN is process names — the two share a sentence throughout the
  # Python section, and which half is the thing you install is what the line is telling you.
  BLUE=$(printf '\033[94m')
else
  BOLD=""; DIM=""; RED=""; GREEN=""; YELLOW=""; BLUE=""; RESET=""
fi

step() { printf '\n%s==>%s %s%s%s\n' "$BOLD" "$RESET" "$BOLD" "$*" "$RESET"; }
info() { printf '    %s\n' "$*"; }
ok()   { printf '    %s✓%s %s\n' "$GREEN" "$RESET" "$*"; }
warn() { printf '    %s!%s %s\n' "$YELLOW" "$RESET" "$*"; }
die()  { printf '\n%serror:%s %s\n' "$RED" "$RESET" "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

# Interpreters in preference order. Absolute paths trail the bare names so PATH still decides
# first; they are there because pyenv's shims precede Homebrew's on a normal macOS PATH, and
# pyenv's interpreters are the ones most likely to lack Tk.
PYTHON_CANDIDATES="python3.13 python3.12 python3.11 python3.10 python3
/opt/homebrew/bin/python3.13 /opt/homebrew/bin/python3.12 /opt/homebrew/bin/python3
/usr/local/bin/python3 /usr/bin/python3"

# The first interpreter that can build a venv *and* import tkinter, else the first that can build
# a venv at all. A venv cannot acquire Tk after the fact — `_tkinter` is a compiled module of the
# base interpreter, which `pip` cannot supply — so this is the only moment the choice can be
# made. On a Mac with pyenv first on PATH, the plain `python3` is typically Tk-less and the venv
# built from it fails at the moment a window would have opened (user report, 2026-08-08).
pick_python() {
  fallback=""
  for candidate in $PYTHON_CANDIDATES; do
    # Must actually *run*, not merely exist: pyenv leaves a shim for every version it knows
    # about, so `command -v python3.12` succeeds where running it exits non-zero.
    "$candidate" -c 'import venv' >/dev/null 2>&1 || continue
    if "$candidate" -c 'import tkinter' >/dev/null 2>&1; then echo "$candidate"; return 0; fi
    [ -n "$fallback" ] || fallback="$candidate"
  done
  echo "${fallback:-python3}"
}

find_tkinter_python() {
  for candidate in $PYTHON_CANDIDATES; do
    if "$candidate" -c 'import venv, tkinter' >/dev/null 2>&1; then
      command -v "$candidate"
      return 0
    fi
  done
  return 1
}

# Which packaging a venv's *base* interpreter came from — that alone decides how Tk is added to
# it, and getting it wrong is how the previous advice sent a pyenv user to `brew install
# python-tk`, which targets a different interpreter entirely.
python_flavour() {
  case "$1" in
    */.pyenv/*|*/pyenv/versions/*) echo pyenv ;;
    */Cellar/*|/opt/homebrew/*|/usr/local/opt/*) echo homebrew ;;
    /Library/Frameworks/Python.framework/*) echo python-org ;;
    /System/*|/usr|/usr/bin/*|*/CommandLineTools/*|/Applications/Xcode.app/*) echo system ;;
    *) echo other ;;
  esac
}

tkinter_remedy() {
  base="$1" flavour="$2" pyver="$3"
  case "$flavour" in
    pyenv)
      info "the venv is built on pyenv's Python ($base), compiled without Tcl/Tk."
      info "${BOLD}brew install python-tk will not fix this${RESET} — that targets Homebrew's Python."
      info "pyenv links Tk at build time, so the interpreter has to be rebuilt:"
      info "    brew install tcl-tk"
      info "    pyenv install --force ${pyver:-3.13}"
      info "then delete $VENV and re-run this script."
      ;;
    homebrew)
      info "macOS: Homebrew ships Python without it. Install it with"
      info "    brew install python-tk@${pyver}"
      ;;
    python-org)
      info "this is a python.org build, which normally bundles Tk — reinstall it from"
      info "https://www.python.org/downloads/ and pick the Tcl/Tk option."
      ;;
    *)
      if [ "$(uname -s)" = Darwin ]; then
        info "install a Python built with Tcl/Tk (Homebrew's python@${pyver} plus"
        info "python-tk@${pyver} is the usual route), then delete $VENV and re-run this."
      else
        info "Debian/Ubuntu:  sudo apt install python3-tk"
        info "Fedora:         sudo dnf install python3-tkinter"
        info "Arch:           sudo pacman -S tk"
      fi
      ;;
  esac
}

# `confirm` for a question `--yes` must not answer: rebuilding the venv re-downloads everything
# in it, and the machine-learning tier alone is 2.5 GB. A wrong yes costs that download, a wrong
# no costs nothing, so an unattended run has to take the cheap side.
confirm_explicitly() {
  [ -t 0 ] || return 1
  printf '    %s?%s %s [y/N] ' "$YELLOW" "$RESET" "$1"
  read -r reply
  case "$reply" in [yY]*) return 0 ;; *) return 1 ;; esac
}

# Which of a tier's packages are absent from the venv, as a space-separated list of pip names.
#
# Takes `pip-name:module-name` pairs because the two disagree more often than not
# (scikit-learn/sklearn, nara-wpe/nara_wpe, descript-audio-codec/dac). Importing is the honest
# test rather than `pip show`: a package can be recorded as installed and still fail to import,
# which for a compiled wheel on the wrong CPU is exactly the case worth catching.
#
# Nothing was ever re-*downloaded* without this — no tier package is installed with `--upgrade`,
# so pip short-circuits on "Requirement already satisfied" in about half a second. What this
# avoids is being asked again about a 2.5 GB tier you already have, and then watching the script
# print "installing torch" while pip decides there is nothing to do. On a re-run that reads
# exactly like the download starting over.
#
# No venv yet (a first run, or --dry-run) means everything counts as missing, which is both true
# and the right thing to show.
missing_from_tier() {
  local missing="" spec pkg mod have=1
  [ -x "$VENV/bin/python3" ] || have=0
  for spec in "$@"; do
    pkg=${spec%%:*}; mod=${spec##*:}
    if [ "$have" = 0 ] || ! "$VENV/bin/python3" -c "import $mod" 2>/dev/null; then
      missing="$missing $pkg"
    fi
  done
  printf '%s' "${missing# }"
}

run() {
  if [ "$DRY_RUN" = 1 ]; then
    printf '    %s$ %s%s\n' "$DIM" "$*" "$RESET"
  else
    "$@"
  fi
}

# Move the checkout to `PINNED_COMMIT`, forcing past the phantom local edits a
# case-insensitive filesystem produces.
#
# praatAudioTools ships four pairs of scripts whose names differ only in case, in the same
# folder — `Reverb/Stereo_Shimmer.praat` and `Reverb/stereo_shimmer.praat` among them. On APFS
# (the macOS default) and on any case-insensitive volume, both tracked paths resolve to one
# file, so git reports whichever it did not write as **locally modified** and refuses to check
# anything out: "Your local changes to the following files would be overwritten by checkout".
# That is not a user edit and there is nothing to preserve — it is the same filesystem limit
# the README's *Known issues* describes, biting the update path rather than a run.
#
# So: try the ordinary checkout, and fall back to `--force`, which is safe here because this
# checkout belongs to tui-wave. It is fetched at a pinned commit, the app runs each script from
# a temporary copy and never writes to it, and anyone who wants a checkout of their own to edit
# points `praat_audiotools_dir` at it instead.
checkout_pinned_commit() {
  if [ "$DRY_RUN" = 1 ]; then
    printf '    %s$ git -C %s checkout --detach %s%s\n' "$DIM" "$SCRIPTS" "$PINNED_COMMIT" "$RESET"
    return 0
  fi
  if git -C "$SCRIPTS" checkout --quiet --detach "$PINNED_COMMIT" 2>/dev/null; then
    return 0
  fi
  warn "the checkout refused to move — forcing past local changes"
  warn "(expected on macOS: four scripts differ only in case and cannot coexist here)"
  git -C "$SCRIPTS" checkout --quiet --detach --force "$PINNED_COMMIT" \
    || die "could not move $SCRIPTS to $PINNED_COMMIT — delete it and re-run to re-clone"
}

confirm() {
  [ "$ASSUME_YES" = 1 ] && return 0
  [ "$DRY_RUN" = 1 ] && return 0
  [ -t 0 ] || { warn "not interactive and --yes was not given; skipping"; return 1; }
  printf '    %s?%s %s [y/N] ' "$YELLOW" "$RESET" "$1"
  read -r reply
  case "$reply" in [yY]*) return 0 ;; *) return 1 ;; esac
}

# --- 1. Praat itself ------------------------------------------------------------------------
#
# Checked first because everything below is pointless without it, and because it is the one
# dependency with a real package on every platform.
step "Praat"
if have praat; then
  ok "praat found: $(command -v praat)"
else
  warn "praat is not installed — the scripts fetched below will have nothing to run them"
  info "  macOS:          brew install praat"
  info "  Debian/Ubuntu:  sudo apt install praat"
  info "  Fedora:         sudo dnf install praat"
  info "  Arch:           sudo pacman -S praat"
  info "  or download it from https://www.fon.hum.uva.nl/praat/"
  info "tui-wave works without Praat; only the Praat process group is affected."
fi

# --- 2. The praatAudioTools scripts ----------------------------------------------------------
step "praatAudioTools scripts"
have git || die "git is required to fetch the scripts — install it and re-run"
info "about 457 of tui-wave's processes are scripts from this project"
# Whose work this is, at the moment it is being downloaded. The scripts are run as-is by
# absolute path and never modified, so the credit belongs where the fetch happens rather than
# buried in a notices file nobody opens.
info "by Shai Cohen (Department of Music, Bar-Ilan University, Israel), MIT-licensed"
info "$UPSTREAM"
info "target: $SCRIPTS"
info "commit: $PINNED_COMMIT"

if [ -d "$SCRIPTS/.git" ]; then
  current="$(git -C "$SCRIPTS" rev-parse HEAD 2>/dev/null || echo unknown)"
  if [ "$current" = "$PINNED_COMMIT" ]; then
    ok "already present at the right commit"
  else
    info "present at ${current:0:7}, but this build's catalog needs ${PINNED_COMMIT:0:7}"
    # `fetch` rather than a fresh clone: the checkout may be large and is usually only a few
    # commits behind. A detached HEAD is correct here — this is a pinned dependency, not a
    # branch anybody works on.
    run git -C "$SCRIPTS" fetch --quiet origin || die "could not fetch from $UPSTREAM"
    # Ask whether the commit actually arrived *before* trying to check it out, so the two real
    # failures stop sharing one message. A missing commit means this tui-wave is newer than the
    # upstream the checkout points at; a refused checkout means something in the working tree
    # is in the way, which is a different problem with a different fix.
    #
    # Skipped under --dry-run, where the fetch above was printed rather than run: the commit
    # would be missing precisely because nothing was fetched, and a dry run must not fail on
    # the consequences of its own inaction.
    if [ "$DRY_RUN" != 1 ] \
      && ! git -C "$SCRIPTS" cat-file -e "${PINNED_COMMIT}^{commit}" 2>/dev/null; then
      die "commit $PINNED_COMMIT not found upstream — is this tui-wave newer than the script?"
    fi
    checkout_pinned_commit
    ok "moved to ${PINNED_COMMIT:0:7}"
  fi
else
  [ -e "$SCRIPTS" ] && die "$SCRIPTS exists but is not a git checkout — move it aside and re-run"
  run mkdir -p "$(dirname "$SCRIPTS")"
  run git clone --quiet "$UPSTREAM" "$SCRIPTS" || die "could not clone $UPSTREAM"
  checkout_pinned_commit
  ok "cloned at ${PINNED_COMMIT:0:7}"
fi

# --- 3. Point tui-wave at them ---------------------------------------------------------------
#
# The binary looks for the scripts relative to itself, which works for a source build sitting in
# `target/release/` and cannot work for `/usr/bin/tui-wave`. Writing the path into the config is
# what makes an installed package find them.
step "Configuration"
info "config: $CONFIG"
if [ "$DRY_RUN" = 1 ]; then
  printf '    %s$ set praat_audiotools_dir = "%s" in %s%s\n' "$DIM" "$SCRIPTS" "$CONFIG" "$RESET"
elif grep -q '^praat_audiotools_dir[[:space:]]*=' "$CONFIG" 2>/dev/null; then
  # Rewritten in place rather than appended: a second key would be parsed as a duplicate and
  # the file would stop loading, which `Config::load` answers by silently falling back to
  # defaults — losing every other setting the user has.
  tmp="$(mktemp)"
  sed "s|^praat_audiotools_dir[[:space:]]*=.*|praat_audiotools_dir = \"$SCRIPTS\"|" "$CONFIG" > "$tmp"
  mv "$tmp" "$CONFIG"
  ok "praat_audiotools_dir updated"
else
  mkdir -p "$(dirname "$CONFIG")"
  printf 'praat_audiotools_dir = "%s"\n' "$SCRIPTS" >> "$CONFIG"
  ok "praat_audiotools_dir written"
fi

# --- 4. Python venv for the `py` process group -----------------------------------------------
#
# Kept entirely inside a venv the app owns; the system Python is never touched, which matters on
# Arch and Debian where it is externally managed and rejects `pip install` outright (PEP 668).
# The `py` scripts resolve their own interpreter, so tui-wave runs a copy with those assignments
# repointed at this venv — a PATH-only mechanism worked on Linux and silently did nothing on
# macOS, where they pick an absolute path before consulting PATH.
step "Python backend (optional — the 46 processes in the 'py' group)"
info "these scripts drive a Python helper and need ${BLUE}numpy, scipy${RESET} and ${BLUE}soundfile${RESET}"
info "(plus ${BLUE}sounddevice${RESET} and ${BLUE}pillow${RESET} for three interactive editors)"
info "everything else in tui-wave works without them"

if [ "$WANT_PYTHON" = 0 ]; then
  info "skipped (--no-python)"
elif ! have python3; then
  warn "python3 not found; skipping. Install Python 3 and re-run to enable the 'py' group."
elif ! confirm "Install the Python dependencies?"; then
  info "skipped; re-run this script later to add them"
else
  info "venv: $VENV"
  if [ -x "$VENV/bin/python3" ]; then
    ok "venv already exists"
  else
    if ! python3 -c 'import venv' 2>/dev/null; then
      warn "python3's venv module is unavailable"
      info "Debian/Ubuntu split it out: sudo apt install python3-venv"
      die "install it and re-run"
    fi
    PYBIN=$(pick_python)
    [ "$PYBIN" = python3 ] || info "interpreter: $PYBIN (it has tkinter; plain python3 does not)"
    run mkdir -p "$(dirname "$VENV")"
    run "$PYBIN" -m venv "$VENV"
    ok "venv created"
  fi

  PIP="$VENV/bin/pip"
  run "$PIP" install --quiet --disable-pip-version-check --upgrade pip \
    || warn "could not upgrade pip; continuing with the version the venv shipped"
  for pkg in numpy scipy soundfile; do
    info "installing ${BLUE}$pkg${RESET} (this can take a few minutes if no wheel matches your Python)"
    run "$PIP" install --quiet --disable-pip-version-check "$pkg" \
      || die "${BLUE}$pkg${RESET} failed to install — the 'py' group needs all three"
  done
  if [ "$DRY_RUN" = 0 ]; then
    "$VENV/bin/python3" -c 'import numpy, scipy, soundfile' \
      && ok "${BLUE}numpy, scipy, soundfile${RESET} import cleanly" \
      || die "the venv was created but the packages did not import"
  fi

  # tkinter is standard library, which is exactly why nothing checks for it and exactly why it
  # goes missing: it is a *compiled* module (`_tkinter`, linked against Tcl/Tk) that several
  # distributions and Homebrew split into a separate package. `pip install` cannot supply it —
  # it belongs to the base interpreter, not to this venv.
  #
  # It bites on macOS specifically. Homebrew's `python@3.x` ships without it, and a venv built
  # from that base inherits the gap, so Arranger opens on Linux and fails on a Mac with
  # `ModuleNotFoundError: No module named 'tkinter'` and no hint as to why (user report,
  # 2026-08-08). The three processes that need it import it *lazily*, so nothing surfaces until
  # the moment the window would have opened.
  #
  # A warning rather than a failure: it costs three processes out of 456 and nothing else.
  if [ "$DRY_RUN" = 0 ] && ! "$VENV/bin/python3" -c 'import tkinter' 2>/dev/null; then
    warn "this Python has no ${BLUE}tkinter${RESET} — ${GREEN}Arranger${RESET}, ${GREEN}Performance Launcher${RESET}"
    warn "and ${GREEN}Spatial Panner${RESET} will fail with \"No module named 'tkinter'\""
    info "every other process is unaffected"
    pyver=$("$VENV/bin/python3" -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")' 2>/dev/null)
    base=$("$VENV/bin/python3" -c 'import sys; print(sys.base_prefix)' 2>/dev/null)
    tkinter_remedy "$base" "$(python_flavour "$base")" "$pyver"
    info "pip cannot install it; it is part of the base Python this venv was built from."

    # A venv's base interpreter is fixed when it is created, so the only in-place repair is to
    # build it again on a different one. Offered when such an interpreter is actually present,
    # never taken automatically and never by `--yes`: the packages have to come down again, and
    # the machine-learning tier alone is 2.5 GB. What is installed now is listed before the
    # question rather than discovered after it.
    if tkpy=$(find_tkinter_python); then
      info ""
      info "found ${BLUE}$tkpy${RESET}, which does have tkinter"
      # `|| true` because pipefail would abort the script when grep filters everything out,
      # which for an otherwise-empty venv is a legitimate answer.
      installed=$("$VENV/bin/pip" list --format=freeze --disable-pip-version-check 2>/dev/null \
        | cut -d= -f1 | grep -Ev '^(pip|setuptools|wheel|pkg_resources)$' | tr '\n' ' ' || true)
      venvsize=$(du -sh "$VENV" 2>/dev/null | cut -f1)
      info "rebuilding on it re-downloads what is in the venv now (${venvsize:-unknown} on disk):"
      info "  ${BLUE}${installed:-nothing}${RESET}"
      if confirm_explicitly "Rebuild the venv on $tkpy?"; then
        # Reinstalled by *name*, not name==version: a version pinned for one interpreter may
        # have no wheel for another, and building numpy from source is the wait this avoids.
        rm -rf "$VENV"
        "$tkpy" -m venv "$VENV" || die "could not create the venv with $tkpy"
        PIP="$VENV/bin/pip"
        "$PIP" install --quiet --disable-pip-version-check --upgrade pip \
          || warn "could not upgrade pip; continuing"
        for pkg in $installed; do
          info "installing ${BLUE}$pkg${RESET}"
          "$PIP" install --quiet --disable-pip-version-check "$pkg" \
            || warn "${BLUE}$pkg${RESET} failed; the processes needing it will say so when run"
        done
        if "$VENV/bin/python3" -c 'import tkinter' 2>/dev/null; then
          ok "rebuilt — ${GREEN}Arranger, Performance Launcher, Spatial Panner${RESET} will open now"
        else
          warn "the rebuilt venv still has no tkinter; the remedy above is the remaining route"
        fi
      else
        info "kept as it is; the three Tk processes stay unavailable"
      fi
    fi
  elif [ "$DRY_RUN" = 0 ]; then
    ok "${BLUE}tkinter${RESET} present — ${GREEN}Arranger, Performance Launcher, Spatial Panner${RESET}"
  fi

  # --- Optional tiers -------------------------------------------------------------------
  #
  # Same bargain as the base packages: a process whose library is missing still appears in the
  # browser, and the helper's own dependency check names what is absent — declining costs
  # nothing except that process failing if you run it.
  #
  # Two prompts rather than one, because the sizes are not comparable: bundling them would make
  # "yes" mean a 2.5 GB download for someone who only wanted timbre analysis.
  #
  # torch comes from PyTorch's CPU index on Linux rather than PyPI. The default wheel hard-depends
  # on the entire CUDA runtime — cuDNN, cuBLAS, NCCL and the rest, measured at 2.7 GB of `nvidia/*`
  # in a venv on a laptop with no NVIDIA GPU, taking it from 2.3 GB to 6.0 GB. Nothing here can
  # use any of it: the two ML processes are a speech vocoder and a codec running at 16-24 kHz,
  # which is CPU work. Linux only, because that is where the split exists — macOS wheels on PyPI
  # are already CPU/MPS builds. `--index-url` rather than `--extra-index-url`, as PyTorch's own
  # instructions have it: it replaces PyPI for that command so the CUDA build is not reachable at
  # all, which is the point — left reachable, resolution can wander back to it.
  TORCH_INDEX=""
  [ "$(uname -s)" = Linux ] && TORCH_INDEX="--index-url https://download.pytorch.org/whl/cpu"
  info ""
  ANALYSIS_TIER="librosa:librosa scikit-learn:sklearn nara-wpe:nara_wpe mido:mido"
  missing=$(missing_from_tier $ANALYSIS_TIER)
  if [ -z "$missing" ]; then
    ok "analysis libraries already installed — nothing to download"
  else
    if [ "$missing" = "librosa scikit-learn nara-wpe mido" ]; then
      info "Optional: analysis libraries (~60 MB) — ${BLUE}$missing${RESET}"
    else
      info "Optional: analysis libraries — ${BLUE}$missing${RESET} (the rest are already installed)"
    fi
    info "  enables ${GREEN}AI Conductor Mix, Dereverberation, IdentitySeparation, Recomposer (x2),${RESET}"
    info "  ${GREEN}ThermodynamicTransform, AcousticDNAResonator${RESET}"
    if confirm "Install the analysis libraries?"; then
      for pkg in $missing; do
        info "installing ${BLUE}$pkg${RESET}"
        run "$PIP" install --quiet --disable-pip-version-check "$pkg" \
          || warn "${BLUE}$pkg${RESET} failed; the processes needing it will say so when run"
      done
    else
      info "skipped; those processes stay listed and name the missing library if run"
    fi
  fi

  info ""
  ML_TIER="torch:torch torchaudio:torchaudio encodec:encodec descript-audio-codec:dac"
  missing=$(missing_from_tier $ML_TIER)
  if [ -z "$missing" ]; then
    ok "machine-learning libraries already installed — nothing to download"
  else
    if [ "$missing" = "torch torchaudio encodec descript-audio-codec" ]; then
      info "Optional: machine-learning libraries (~2.5 GB) — ${BLUE}$missing${RESET}"
    else
      info "Optional: machine-learning libraries — ${BLUE}$missing${RESET} (the rest are already installed)"
    fi
    info "  enables ${GREEN}HierarchicalRecomposition${RESET} and ${GREEN}NeuralResynthesisVocoder${RESET}"
    info "  some ML processes additionally need model files you supply yourself"
    [ -n "$TORCH_INDEX" ] && info "  CPU builds of ${BLUE}torch${RESET}/${BLUE}torchaudio${RESET} — the CUDA ones add 2.7 GB nothing here uses"
    if confirm "Install the machine-learning libraries? (large download)"; then
      # torch and torchaudio first, from the CPU index, *before* the two packages that depend on
      # them: pip stops at "already satisfied", so a CPU torch installed first is what encodec and
      # descript-audio-codec then build on. The other order lets their resolution pull the CUDA
      # torch from PyPI, and the saving is gone.
      for pkg in $missing; do
        case "$pkg" in
          torch|torchaudio) index="$TORCH_INDEX" ;;
          *)                index="" ;;
        esac
        info "installing ${BLUE}$pkg${RESET}"
        run "$PIP" install --quiet --disable-pip-version-check $index "$pkg" \
          || warn "${BLUE}$pkg${RESET} failed; the processes needing it will say so when run"
      done
    else
      info "skipped; those processes stay listed and name the missing library if run"
    fi
  fi

  # Only the three interactive editors need these, and sounddevice wants PortAudio at run time,
  # which a headless machine legitimately lacks. A failure costs three processes, so it warns
  # rather than dying.
  extras_ok=1
  for pkg in sounddevice pillow; do
    run "$PIP" install --quiet --disable-pip-version-check "$pkg" || extras_ok=0
  done
  if [ "$DRY_RUN" = 0 ]; then
    if [ "$extras_ok" = 1 ] && "$VENV/bin/python3" -c 'import sounddevice, PIL' 2>/dev/null; then
      ok "${BLUE}sounddevice, pillow${RESET} ready — ${GREEN}Arranger, Performance Launcher, Spectral Eraser${RESET}"
    else
      warn "${BLUE}sounddevice/pillow${RESET} unavailable — ${GREEN}Arranger${RESET}, ${GREEN}Performance Launcher${RESET}"
      warn "and ${GREEN}Spectral Eraser${RESET} will report missing dependencies; everything else works"
    fi
  fi
fi

# --- 5. Done ----------------------------------------------------------------------------------
step "Done"
if [ "$DRY_RUN" = 1 ]; then
  info "dry run — nothing was changed"
  exit 0
fi
ok "the Praat process group is ready"
info ""
info "Start tui-wave and press Ctrl+P to browse processes."
info "CDP is separate and has no installer: download it from"
info "  https://www.composersdesktop.com/  — tui-wave will ask for the folder"
info "the first time you run a CDP process."
