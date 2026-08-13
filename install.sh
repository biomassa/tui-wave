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
# At the end it offers to delete ./target, which `cargo install --path .` fills with ~500MB the
# installed binary does not need. `--yes` does not answer that one: an unattended run must not
# delete a build cache nobody asked it to touch.
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
    -h|--help)     sed -n '3,29p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
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

# `confirm` for a question `--yes` must not answer on the user's behalf. The only caller is the
# offer to delete the build directory, where a wrong yes costs a rebuild and a wrong no costs
# nothing — so an unattended run has to take the cheap side rather than the convenient one.
confirm_explicitly() {
  [ -t 0 ] || return 1
  printf '    %s? %s [y/N] ' "$YELLOW$RESET" "$1"
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
# Interpreters are tried in this order by both passes below. Absolute paths trail the bare names
# so PATH still decides first; they are there because pyenv's shim directory precedes Homebrew's
# on a normal macOS PATH, and pyenv's interpreters are the ones most likely to lack Tk.
PYTHON_CANDIDATES="python3.13 python3.12 python3.11 python3.10 python3
/opt/homebrew/bin/python3.13 /opt/homebrew/bin/python3.12 /opt/homebrew/bin/python3
/usr/local/bin/python3 /usr/bin/python3"

# The newest interpreter that can build a venv *and* import tkinter, else the newest that can
# build a venv at all.
#
# Tk capability is a preference rather than a requirement because it costs three processes out of
# 456 — but it has to be considered here, at the moment the base interpreter is chosen, since a
# venv cannot acquire Tk afterwards. `pip install` cannot supply `_tkinter`: it is a compiled
# module belonging to the base Python. A Mac with pyenv first on PATH is the case that matters —
# pyenv builds without Tcl/Tk unless it was present at compile time, and the resulting venv fails
# at the moment a window would have opened, with a traceback pointing into ~/.pyenv (user report,
# 2026-08-08). The advice printed then named `brew install python-tk`, which is correct for
# Homebrew's interpreter and does nothing whatever for pyenv's.
pick_python() {
  fallback=""
  for candidate in $PYTHON_CANDIDATES; do
    # Must actually *run*, not merely exist on PATH. A pyenv install leaves a shim for every
    # version it knows about, so `command -v python3.12` succeeds on a machine where running it
    # prints "command not found" and exits non-zero — found by dry-running this on a box with
    # pyenv. Creating the venv is the thing that would have failed, several steps later.
    "$candidate" -c 'import venv' >/dev/null 2>&1 || continue
    if "$candidate" -c 'import tkinter' >/dev/null 2>&1; then echo "$candidate"; return 0; fi
    [ -n "$fallback" ] || fallback="$candidate"
  done
  echo "${fallback:-python3}"
}

# The first interpreter that can build a venv with working Tk, or nothing. Used only to offer a
# repair for a venv that already exists on a Tk-less base — `pick_python` covers new ones.
find_tkinter_python() {
  for candidate in $PYTHON_CANDIDATES; do
    if "$candidate" -c 'import venv, tkinter' >/dev/null 2>&1; then
      command -v "$candidate"
      return 0
    fi
  done
  return 1
}

# Which packaging a venv's *base* interpreter came from, since that alone decides how Tk is
# added to it — and getting this wrong is how the previous advice sent a pyenv user to `brew
# install python-tk`, which targets a different interpreter entirely.
python_flavour() {
  case "$1" in
    */.pyenv/*|*/pyenv/versions/*) echo pyenv ;;
    */Cellar/*|/opt/homebrew/*|/usr/local/opt/*) echo homebrew ;;
    /Library/Frameworks/Python.framework/*) echo python-org ;;
    /System/*|/usr|/usr/bin/*|*/CommandLineTools/*|/Applications/Xcode.app/*) echo system ;;
    *) echo other ;;
  esac
}

# What it takes to give that interpreter Tk. Prints the remedy and nothing else; the caller
# decides whether a rebuild is also on offer.
tkinter_remedy() {
  base="$1" flavour="$2" pyver="$3"
  case "$flavour" in
    pyenv)
      info "the venv is built on pyenv's Python ($base), which was compiled without Tcl/Tk."
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

# --- 4. Bundled sources (git submodules) --------------------------------------------------
step "Bundled sources"
# Same credit setup-environment.sh prints where it clones them: about 456 of the catalog's
# processes are this project's work, run as-is and never modified.
info "about 456 of tui-wave's processes are scripts from praatAudioTools, by Shai Cohen"
info "(Department of Music, Bar-Ilan University, Israel), MIT-licensed"
info "https://github.com/ShaiCohen-ops/Praat-plugin_AudioTools"
info ""
info "500 more are Airwindows effects by Chris Johnson, MIT-licensed, compiled into the"
info "binary from baconpaul's airwin2rack consolidation of them"
info "https://www.airwindows.com  |  https://github.com/baconpaul/airwin2rack"

# Ask git which submodules are uninitialised rather than testing one directory for content.
# The directory test was praatAudioTools-only, so an existing clone that already had *it*
# reported "present" and skipped the rest -- which is how a `git pull` that introduced
# airwin2rack left the build failing on a missing autogen_airwin (user report, 2026-08-12).
# `git submodule status` prefixes an uninitialised entry with `-`, so this notices any
# submodule added in the future without needing to be told about it.
uninitialised_submodules() {
  git submodule status 2>/dev/null | awk '$1 ~ /^-/ { print $2 }'
}

missing_subs="$(uninitialised_submodules)"
if [ -z "$missing_subs" ]; then
  ok "submodules present"
else
  for sub in $missing_subs; do
    info "missing: $sub"
  done
  # `--init`, deliberately **not** `--recursive`: airwin2rack declares submodules of its own
  # (`libs/airwindows`, the entire upstream Airwindows history) that nothing here reads --
  # build.rs compiles the committed `src/autogen_airwin/` tree instead. Recursing would pull
  # hundreds of megabytes to produce nothing. This mirrors the release workflow's own
  # `submodules: true`.
  run git submodule update --init
  still_missing="$(uninitialised_submodules)"
  [ -z "$still_missing" ] || die "submodules still uninitialised: $still_missing"
  ok "submodules initialised"
fi

# --- 5. Python venv for the `py` process group --------------------------------------------
#
# Kept entirely inside a venv the app owns. The `py` scripts pick their own interpreter, so the
# app runs a copy with those assignments repointed at this venv (see `model::praat::python`) --
# a PATH-only mechanism worked on Linux and silently did nothing on macOS, where they resolve an
# absolute path.
step "Python backend (optional — the 46 processes in the 'py' group)"
VENV="${XDG_CONFIG_HOME:-$HOME/.config}/tui-wave/praat/pyenv"
info "46 praatAudioTools scripts drive a Python helper; all need ${BLUE}numpy, scipy${RESET} and ${BLUE}soundfile${RESET}"
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
    # rather than described in the abstract — but never taken automatically, and never by
    # `--yes`: the packages have to come down again, and the machine-learning tier alone is
    # 2.5 GB. What is currently installed is listed first, so the size is visible before the
    # question rather than after it.
    if tkpy=$(find_tkinter_python); then
      info ""
      info "found ${BLUE}$tkpy${RESET}, which does have tkinter"
      # `|| true` because `set -o pipefail` would otherwise abort the script on an empty venv:
      # grep exits 1 when it filters everything out, and that is a legitimate answer here.
      installed=$("$VENV/bin/pip" list --format=freeze --disable-pip-version-check 2>/dev/null \
        | cut -d= -f1 | grep -Ev '^(pip|setuptools|wheel|pkg_resources)$' | tr '\n' ' ' || true)
      venvsize=$(du -sh "$VENV" 2>/dev/null | cut -f1)
      info "rebuilding the venv on it re-downloads what is in it now (${venvsize:-unknown} on disk):"
      info "  ${BLUE}${installed:-nothing}${RESET}"
      if confirm_explicitly "Rebuild the venv on $tkpy?"; then
        # The freeze is taken by *name*, not name==version: a version pinned for one interpreter
        # may have no wheel for another, and building numpy from source is exactly the wait this
        # script exists to avoid. Reinstalling by name gets whatever is current and has a wheel.
        rm -rf "$VENV"
        run_with_progress "creating the venv on $tkpy" "$tkpy" -m venv "$VENV" \
          || die "could not create the venv with $tkpy"
        PIP="$VENV/bin/pip"
        run_with_progress "upgrading pip" "$PIP" install --disable-pip-version-check \
          --progress-bar off --upgrade pip || warn "could not upgrade pip; continuing"
        for pkg in $installed; do
          run_with_progress "installing ${BLUE}$pkg${RESET}" "$PIP" install \
            --disable-pip-version-check --progress-bar off "$pkg" \
            || warn "${BLUE}$pkg${RESET} failed; the processes needing it will say so"
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
  # Same bargain as everything above: a process whose library is missing still appears in the
  # browser, and the helper's own dependency check names what is absent — so declining costs
  # nothing except the process failing if you run it.
  #
  # Two prompts rather than one, because the sizes are not comparable. Bundling them would make
  # "yes" mean a 2.5 GB download for someone who only wanted timbre analysis.
  #
  # torch comes from PyTorch's CPU index on Linux, not from PyPI. The default `pip install torch`
  # wheel drags in the whole CUDA runtime as hard dependencies — cuDNN, cuBLAS, NCCL and the rest,
  # measured at 2.7 GB of `nvidia/*` in a venv on a laptop with no NVIDIA GPU at all, more than
  # doubling it to 6.0 GB. Nothing here can use them: the two ML processes run a speech vocoder
  # and a codec at 16-24 kHz, which is CPU work, and a GPU would be idle-to-marginal even where
  # one exists. The CPU wheels are the same torch, minus a runtime for hardware this application
  # does not address.
  #
  # Linux only, since that is where the split exists: macOS wheels on PyPI are already CPU/MPS
  # builds with no CUDA to avoid, and pointing them at this index would be a change with no
  # benefit. `--index-url` (not `--extra-index-url`) is what PyTorch's own instructions use — it
  # replaces PyPI for that one command, which is the point: the CUDA variant must not be
  # reachable, or pip may resolve back to it.
  TORCH_INDEX=""
  if [ "$PLATFORM" = linux ]; then
    TORCH_INDEX="--index-url https://download.pytorch.org/whl/cpu"
  fi
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
    [ -n "$TORCH_INDEX" ] && info "  CPU builds of ${BLUE}torch${RESET}/${BLUE}torchaudio${RESET} — the CUDA ones add 2.7 GB nothing here uses"
    if confirm "Install the machine-learning libraries? (large download)"; then
      # torch and torchaudio must come from the CPU index *before* the two packages that depend
      # on them: pip stops at "already satisfied", so a CPU torch installed first is what
      # encodec and descript-audio-codec then build on. Installed the other way around, their
      # dependency resolution pulls the CUDA torch from PyPI and the saving is lost.
      for pkg in $missing; do
        case "$pkg" in
          torch|torchaudio) index="$TORCH_INDEX" ;;
          *)                index="" ;;
        esac
        run_with_progress "installing ${BLUE}$pkg${RESET}" "$PIP" install --disable-pip-version-check \
          --progress-bar off $index "$pkg" || warn "${BLUE}$pkg${RESET} failed; the processes needing it will say so"
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

# --- 6b. The build artifacts ---------------------------------------------------------------
#
# `cargo install --path .` builds in *this checkout's* target directory, not a temporary one --
# that is what `--path` changes about it -- and leaves it behind. The installed binary lives in
# ~/.cargo/bin and does not need any of it, so for someone who cloned only to install, it is
# half a gigabyte of nothing.
#
# Offered rather than done, because the same directory is a build cache worth minutes per
# rebuild to anyone who is actually working on this. And deliberately *not* answered by --yes:
# that flag exists so CI and scripted setups can run unattended, and neither should discover it
# has deleted a cache nobody asked it to touch. The prompt is skipped entirely when there is no
# terminal to ask, which is the same outcome as answering no.
if [ "$DRY_RUN" = 0 ] && [ -d target ]; then
  size=$(du -sh target 2>/dev/null | cut -f1)
  if [ -n "$size" ]; then
    step "Build artifacts"
    info "./target holds $size of build files; the installed binary does not need them"
    info "keeping them makes a later rebuild much faster"
    if confirm_explicitly "Remove them with \`cargo clean\`?"; then
      cargo clean && ok "removed; ./target will be rebuilt from scratch next time"
    else
      info "kept; remove them yourself any time with: cargo clean"
    fi
  fi
fi

# --- 7. What is left for the user ---------------------------------------------------------
step "Done"
info "CDP is not installed by this script. Its ~250 binaries are a separate download from"
info "https://www.composersdesktop.com/ — unpack them anywhere, then point the app at that"
info "folder with CDP Setup. Everything else works without them."
printf '\n    Run %stui-wave <file.wav>%s to start.\n\n' "$BOLD" "$RESET"
