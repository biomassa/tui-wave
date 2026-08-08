#!/usr/bin/env bash
#
# tui-wave installer — macOS and Linux.
#
# Builds tui-wave from this checkout, installs it, and sets up the optional backends. Run it
# from the repository root:
#
#     ./install.sh                 # interactive: asks before anything that needs sudo
#     ./install.sh --yes           # assume yes to every prompt (for CI or a scripted setup)
#     ./install.sh --no-python     # skip the Python venv the `py` process group needs
#     ./install.sh --no-praat      # skip installing Praat itself
#     ./install.sh --dry-run       # print every command it would run, change nothing
#
# What it will NOT do, deliberately:
#
#   * Install CDP. Those ~250 binaries are a separate download from
#     https://www.composersdesktop.com/ that you place yourself; the app has a configured
#     directory setting for them (CDP Setup in the app). Nothing here can accept that licence
#     on your behalf.
#   * Touch your system Python. The `py` process group's dependencies go in a virtual
#     environment this app owns. Arch and recent Debian mark the system interpreter
#     externally-managed (PEP 668) and reject `pip install` outright, and working around that
#     with --break-system-packages puts packages somewhere the distro's own updates fight over.
#   * Install anything into your Praat preferences folder. The app redirects Praat to a
#     directory of its own instead, so a `plugin_AudioTools` you already have is left alone.

set -euo pipefail

ASSUME_YES=0
WANT_PYTHON=1
WANT_PRAAT=1
DRY_RUN=0

for arg in "$@"; do
  case "$arg" in
    -y|--yes)      ASSUME_YES=1 ;;
    --no-python)   WANT_PYTHON=0 ;;
    --no-praat)    WANT_PRAAT=0 ;;
    --dry-run)     DRY_RUN=1 ;;
    -h|--help)     sed -n '3,26p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *)             echo "unknown option: $arg (try --help)" >&2; exit 2 ;;
  esac
done

# --- output -------------------------------------------------------------------------------
if [ -t 1 ]; then
  BOLD=$(printf '\033[1m'); DIM=$(printf '\033[2m'); RED=$(printf '\033[31m')
  GREEN=$(printf '\033[32m'); YELLOW=$(printf '\033[33m'); RESET=$(printf '\033[0m')
  # Python package names, as GREEN is process names. A distinct colour because the two appear in
  # the same sentence all through the Python section — "librosa enables AI Conductor Mix" names
  # one of each, and which half is the thing you install is the whole point of that line.
  BLUE=$(printf '\033[94m')
else
  BOLD=""; DIM=""; RED=""; GREEN=""; YELLOW=""; BLUE=""; RESET=""
fi

step()  { printf '\n%s==>%s %s%s%s\n' "$BOLD" "$RESET" "$BOLD" "$*" "$RESET"; }
info()  { printf '    %s\n' "$*"; }
ok()    { printf '    %s✓%s %s\n' "$GREEN" "$RESET" "$*"; }
warn()  { printf '    %s!%s %s\n' "$YELLOW" "$RESET" "$*"; }
die()   { printf '\n%serror:%s %s\n' "$RED" "$RESET" "$*" >&2; exit 1; }

# Runs a command, or prints it under --dry-run. Every mutating action goes through this, so
# --dry-run is honest rather than approximate.
run() {
  if [ "$DRY_RUN" = 1 ]; then
    printf '    %s$ %s%s\n' "$DIM" "$*" "$RESET"
  else
    "$@"
  fi
}

# Asks before doing something the user might not want. `--yes` answers yes; a non-interactive
# shell without `--yes` answers *no*, so piping this into sh can never silently sudo.
confirm() {
  [ "$ASSUME_YES" = 1 ] && return 0
  [ -t 0 ] || { warn "not interactive and --yes was not given; skipping"; return 1; }
  printf '    %s? %s [y/N] ' "$YELLOW$RESET" "$1"
  read -r reply
  case "$reply" in [yY]*) return 0 ;; *) return 1 ;; esac
}

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

# --- long-running command with live feedback ----------------------------------------------
#
# Every pip install below goes through this. It exists because `pip install --quiet` printed
# nothing at all for however long it ran, and on macOS that is routinely many minutes: when a
# wheel is missing for your Python version, pip silently falls back to *compiling from source*.
# The install looked frozen and there was no way to tell a slow build from a wedged one.
#
# So: announce the package before starting, tick a live elapsed timer, and keep the full output
# in a log that is printed only if the command fails. `pip --progress-bar off` because its bar
# fights the timer for the same line; the timer is the better signal, being visible on a
# non-tty too.
LOGDIR=""
run_with_progress() {
  label="$1"; shift
  if [ "$DRY_RUN" = 1 ]; then
    printf '    %s$ %s%s\n' "$DIM" "$*" "$RESET"
    return 0
  fi
  [ -n "$LOGDIR" ] || LOGDIR=$(mktemp -d 2>/dev/null || echo /tmp)
  # Colour codes stripped before the label becomes a filename: `tr -c` would otherwise turn the
  # escapes into underscores, and the path printed on failure is the one thing here a user has
  # to read back to us. A regex rather than a shell glob, because in a glob `[0-9;]*` is one
  # class character followed by an unbounded wildcard — it matches straight through `94mnumpy`
  # to the *last* `m` and takes the package name with it.
  esc=$(printf '\033')
  log="$LOGDIR/$(printf '%s' "$label" | sed "s/${esc}\[[0-9;]*m//g" | tr -c 'A-Za-z0-9' '_').log"

  "$@" >"$log" 2>&1 &
  pid=$!
  start=$(date +%s)
  note=""
  ticks=0
  while kill -0 "$pid" 2>/dev/null; do
    now=$(date +%s); elapsed=$(( now - start ))
    # Say *why* it is slow rather than only that it is. A source build is the one cause that
    # takes minutes, and pip announces it in the log before it starts.
    if [ -z "$note" ] && grep -qi 'building wheel\|setup.py\|pyproject.toml (PEP 517)' "$log" 2>/dev/null; then
      note=" — building from source, this can take 10+ minutes"
    fi
    if [ -t 1 ]; then
      printf '\r    %s …%s  [ %sm%02ds ]  ' "$label" "$note" "$(( elapsed / 60 ))" "$(( elapsed % 60 ))"
    elif [ "$ticks" -gt 0 ] && [ $(( ticks % 30 )) = 0 ]; then
      # Counted in loop iterations rather than off the clock: testing `elapsed % 30` and then
      # sleeping to avoid a duplicate line drifts the heartbeat off its own cadence.
      printf '    %s … still working (%ss)%s\n' "$label" "$elapsed" "$note"
    fi
    ticks=$(( ticks + 1 ))
    sleep 1
  done
  wait "$pid"; status=$?
  now=$(date +%s); elapsed=$(( now - start ))
  [ -t 1 ] && printf '\r%s\r' "                                                                            "
  if [ "$status" = 0 ]; then
    ok "$label (${elapsed}s)"
  else
    warn "$label FAILED after ${elapsed}s — last 20 lines of $log:"
    tail -20 "$log" | sed 's/^/      /'
  fi
  return $status
}

# The newest Python that has prebuilt numpy/scipy wheels, falling back to plain `python3`.
#
# Ordered newest-first among versions known to ship wheels, because the alternative is what made
# this slow in the first place: a brand-new interpreter (macOS installers are already shipping
# 3.14) has no wheels yet, so pip compiles scipy from source and the install takes tens of
# minutes instead of seconds. Nothing here is pinned or installed — it only picks among the
# interpreters already present.
pick_python() {
  for candidate in python3.13 python3.12 python3.11 python3.10; do
    # Must actually *run*, not merely exist on PATH. A pyenv install leaves a shim for every
    # version it knows about, so `command -v python3.12` succeeds on a machine where running it
    # prints "command not found" and exits non-zero — found by dry-running this on a box with
    # pyenv. Creating the venv is the thing that would have failed, several steps later.
    if "$candidate" -c 'import venv' >/dev/null 2>&1; then echo "$candidate"; return 0; fi
  done
  echo python3
}

# --- platform -----------------------------------------------------------------------------
OS="$(uname -s)"
case "$OS" in
  Darwin) PLATFORM=macos ;;
  Linux)  PLATFORM=linux ;;
  *)      die "unsupported platform: $OS (this script covers macOS and Linux)" ;;
esac

# The package manager, and the exact command to install with it. Kept as one place so every
# later step can just say what it needs.
PKG=""
if [ "$PLATFORM" = macos ]; then
  have brew && PKG=brew
else
  for candidate in pacman apt-get dnf zypper apk; do
    have "$candidate" && { PKG="$candidate"; break; }
  done
fi

# Prints the install command for a package, or nothing if we do not know this manager.
pkg_install_cmd() {
  case "$PKG" in
    brew)    echo "brew install $*" ;;
    pacman)  echo "sudo pacman -S --needed $*" ;;
    apt-get) echo "sudo apt-get install -y $*" ;;
    dnf)     echo "sudo dnf install -y $*" ;;
    zypper)  echo "sudo zypper install -y $*" ;;
    apk)     echo "sudo apk add $*" ;;
    *)       echo "" ;;
  esac
}

# Installs system packages, asking first when it needs sudo. Returns non-zero if it did not.
install_packages() {
  local what="$1"; shift
  local cmd; cmd="$(pkg_install_cmd "$@")"
  [ -n "$cmd" ] || { warn "no known package manager; install $* yourself"; return 1; }
  info "$what needs: $*"
  info "would run: $cmd"
  confirm "run that now?" || { warn "skipped"; return 1; }
  # shellcheck disable=SC2086
  run $cmd
}

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$REPO"
[ -f Cargo.toml ] || die "run this from the tui-wave checkout (no Cargo.toml here)"

printf '%stui-wave installer%s  —  %s, %s\n' "$BOLD" "$RESET" "$PLATFORM" "${PKG:-no package manager found}"
[ "$DRY_RUN" = 1 ] && warn "dry run: nothing will be changed"

# --- 1. Rust ------------------------------------------------------------------------------
step "Rust toolchain"
if have cargo; then
  ok "cargo $(cargo --version | awk '{print $2}')"
else
  info "cargo not found; rustup is the supported way to install it"
  if confirm "install Rust via rustup.rs?"; then
    run sh -c "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y"
    # shellcheck disable=SC1091
    [ "$DRY_RUN" = 1 ] || . "$HOME/.cargo/env"
    have cargo || die "rustup finished but cargo is still not on PATH; open a new shell and re-run"
    ok "cargo installed"
  else
    die "cargo is required to build tui-wave"
  fi
fi

# --- 2. Build-time system libraries -------------------------------------------------------
step "Build dependencies"
if [ "$PLATFORM" = macos ]; then
  ok "CoreAudio is part of the system; nothing to install"
else
  # cpal needs ALSA's development headers on Linux. Everything else the build needs
  # (flacenc, mp3lame-encoder) vendors its own C and only wants a working compiler.
  case "$PKG" in
    pacman)  ALSA_PKG=alsa-lib ;;
    apt-get) ALSA_PKG=libasound2-dev ;;
    dnf)     ALSA_PKG=alsa-lib-devel ;;
    zypper)  ALSA_PKG=alsa-devel ;;
    apk)     ALSA_PKG=alsa-lib-dev ;;
    *)       ALSA_PKG="" ;;
  esac
  if pkg-config --exists alsa 2>/dev/null; then
    ok "ALSA development headers present"
  elif [ -n "$ALSA_PKG" ]; then
    install_packages "audio output (cpal)" "$ALSA_PKG" || warn "the build will fail without ALSA headers"
  else
    warn "install your distribution's ALSA development package before building"
  fi
fi

# --- 3. Praat -----------------------------------------------------------------------------
step "Praat"
if [ "$WANT_PRAAT" = 0 ]; then
  info "skipped (--no-praat); the ~430 Praat processes will be unavailable"
elif have praat; then
  ok "praat found: $(command -v praat)"
else
  info "Praat drives about 430 of this app's processes; without it only CDP is available"
  install_packages "Praat" praat || warn "install Praat yourself from https://www.fon.hum.uva.nl/praat/"
fi

# --- 4. The praatAudioTools submodule -----------------------------------------------------
step "praatAudioTools scripts"
# Same credit setup-environment.sh prints where it clones them: about 439 of the catalog's
# processes are this project's work, run as-is and never modified.
info "about 439 of tui-wave's processes are scripts from praatAudioTools, by Shai Cohen"
info "(Department of Music, Bar-Ilan University, Israel), MIT-licensed"
info "https://github.com/ShaiCohen-ops/Praat-plugin_AudioTools"
if [ -f third_party/praat-audiotools/setup.praat ] || [ -n "$(ls -A third_party/praat-audiotools 2>/dev/null)" ]; then
  ok "submodule present"
else
  info "the Praat catalog is inert without it"
  run git submodule update --init --recursive
  ok "submodule initialised"
fi

# --- 5. Python venv for the `py` process group --------------------------------------------
#
# Kept entirely inside a venv the app owns. The `py` scripts pick their own interpreter, so the
# app runs a copy with those assignments repointed at this venv (see `model::praat::python`) --
# a PATH-only mechanism worked on Linux and silently did nothing on macOS, where they resolve an
# absolute path.
step "Python backend (optional — the 45 processes in the 'py' group)"
VENV="${XDG_CONFIG_HOME:-$HOME/.config}/tui-wave/praat/pyenv"
info "45 praatAudioTools scripts drive a Python helper; all need ${BLUE}numpy, scipy${RESET} and ${BLUE}soundfile${RESET}"
info "(plus ${BLUE}sounddevice${RESET} and ${BLUE}pillow${RESET} for three interactive editors). They go in a virtual"
info "environment this app owns — your system Python is not touched."
info "Everything else in tui-wave works without them."
if [ "$WANT_PYTHON" = 0 ]; then
  info "skipped (--no-python); the 'py' group will report missing dependencies if used"
elif ! confirm "Install Python dependencies for Praat-AudioTools scripts?"; then
  info "skipped; the 'py' group will report missing dependencies if used"
  info "you can add them later by re-running: ./install.sh"
elif ! have python3; then
  warn "python3 not found; skipping. Install Python 3, then re-run with no other flags."
else
  PYBIN=$(pick_python)
  info "venv: $VENV"
  info "interpreter: $PYBIN ($("$PYBIN" -V 2>&1))"
  if [ "$PYBIN" = python3 ]; then
    info "(no 3.10-3.13 found; if ${BLUE}numpy/scipy${RESET} have no wheel for this version pip will build"
    info "them from source, which is slow but works — the timer below will say so)"
  fi
  if [ -x "$VENV/bin/python3" ]; then
    ok "venv already exists"
  else
    # Debian splits venv support into its own package; fail with that hint rather than a
    # bare traceback from the module.
    if ! "$PYBIN" -c 'import venv' 2>/dev/null; then
      case "$PKG" in
        apt-get) install_packages "Python venv support" python3-venv || true ;;
        *) warn "$PYBIN's venv module is unavailable; install it and re-run" ;;
      esac
    fi
    run mkdir -p "$(dirname "$VENV")"
    run "$PYBIN" -m venv "$VENV"
    ok "venv created"
  fi
  info "installing ${BLUE}numpy, scipy, soundfile, sounddevice${RESET} and ${BLUE}pillow${RESET} (about 60 MB)"
  info "each step prints its own elapsed time; nothing here is silent"
  # One package per call so a stall names the package it is stalled on. `--progress-bar off`
  # because pip's bar and the elapsed timer would fight over the same line.
  PIP="$VENV/bin/pip"
  run_with_progress "upgrading pip" "$PIP" install --disable-pip-version-check --progress-bar off --upgrade pip \
    || warn "could not upgrade pip; continuing with the version the venv shipped"
  for pkg in numpy scipy soundfile; do
    run_with_progress "installing ${BLUE}$pkg${RESET}" "$PIP" install --disable-pip-version-check --progress-bar off "$pkg" \
      || die "${BLUE}$pkg${RESET} failed to install — see the log above; the 'py' group needs all three"
  done
  if [ "$DRY_RUN" = 0 ]; then
    "$VENV/bin/python3" -c 'import numpy, scipy, soundfile' \
      && ok "${BLUE}numpy, scipy, soundfile${RESET} import cleanly" \
      || die "the venv was created but the packages did not import"
  fi
  # Needed only by the three interactive editors (Arranger, Performance Launcher, Spectral
  # Eraser). Installed by default because those processes are in the catalog, but a failure
  # here is not fatal: sounddevice wants PortAudio at run time and can legitimately be
  # unavailable on a headless machine, which costs three processes and nothing else.
  extras_ok=1
  for pkg in sounddevice pillow; do
    run_with_progress "installing ${BLUE}$pkg${RESET} (interactive editors)" \
      "$PIP" install --disable-pip-version-check --progress-bar off "$pkg" || extras_ok=0
  done
  if [ "$DRY_RUN" = 0 ]; then
    if [ "$extras_ok" = 1 ] && "$VENV/bin/python3" -c 'import sounddevice, PIL' 2>/dev/null; then
      ok "${BLUE}sounddevice, pillow${RESET} ready — ${GREEN}Arranger, Performance Launcher, Spectral Eraser${RESET}"
    else
      warn "${BLUE}sounddevice/pillow${RESET} unavailable — ${GREEN}Arranger${RESET}, ${GREEN}Performance Launcher${RESET}"
      warn "and ${GREEN}Spectral Eraser${RESET} will report missing dependencies; everything else works"
    fi
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
  # Same bargain as everything above: a process whose library is missing still appears in the
  # browser, and the helper's own dependency check names what is absent — so declining costs
  # nothing except the process failing if you run it.
  #
  # Two prompts rather than one, because the sizes are not comparable. Bundling them would make
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
        run_with_progress "installing ${BLUE}$pkg${RESET}" "$PIP" install --disable-pip-version-check \
          --progress-bar off "$pkg" || warn "${BLUE}$pkg${RESET} failed; the processes needing it will say so"
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
        run_with_progress "installing ${BLUE}$pkg${RESET}" "$PIP" install --disable-pip-version-check \
          --progress-bar off "$pkg" || warn "${BLUE}$pkg${RESET} failed; the processes needing it will say so"
      done
    else
      info "skipped; those processes stay listed and name the missing library if run"
    fi
  fi

  # `pedalboard` is deliberately not installed: wheel 0.9.24 aborts with SIGILL on import on
  # some x86-64 CPUs, so VST_Effect_from_Praat is excluded from the catalog regardless.
fi

# --- 6. Build and install -----------------------------------------------------------------
step "Build and install tui-wave"
info "cargo install --path . (release build; this takes a few minutes the first time)"
run cargo install --path . --locked
if [ "$DRY_RUN" = 0 ]; then
  have tui-wave && ok "installed: $(command -v tui-wave)" \
    || warn "installed to ~/.cargo/bin, which is not on your PATH — add it to your shell profile"
fi

# --- 7. What is left for the user ---------------------------------------------------------
step "Done"
info "CDP is not installed by this script. Its ~250 binaries are a separate download from"
info "https://www.composersdesktop.com/ — unpack them anywhere, then point the app at that"
info "folder with CDP Setup. Everything else works without them."
printf '\n    Run %stui-wave <file.wav>%s to start.\n\n' "$BOLD" "$RESET"
