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
PINNED_COMMIT="0de18dbd17187dc711a09eb5465c6d72c05c1fdb"
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
    -h|--help)    sed -n '2,31p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
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
info "about 439 of tui-wave's processes are scripts from this project"
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
    run git -C "$SCRIPTS" checkout --quiet --detach "$PINNED_COMMIT" \
      || die "commit $PINNED_COMMIT not found — is this tui-wave newer than the script?"
    ok "moved to ${PINNED_COMMIT:0:7}"
  fi
else
  [ -e "$SCRIPTS" ] && die "$SCRIPTS exists but is not a git checkout — move it aside and re-run"
  run mkdir -p "$(dirname "$SCRIPTS")"
  run git clone --quiet "$UPSTREAM" "$SCRIPTS" || die "could not clone $UPSTREAM"
  run git -C "$SCRIPTS" checkout --quiet --detach "$PINNED_COMMIT" \
    || die "commit $PINNED_COMMIT not found — is this tui-wave newer than the script?"
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
step "Python backend (optional — the 45 processes in the 'py' group)"
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
    run mkdir -p "$(dirname "$VENV")"
    run python3 -m venv "$VENV"
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
  # A warning rather than a failure: it costs three processes out of 453 and nothing else.
  if [ "$DRY_RUN" = 0 ] && ! "$VENV/bin/python3" -c 'import tkinter' 2>/dev/null; then
    warn "this Python has no ${BLUE}tkinter${RESET} — ${GREEN}Arranger${RESET}, ${GREEN}Performance Launcher${RESET}"
    warn "and ${GREEN}Spatial Panner${RESET} will fail with \"No module named 'tkinter'\""
    info "every other process is unaffected"
    pyver=$("$VENV/bin/python3" -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")' 2>/dev/null)
    case "$(uname -s)" in
      Darwin)
        info "macOS: Homebrew ships Python without it. Install it with"
        info "    brew install python-tk@${pyver}"
        ;;
      *)
        info "Debian/Ubuntu:  sudo apt install python3-tk"
        info "Fedora:         sudo dnf install python3-tkinter"
        info "Arch:           sudo pacman -S tk"
        ;;
    esac
    info "pip cannot install it; it is part of the base Python this venv was built from."
    info "Installing it takes effect immediately — no need to recreate the venv or re-run this."
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
    if confirm "Install the machine-learning libraries? (large download)"; then
      for pkg in $missing; do
        info "installing ${BLUE}$pkg${RESET}"
        run "$PIP" install --quiet --disable-pip-version-check "$pkg" \
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
