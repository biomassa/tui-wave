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
else
  BOLD=""; DIM=""; RED=""; GREEN=""; YELLOW=""; RESET=""
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
if [ -f third_party/praat-audiotools/setup.praat ] || [ -n "$(ls -A third_party/praat-audiotools 2>/dev/null)" ]; then
  ok "submodule present"
else
  info "the Praat catalog is inert without it"
  run git submodule update --init --recursive
  ok "submodule initialised"
fi

# --- 5. Python venv for the `py` process group --------------------------------------------
#
# Kept entirely inside a venv the app owns. The `py` scripts resolve their interpreter as a
# bare `python3` from PATH, and the app puts this venv's bin at the front of PATH for the Praat
# child — so nothing has to be installed system-wide and nothing is patched in the scripts.
step "Python backend (optional — the 31 processes in the 'py' group)"
VENV="${XDG_CONFIG_HOME:-$HOME/.config}/tui-wave/praat/pyenv"
if [ "$WANT_PYTHON" = 0 ]; then
  info "skipped (--no-python); the 'py' group will report missing dependencies if used"
elif ! have python3; then
  warn "python3 not found; skipping. Install Python 3, then re-run with no other flags."
else
  info "venv: $VENV"
  if [ -x "$VENV/bin/python3" ]; then
    ok "venv already exists"
  else
    # Debian splits venv support into its own package; fail with that hint rather than a
    # bare traceback from the module.
    if ! python3 -c 'import venv' 2>/dev/null; then
      case "$PKG" in
        apt-get) install_packages "Python venv support" python3-venv || true ;;
        *) warn "python3's venv module is unavailable; install it and re-run" ;;
      esac
    fi
    run mkdir -p "$(dirname "$VENV")"
    run python3 -m venv "$VENV"
    ok "venv created"
  fi
  info "installing numpy, scipy and soundfile into it (about 60 MB)"
  run "$VENV/bin/pip" install --quiet --upgrade pip
  run "$VENV/bin/pip" install --quiet numpy scipy soundfile
  if [ "$DRY_RUN" = 0 ]; then
    "$VENV/bin/python3" -c 'import numpy, scipy, soundfile' \
      && ok "numpy, scipy, soundfile ready" \
      || die "the venv was created but the packages did not import"
  fi
  info "optional extras, for four more 'py' processes:"
  info "  $VENV/bin/pip install sounddevice pillow pedalboard"
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
