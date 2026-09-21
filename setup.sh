#!/usr/bin/env bash
#
# tui-wave setup for macOS and Linux. This is the one script that installs and configures tui-wave.
#
#     ./setup.sh                 # interactive: asks before any step that needs sudo
#     ./setup.sh --yes           # answer yes to every prompt (for CI or a scripted setup)
#     ./setup.sh --no-python     # skip the Python environment for the 'py' process group
#     ./setup.sh --no-praat      # skip the installation of Praat itself
#     ./setup.sh --no-build      # set up the environment only and leave the binary alone
#     ./setup.sh --keep-build    # keep the Rust build directory (source builds only)
#     ./setup.sh --rebuild       # build again even if the installed binary is up to date
#     ./setup.sh --dry-run       # print every command, change nothing
#
# The script looks at what is beside it and does the matching work:
#
#   * A source checkout (Cargo.toml is beside the script). The script installs the Rust
#     toolchain and the build libraries if they are missing. Then it builds tui-wave and
#     installs it to ~/.cargo/bin. The Rust build directory is about 500 MB. It is temporary,
#     and the script deletes it when it ends. If the installed binary was built from exactly
#     this source, the script skips the build. The compiled Airwindows library (28 MB) is kept in
#     ~/.cache/tui-wave, so a build after a change to the Rust code does not compile the C++ again.
#   * An unpacked release tarball (a tui-wave binary is beside the script). The script copies
#     the binary to ~/.local/bin.
#   * A .deb or .rpm package (this script is in /usr/share/tui-wave/), or the copy that is
#     attached to a release page. The binary is already installed, or it is not part of this
#     script. The script sets up the environment only.
#
# In every case the script then prepares what the Praat and Python process groups need:
#
#   1. Praat itself.
#   2. The praatAudioTools scripts, at the exact commit this build was made from.
#   3. The setting that tells tui-wave where those scripts are.
#   4. A Python environment for the 'py' process group.
#
# Run the script again after each update. It moves the scripts to the correct commit again, so
# the scripts and the binary cannot drift apart.
#
# The script does NOT do these things, on purpose:
#
#   * It does not install CDP. The CDP programs are a separate download from
#     https://www.composersdesktop.com/. You put them in a folder yourself, and tui-wave asks
#     for that folder. The script cannot accept the CDP licence for you.
#   * It does not change your system Python. The packages for the 'py' group go in a virtual
#     environment that tui-wave owns. Arch and recent Debian mark the system Python as
#     externally managed (PEP 668) and refuse "pip install". The --break-system-packages
#     option is not a good answer, because it puts files where the package manager also writes.
#   * It does not write to your Praat preferences folder. tui-wave sends Praat to a folder of
#     its own, so a plugin_AudioTools folder that you already have stays as it is.
#
# ## The commit pin is not optional
#
# The process catalog of tui-wave comes from one specific praatAudioTools commit. The catalog is
# compiled into the binary. It holds the name, type, order, and number of every parameter.
# Upstream changes scripts often and without warning: every upstream commit message is
# "Add files via upload". If the scripts are at a different commit, Praat does not report an
# error. Praat fills a script form by position, so it gives audio that sounds plausible and is
# wrong. The test praat_setup_commit_matches_the_catalog makes sure PINNED_COMMIT matches the
# catalog.

set -euo pipefail

# The praatAudioTools commit that this build's catalog was made from. A test compares this
# value with the header of src/model/cdp/praat_catalog.toml. Change it only together with the
# catalog. The update-praat-scripts.sh script does both.
PINNED_COMMIT="90642e21fb7f09754f0fafc034736b6d9d613fb9"
UPSTREAM="https://github.com/ShaiCohen-ops/Praat-plugin_AudioTools"

# Where tui-wave keeps its state. These paths must match config_home() in src/config.rs.
CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
STATE="$CONFIG_HOME/tui-wave/praat"
SCRIPTS="$STATE/audiotools"
VENV="$STATE/pyenv"
CONFIG="$CONFIG_HOME/tui-wave/config.toml"

# The cache folder. The build script of the Airwindows library (crates/airwindows-sys/build.rs)
# keeps its compiled library here, and this script keeps the "installed from" stamp here.
CACHE_HOME="${XDG_CACHE_HOME:-$HOME/.cache}/tui-wave"
STAMP_FILE="$CACHE_HOME/installed-from"

ASSUME_YES=0
WANT_PYTHON=1
WANT_PRAAT=1
WANT_BUILD=1
KEEP_BUILD=0
FORCE_BUILD=0
DRY_RUN=0

for arg in "$@"; do
  case "$arg" in
    -y|--yes)      ASSUME_YES=1 ;;
    --no-python)   WANT_PYTHON=0 ;;
    --no-praat)    WANT_PRAAT=0 ;;
    --no-build)    WANT_BUILD=0 ;;
    --keep-build)  KEEP_BUILD=1 ;;
    --rebuild)     FORCE_BUILD=1 ;;
    --dry-run)     DRY_RUN=1 ;;
    # Print the header up to the commit-pin note. That note is for the person who maintains
    # this file, not for the person who runs it.
    -h|--help)
      awk 'NR > 1 && /^# ## The commit pin/ { exit } NR > 1 { sub(/^# ?/, ""); print }' "$0"
      exit 0 ;;
    *)             echo "unknown option: $arg (try --help)" >&2; exit 2 ;;
  esac
done

# --- Output helpers -----------------------------------------------------------------------
#
# Colors show only when the output is a terminal. Package names are blue and process names are
# green. Both appear in one sentence in the Python section, and the colors show which is which.
if [ -t 1 ]; then
  BOLD=$(printf '\033[1m'); DIM=$(printf '\033[2m'); RED=$(printf '\033[31m')
  GREEN=$(printf '\033[32m'); YELLOW=$(printf '\033[33m'); RESET=$(printf '\033[0m')
  BLUE=$(printf '\033[94m')
else
  BOLD=""; DIM=""; RED=""; GREEN=""; YELLOW=""; BLUE=""; RESET=""
fi

step()  { printf '\n%s==>%s %s%s%s\n' "$BOLD" "$RESET" "$BOLD" "$*" "$RESET"; }
info()  { printf '    %s\n' "$*"; }
ok()    { printf '    %s✓%s %s\n' "$GREEN" "$RESET" "$*"; }
warn()  { printf '    %s!%s %s\n' "$YELLOW" "$RESET" "$*"; }
die()   { printf '\n%serror:%s %s\n' "$RED" "$RESET" "$*" >&2; exit 1; }
have()  { command -v "$1" >/dev/null 2>&1; }

# Every command that changes something goes through run. With --dry-run, run prints the
# command and does not start it. This makes the dry run accurate and not an estimate.
run() {
  if [ "$DRY_RUN" = 1 ]; then
    printf '    %s$ %s%s\n' "$DIM" "$*" "$RESET"
  else
    "$@"
  fi
}

# Ask a yes/no question. Three rules:
#   * --yes answers yes.
#   * --dry-run answers yes, so the dry run shows the whole plan.
#   * With no terminal and no --yes, the answer is no. A script that is piped into a shell
#     can then never run sudo without permission.
confirm() {
  [ "$ASSUME_YES" = 1 ] && return 0
  [ "$DRY_RUN" = 1 ] && return 0
  [ -t 0 ] || { warn "not interactive and --yes was not given; skipping"; return 1; }
  printf '    %s?%s %s [y/N] ' "$YELLOW" "$RESET" "$1"
  read -r reply
  case "$reply" in [yY]*) return 0 ;; *) return 1 ;; esac
}

# Like confirm, but --yes and --dry-run do not answer it. Use it for a step that costs a lot
# when the answer is a wrong yes. An unattended run must take the cheap answer.
confirm_explicitly() {
  [ -t 0 ] || return 1
  printf '    %s?%s %s [y/N] ' "$YELLOW" "$RESET" "$1"
  read -r reply
  case "$reply" in [yY]*) return 0 ;; *) return 1 ;; esac
}

# --- Platform and package manager ---------------------------------------------------------
OS="$(uname -s)"
case "$OS" in
  Darwin) PLATFORM=macos ;;
  Linux)  PLATFORM=linux ;;
  *)      die "unsupported platform: $OS (this script covers macOS and Linux)" ;;
esac

# PKG is the package manager. Later steps use it to install what they need.
PKG=""
if [ "$PLATFORM" = macos ]; then
  have brew && PKG=brew
else
  for candidate in pacman apt-get dnf zypper apk; do
    have "$candidate" && { PKG="$candidate"; break; }
  done
fi

# Print the install command for the packages in the arguments. Print nothing when the package
# manager is not known.
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

# Install system packages. Show the command and ask first, because most managers need sudo.
# Return a non-zero status if the packages were not installed.
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

# --- Python helpers -----------------------------------------------------------------------

# List the packages of one tier that the virtual environment does not have. Each argument is a
# "pip-name:module-name" pair, because the two names often differ (scikit-learn is sklearn).
# The test imports the module. "pip show" is not enough: a package can be installed and still
# fail to import, for example a compiled wheel that is wrong for the CPU.
# The list also lets a second run skip a tier that is already installed, so the script does not
# ask again about a 2.5 GB download. If there is no environment yet, every package is missing.
missing_from_tier() {
  local missing="" spec pkg mod present=1
  [ -x "$VENV/bin/python3" ] || present=0
  for spec in "$@"; do
    pkg=${spec%%:*}; mod=${spec##*:}
    if [ "$present" = 0 ] || ! "$VENV/bin/python3" -c "import $mod" 2>/dev/null; then
      missing="$missing $pkg"
    fi
  done
  printf '%s' "${missing# }"
}

# Run a long command and show that it is alive. Every pip install uses this. Plain
# "pip install --quiet" prints nothing, and on macOS it can run for many minutes when no wheel
# exists for your Python version: pip then builds the package from source. A silent install
# looks frozen.
# This function names the package before it starts and shows a timer. It keeps the full output
# in a log file and prints the end of the log only if the command fails.
LOGDIR=""
run_with_progress() {
  label="$1"; shift
  if [ "$DRY_RUN" = 1 ]; then
    printf '    %s$ %s%s\n' "$DIM" "$*" "$RESET"
    return 0
  fi
  [ -n "$LOGDIR" ] || LOGDIR=$(mktemp -d 2>/dev/null || echo /tmp)
  # The label has color codes in it. Remove them before the label becomes a file name.
  # Use a regular expression here. In a shell pattern, [0-9;]* matches too much and removes
  # the package name together with the code.
  esc=$(printf '\033')
  log="$LOGDIR/$(printf '%s' "$label" | sed "s/${esc}\[[0-9;]*m//g" | tr -c 'A-Za-z0-9' '_').log"

  "$@" >"$log" 2>&1 &
  pid=$!
  start=$(date +%s)
  note=""
  ticks=0
  while kill -0 "$pid" 2>/dev/null; do
    now=$(date +%s); elapsed=$(( now - start ))
    # Say why the command is slow. A build from source is the one cause that takes minutes,
    # and pip writes a message to the log before it starts one.
    if [ -z "$note" ] && grep -qi 'building wheel\|setup.py\|pyproject.toml (PEP 517)' "$log" 2>/dev/null; then
      note=" — building from source, this can take 10+ minutes"
    fi
    if [ -t 1 ]; then
      printf '\r    %s …%s  [ %sm%02ds ]  ' "$label" "$note" "$(( elapsed / 60 ))" "$(( elapsed % 60 ))"
    elif [ "$ticks" -gt 0 ] && [ $(( ticks % 30 )) = 0 ]; then
      # Without a terminal there is no timer. Print one line every 30 seconds instead.
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

# The Python interpreters to try, in order. The bare names come first, so PATH decides first.
# The absolute paths come last. They are there because on a Mac the pyenv shim directory is
# before the Homebrew directory on PATH, and pyenv Pythons often have no Tk.
PYTHON_CANDIDATES="python3.13 python3.12 python3.11 python3.10 python3
/opt/homebrew/bin/python3.13 /opt/homebrew/bin/python3.12 /opt/homebrew/bin/python3
/usr/local/bin/python3 /usr/bin/python3"

# Choose the Python for a new virtual environment. Prefer the newest one that can make an
# environment and import tkinter. If none has tkinter, use the newest one that can make an
# environment. Newer Pythons come first among the ones with prebuilt numpy and scipy wheels.
# A very new Python has no wheels yet, and pip then builds scipy from source, which takes
# tens of minutes.
# The choice must happen now. A virtual environment cannot get tkinter later, because tkinter
# is a compiled module of the base Python and pip cannot supply it.
pick_python() {
  fallback=""
  for candidate in $PYTHON_CANDIDATES; do
    # The candidate must run, not only exist. A pyenv install leaves a shim for every version
    # it knows, and some shims exit with an error when you run them.
    "$candidate" -c 'import venv' >/dev/null 2>&1 || continue
    if "$candidate" -c 'import tkinter' >/dev/null 2>&1; then echo "$candidate"; return 0; fi
    [ -n "$fallback" ] || fallback="$candidate"
  done
  echo "${fallback:-python3}"
}

# Find the first Python that can make an environment and has tkinter. The script uses this only
# to offer a repair for an environment that already exists on a Python without tkinter.
find_tkinter_python() {
  for candidate in $PYTHON_CANDIDATES; do
    if "$candidate" -c 'import venv, tkinter' >/dev/null 2>&1; then
      command -v "$candidate"
      return 0
    fi
  done
  return 1
}

# Say which kind of Python an environment was built on. The kind decides how to add tkinter.
# The wrong advice was once given to a pyenv user: "brew install python-tk" changes the
# Homebrew Python and does nothing for the pyenv one.
python_flavour() {
  case "$1" in
    */.pyenv/*|*/pyenv/versions/*) echo pyenv ;;
    */Cellar/*|/opt/homebrew/*|/usr/local/opt/*) echo homebrew ;;
    /Library/Frameworks/Python.framework/*) echo python-org ;;
    /System/*|/usr|/usr/bin/*|*/CommandLineTools/*|/Applications/Xcode.app/*) echo system ;;
    *) echo other ;;
  esac
}

# Print the steps that give a Python tkinter. The function only prints. The caller decides
# whether to offer a rebuild of the environment.
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

# --- Has the source changed since the last install? ---------------------------------------

# Print a checksum of the program that reads standard input.
hash_stdin() {
  if have sha256sum; then sha256sum | cut -d' ' -f1; else shasum -a 256 | cut -d' ' -f1; fi
}

# Print one checksum that describes the source in this checkout, or print nothing when it cannot
# be worked out (the folder is not a git checkout). The checksum changes when any of these change:
# the commit, the commit of each submodule, a change to a tracked file (staged or not), or a new
# file that git does not ignore.
source_stamp() {
  [ -e .git ] || return 1
  have git || return 1
  {
    git rev-parse HEAD
    git submodule status
    git diff HEAD
    git ls-files --others --exclude-standard -z | while IFS= read -r -d '' file; do
      printf '%s\n' "$file"
      cat "$file"
    done
  } 2>/dev/null | hash_stdin
}

# --- What is this run? --------------------------------------------------------------------
#
# The script decides from the files beside it, not from a flag. So the same file works in each
# place it can be: a clone, an unpacked tarball, /usr/share/tui-wave/, or a lone download.
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$HERE"

if [ -f Cargo.toml ] && [ -d src ] && [ -d third_party ]; then
  MODE=source
elif [ -f tui-wave ] && [ -x tui-wave ]; then
  MODE=tarball
else
  MODE=environment
fi

printf '%stui-wave setup%s  —  %s, %s\n' "$BOLD" "$RESET" "$PLATFORM" "${PKG:-no package manager found}"
case "$MODE" in
  source)      info "source checkout: this run builds and installs tui-wave" ;;
  tarball)     info "release tarball: this run installs the binary that is beside the script" ;;
  environment) info "no source and no binary beside the script: this run sets up the environment only" ;;
esac
[ "$WANT_BUILD" = 1 ] || info "--no-build: the tui-wave binary is not changed"
[ "$DRY_RUN" = 1 ] && warn "dry run: nothing is changed"

# BUILDING is 1 only when there is source to build and the user did not pass --no-build.
BUILDING=0
[ "$MODE" = source ] && [ "$WANT_BUILD" = 1 ] && BUILDING=1

# --- 1. Build tools (source builds only) --------------------------------------------------
#
# Check the Rust toolchain, the audio library, a C++ compiler, and the Airwindows sources. The
# build fails without them, and it is better to find that out now than after some minutes.
if [ "$BUILDING" = 1 ]; then
  step "Rust toolchain"
  if have cargo; then
    ok "cargo $(cargo --version | awk '{print $2}')"
  else
    info "cargo not found. rustup is the supported way to install it."
    if confirm "install Rust with rustup.rs?"; then
      run sh -c "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y"
      # shellcheck disable=SC1091
      [ "$DRY_RUN" = 1 ] || . "$HOME/.cargo/env"
      have cargo || die "rustup finished but cargo is still not on PATH. Open a new shell and run this script again."
      ok "cargo installed"
    else
      die "cargo is required to build tui-wave"
    fi
  fi

  step "Build libraries"
  if [ "$PLATFORM" = macos ]; then
    ok "CoreAudio is part of the system, so there is nothing to install"
  else
    # The audio library (cpal) needs the ALSA development headers on Linux. The other
    # libraries (flacenc, mp3lame-encoder) include their own C code and need only a compiler.
    case "$PKG" in
      pacman)  ALSA_PKG=alsa-lib ;;
      apt-get) ALSA_PKG=libasound2-dev ;;
      dnf)     ALSA_PKG=alsa-lib-devel ;;
      zypper)  ALSA_PKG=alsa-devel ;;
      apk)     ALSA_PKG=alsa-lib-dev ;;
      *)       ALSA_PKG="" ;;
    esac
    if pkg-config --exists alsa 2>/dev/null; then
      ok "ALSA development headers found"
    elif [ -n "$ALSA_PKG" ]; then
      install_packages "audio output (cpal)" "$ALSA_PKG" || warn "the build fails without the ALSA headers"
    else
      warn "install the ALSA development package of your distribution before the build"
    fi
  fi

  # The built-in Airwindows effects are C++ code. The build compiles about 1000 files.
  if have c++ || have g++ || have clang++; then
    ok "C++ compiler found"
  else
    warn "no C++ compiler found. The built-in Airwindows effects need one."
    if [ "$PLATFORM" = macos ]; then
      info "install the Xcode command line tools: xcode-select --install"
    else
      info "Debian/Ubuntu: build-essential | Fedora: gcc-c++ | Arch: base-devel"
    fi
  fi

  step "Airwindows sources"
  info "500 of the effects are Airwindows plugins by Chris Johnson, under the MIT licence."
  info "The build compiles them into the binary. They come from the airwin2rack project by baconpaul."
  info "https://www.airwindows.com  |  https://github.com/baconpaul/airwin2rack"
  # Only airwin2rack is needed to build. The Praat scripts come from their own clone (step 3),
  # so the praat-audiotools submodule stays empty. Do not use --recursive: airwin2rack has
  # submodules of its own with the whole upstream history, and the build does not read them.
  if [ -d third_party/airwin2rack/src/autogen_airwin ]; then
    ok "airwin2rack present"
  else
    have git || die "git is required to fetch the Airwindows sources"
    [ -e .git ] || die "third_party/airwin2rack is empty and this is not a git checkout. Clone the repository with git."
    run git submodule update --init third_party/airwin2rack
    [ "$DRY_RUN" = 1 ] || [ -d third_party/airwin2rack/src/autogen_airwin ] \
      || die "the airwin2rack submodule is still empty after the update"
    ok "airwin2rack fetched"
  fi
fi

# --- 2. Praat -----------------------------------------------------------------------------
#
# Praat runs about 430 of the processes. Without it, only CDP and Airwindows work. Install
# Praat first: the scripts in step 3 do nothing without it.
step "Praat"
if [ "$WANT_PRAAT" = 0 ]; then
  info "skipped (--no-praat). The Praat processes will not work."
elif have praat; then
  ok "praat found: $(command -v praat)"
else
  info "Praat runs about 430 of the processes. Without it, only CDP and Airwindows work."
  install_packages "Praat" praat || warn "install Praat yourself from https://www.fon.hum.uva.nl/praat/"
fi

# --- 3. The praatAudioTools scripts -------------------------------------------------------
#
# About 456 processes are scripts from the praatAudioTools project. No package bundles them.
# This step clones the project into the tui-wave state folder and moves it to PINNED_COMMIT.
# Then it writes the path into the config file.
#
# This step runs for every mode, source builds too. One clone in one place is easier to keep
# correct than a submodule inside a checkout that the user can move or delete. And the step
# corrects an old path in the config file. An old path is what caused the message "scripts are
# at X but this build expects Y".

# Move the clone to PINNED_COMMIT. A plain checkout can fail on a file system that ignores
# case (the default on macOS). The project has four pairs of scripts in one folder with names
# that differ only in case, for example Stereo_Shimmer.praat and stereo_shimmer.praat. On such
# a file system both names are one file, and git reports the second one as changed by you.
# Then git refuses to move. Nothing is lost by a forced checkout, because this clone belongs to
# tui-wave. The app runs each script from a temporary copy and never writes to the clone.
checkout_pinned_commit() {
  if [ "$DRY_RUN" = 1 ]; then
    printf '    %s$ git -C %s checkout --detach %s%s\n' "$DIM" "$SCRIPTS" "$PINNED_COMMIT" "$RESET"
    return 0
  fi
  if git -C "$SCRIPTS" checkout --quiet --detach "$PINNED_COMMIT" 2>/dev/null; then
    return 0
  fi
  warn "the checkout refused to move, so the script forces it"
  warn "(this is normal on macOS: four scripts differ only in case and cannot exist together)"
  git -C "$SCRIPTS" checkout --quiet --detach --force "$PINNED_COMMIT" \
    || die "could not move $SCRIPTS to $PINNED_COMMIT. Delete it and run this script again to clone it again."
}

step "praatAudioTools scripts"
have git || die "git is required to fetch the scripts. Install it and run this script again."
info "about 456 processes are scripts from this project, by Shai Cohen"
info "(Department of Music, Bar-Ilan University, Israel), under the MIT licence"
info "$UPSTREAM"
info "target: $SCRIPTS"
info "commit: $PINNED_COMMIT"

if [ -d "$SCRIPTS/.git" ]; then
  current="$(git -C "$SCRIPTS" rev-parse HEAD 2>/dev/null || echo unknown)"
  if [ "$current" = "$PINNED_COMMIT" ]; then
    ok "already at the correct commit"
  else
    info "the clone is at ${current:0:7} but this build needs ${PINNED_COMMIT:0:7}"
    # Fetch and do not clone again. The clone is large and usually only a few commits behind.
    # A detached HEAD is correct: this is a pinned dependency and nobody works on a branch.
    run git -C "$SCRIPTS" fetch --quiet origin || die "could not fetch from $UPSTREAM"
    # Find out whether the commit arrived before the checkout. Then the two real failures
    # have two messages. A missing commit means this script is newer than the upstream clone.
    # A refused checkout means something in the folder is in the way. Skip the test in a dry
    # run: the fetch was only printed, so the commit is missing because nothing ran.
    if [ "$DRY_RUN" != 1 ] \
      && ! git -C "$SCRIPTS" cat-file -e "${PINNED_COMMIT}^{commit}" 2>/dev/null; then
      die "commit $PINNED_COMMIT was not found upstream. Is this script newer than the clone?"
    fi
    checkout_pinned_commit
    ok "moved to ${PINNED_COMMIT:0:7}"
  fi
else
  [ -e "$SCRIPTS" ] && die "$SCRIPTS exists but is not a git clone. Move it aside and run this script again."
  run mkdir -p "$(dirname "$SCRIPTS")"
  run git clone --quiet "$UPSTREAM" "$SCRIPTS" || die "could not clone $UPSTREAM"
  checkout_pinned_commit
  ok "cloned at ${PINNED_COMMIT:0:7}"
fi

# --- 4. Tell tui-wave where the scripts are ------------------------------------------------
#
# A binary in ~/.cargo/bin or /usr/bin cannot find the scripts by looking beside itself. The
# config key praat_audiotools_dir is what makes an installed binary find them. This step
# replaces the key if it exists. A second copy of the key makes the file invalid, and tui-wave
# then ignores the whole file and loses every other setting.
step "Configuration"
info "config: $CONFIG"
if [ "$DRY_RUN" = 1 ]; then
  printf '    %s$ set praat_audiotools_dir = "%s" in %s%s\n' "$DIM" "$SCRIPTS" "$CONFIG" "$RESET"
elif grep -q '^praat_audiotools_dir[[:space:]]*=' "$CONFIG" 2>/dev/null; then
  tmp="$(mktemp)"
  sed "s|^praat_audiotools_dir[[:space:]]*=.*|praat_audiotools_dir = \"$SCRIPTS\"|" "$CONFIG" > "$tmp"
  cat "$tmp" > "$CONFIG"
  rm -f "$tmp"
  ok "praat_audiotools_dir updated"
else
  mkdir -p "$(dirname "$CONFIG")"
  printf 'praat_audiotools_dir = "%s"\n' "$SCRIPTS" >> "$CONFIG"
  ok "praat_audiotools_dir written"
fi

# --- 5. Python environment for the 'py' process group -------------------------------------
#
# Everything goes into a virtual environment that tui-wave owns. The system Python is not
# changed. The scripts of the 'py' group choose their own Python, so tui-wave runs a copy of
# each script with that choice pointed at this environment (see src/model/praat/python.rs).
# A PATH-only method worked on Linux and did nothing on macOS, where the scripts use an
# absolute path.
step "Python backend (optional — the 'py' process group)"
info "46 praatAudioTools scripts use a Python helper. They need ${BLUE}numpy, scipy${RESET} and ${BLUE}soundfile${RESET}."
info "Three interactive editors also need ${BLUE}sounddevice${RESET} and ${BLUE}pillow${RESET}."
info "The packages go in a virtual environment that tui-wave owns. Your system Python is not changed."
info "Everything else in tui-wave works without them."
if [ "$WANT_PYTHON" = 0 ]; then
  info "skipped (--no-python). The 'py' group reports missing dependencies if you use it."
elif ! confirm "Install the Python dependencies for the praatAudioTools scripts?"; then
  info "skipped. The 'py' group reports missing dependencies if you use it."
  info "To add them later, run this script again: ./setup.sh"
elif ! have python3; then
  warn "python3 not found, so this step is skipped. Install Python 3 and run this script again."
else
  PYBIN=$(pick_python)
  info "venv: $VENV"
  info "interpreter: $PYBIN ($("$PYBIN" -V 2>&1))"
  if [ "$PYBIN" = python3 ]; then
    info "(no Python 3.10 to 3.13 found. If ${BLUE}numpy/scipy${RESET} have no wheel for this version, pip builds"
    info "them from source. This is slow but it works, and the timer below says so.)"
  fi
  if [ -x "$VENV/bin/python3" ]; then
    ok "venv already exists"
  else
    # Debian puts venv support in a separate package. Give that hint and not a bare traceback.
    if ! "$PYBIN" -c 'import venv' 2>/dev/null; then
      case "$PKG" in
        apt-get) install_packages "Python venv support" python3-venv || true ;;
        *) warn "the venv module of $PYBIN is not available. Install it and run this script again." ;;
      esac
    fi
    run mkdir -p "$(dirname "$VENV")"
    run "$PYBIN" -m venv "$VENV"
    ok "venv created"
  fi
  info "installing ${BLUE}numpy, scipy, soundfile, sounddevice${RESET} and ${BLUE}pillow${RESET} (about 60 MB)"
  info "each step shows its own elapsed time, so nothing is silent"
  # One package for each call, so a stall names the package. The pip progress bar is off,
  # because it and the timer write to the same line.
  PIP="$VENV/bin/pip"
  run_with_progress "upgrading pip" "$PIP" install --disable-pip-version-check --progress-bar off --upgrade pip \
    || warn "could not upgrade pip. The script continues with the version that the venv has."
  for pkg in numpy scipy soundfile; do
    run_with_progress "installing ${BLUE}$pkg${RESET}" "$PIP" install --disable-pip-version-check --progress-bar off "$pkg" \
      || die "${BLUE}$pkg${RESET} failed to install. See the log above. The 'py' group needs all three."
  done
  if [ "$DRY_RUN" = 0 ]; then
    "$VENV/bin/python3" -c 'import numpy, scipy, soundfile' \
      && ok "${BLUE}numpy, scipy, soundfile${RESET} import correctly" \
      || die "the venv exists but the packages do not import"
  fi

  # Only three interactive editors need these two packages (Arranger, Performance Launcher,
  # Spectral Eraser). A failure is not fatal. sounddevice needs PortAudio at run time, and a
  # machine with no audio hardware can legitimately lack it. The loss is three processes.
  extras_ok=1
  for pkg in sounddevice pillow; do
    run_with_progress "installing ${BLUE}$pkg${RESET} (interactive editors)" \
      "$PIP" install --disable-pip-version-check --progress-bar off "$pkg" || extras_ok=0
  done
  if [ "$DRY_RUN" = 0 ]; then
    if [ "$extras_ok" = 1 ] && "$VENV/bin/python3" -c 'import sounddevice, PIL' 2>/dev/null; then
      ok "${BLUE}sounddevice, pillow${RESET} ready — ${GREEN}Arranger, Performance Launcher, Spectral Eraser${RESET}"
    else
      warn "${BLUE}sounddevice/pillow${RESET} not available — ${GREEN}Arranger${RESET}, ${GREEN}Performance Launcher${RESET}"
      warn "and ${GREEN}Spectral Eraser${RESET} report missing dependencies. Everything else works."
    fi
  fi

  # Check for tkinter. Nothing else checks for it. It is part of the Python standard library,
  # but it is a compiled module (_tkinter, linked to Tcl/Tk), and many distributions and
  # Homebrew sell it as a separate package. pip cannot supply it, because it belongs to the
  # base Python and not to the virtual environment.
  # This matters most on macOS: Homebrew Python has no tkinter, and an environment built on it
  # has the same gap. Then Arranger works on Linux and fails on the Mac with "No module named
  # tkinter". The three processes that need it import it late, so the error shows only when
  # the window would open.
  # This is a warning and not an error: the loss is three processes.
  if [ "$DRY_RUN" = 0 ] && ! "$VENV/bin/python3" -c 'import tkinter' 2>/dev/null; then
    warn "this Python has no ${BLUE}tkinter${RESET} — ${GREEN}Arranger${RESET}, ${GREEN}Performance Launcher${RESET}"
    warn "and ${GREEN}Spatial Panner${RESET} fail with \"No module named 'tkinter'\""
    info "every other process is not affected"
    pyver=$("$VENV/bin/python3" -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")' 2>/dev/null)
    base=$("$VENV/bin/python3" -c 'import sys; print(sys.base_prefix)' 2>/dev/null)
    tkinter_remedy "$base" "$(python_flavour "$base")" "$pyver"
    info "pip cannot install it. It is part of the base Python that this venv was built on."

    # The base Python of an environment is fixed when the environment is made. The only repair
    # is to build the environment again on a different Python. The script offers this only when
    # such a Python is present. It never does it alone, and --yes does not answer the question:
    # all packages are downloaded again, and the machine-learning tier alone is 2.5 GB. The
    # script lists what is installed now, so you see the size before you answer.
    if tkpy=$(find_tkinter_python); then
      info ""
      info "found ${BLUE}$tkpy${RESET}, which has tkinter"
      # "|| true" is needed. With pipefail, grep ends the script when it removes every line,
      # and that result is valid for an empty environment.
      installed=$("$VENV/bin/pip" list --format=freeze --disable-pip-version-check 2>/dev/null \
        | cut -d= -f1 | grep -Ev '^(pip|setuptools|wheel|pkg_resources)$' | tr '\n' ' ' || true)
      venvsize=$(du -sh "$VENV" 2>/dev/null | cut -f1)
      info "a new venv on it downloads again what is in the venv now (${venvsize:-unknown} on disk):"
      info "  ${BLUE}${installed:-nothing}${RESET}"
      if confirm_explicitly "Rebuild the venv on $tkpy?"; then
        # Install by name and not by name==version. A version that fits one Python can have no
        # wheel for another, and a build of numpy from source is the wait that this avoids.
        rm -rf "$VENV"
        run_with_progress "creating the venv on $tkpy" "$tkpy" -m venv "$VENV" \
          || die "could not create the venv with $tkpy"
        PIP="$VENV/bin/pip"
        run_with_progress "upgrading pip" "$PIP" install --disable-pip-version-check \
          --progress-bar off --upgrade pip || warn "could not upgrade pip. The script continues."
        for pkg in $installed; do
          run_with_progress "installing ${BLUE}$pkg${RESET}" "$PIP" install \
            --disable-pip-version-check --progress-bar off "$pkg" \
            || warn "${BLUE}$pkg${RESET} failed. The processes that need it say so when you run them."
        done
        if "$VENV/bin/python3" -c 'import tkinter' 2>/dev/null; then
          ok "rebuilt — ${GREEN}Arranger, Performance Launcher, Spatial Panner${RESET} open now"
        else
          warn "the new venv still has no tkinter. The steps above are the remaining way."
        fi
      else
        info "kept as it is. The three Tk processes stay unavailable."
      fi
    fi
  elif [ "$DRY_RUN" = 0 ]; then
    ok "${BLUE}tkinter${RESET} present — ${GREEN}Arranger, Performance Launcher, Spatial Panner${RESET}"
  fi

  # Optional tiers. A process whose library is missing stays in the browser. Its own check
  # names the missing library when you run it. If you say no here, the only cost is that the
  # process fails when you use it.
  # There are two prompts and not one, because the sizes are very different. One prompt would
  # make "yes" mean a 2.5 GB download for a person who wants only the timbre analysis.
  #
  # On Linux, torch comes from the CPU index of PyTorch and not from PyPI. The default wheel
  # needs the whole CUDA runtime (cuDNN, cuBLAS, NCCL, and more). That is 2.7 GB of nvidia/*
  # files, measured on a laptop with no NVIDIA GPU. The two ML processes run a speech vocoder
  # and a codec at 16 to 24 kHz. That is CPU work, so the CUDA files are of no use. On macOS
  # the PyPI wheels are already CPU/MPS builds, so nothing changes there.
  # The script uses --index-url and not --extra-index-url. --index-url replaces PyPI for that
  # one command, so pip cannot resolve back to the CUDA build.
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
      info "Optional: analysis libraries — ${BLUE}$missing${RESET} (the others are already installed)"
    fi
    info "  enables ${GREEN}AI Conductor Mix, Dereverberation, IdentitySeparation,${RESET}"
    info "  ${GREEN}RF Concatenative, Recomposer (x2), Semantic timbre retrieval${RESET}"
    info "  and ${GREEN}ThermodynamicTransform${RESET}"
    if confirm "Install the analysis libraries?"; then
      for pkg in $missing; do
        run_with_progress "installing ${BLUE}$pkg${RESET}" "$PIP" install --disable-pip-version-check \
          --progress-bar off "$pkg" || warn "${BLUE}$pkg${RESET} failed. The processes that need it say so."
      done
    else
      info "skipped. Those processes stay in the browser and name the missing library if you run them."
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
      info "Optional: machine-learning libraries — ${BLUE}$missing${RESET} (the others are already installed)"
    fi
    info "  enables ${GREEN}AcousticDNAResonator, HierarchicalRecomposition,${RESET}"
    info "  ${GREEN}IRCAM rave model${RESET} and ${GREEN}NeuralResynthesisVocoder${RESET}"
    info "  some ML processes also need model files that you supply yourself"
    [ -n "$TORCH_INDEX" ] && info "  CPU builds of ${BLUE}torch${RESET}/${BLUE}torchaudio${RESET} — the CUDA builds add 2.7 GB that nothing here uses"
    if confirm "Install the machine-learning libraries? (large download)"; then
      # Install torch and torchaudio first, from the CPU index. The other two packages depend
      # on them. pip stops at "already satisfied", so encodec and descript-audio-codec then use
      # the CPU torch. In the other order, their dependency search pulls the CUDA torch from
      # PyPI and the saving is lost.
      for pkg in $missing; do
        case "$pkg" in
          torch|torchaudio) index="$TORCH_INDEX" ;;
          *)                index="" ;;
        esac
        run_with_progress "installing ${BLUE}$pkg${RESET}" "$PIP" install --disable-pip-version-check \
          --progress-bar off $index "$pkg" || warn "${BLUE}$pkg${RESET} failed. The processes that need it say so."
      done
    else
      info "skipped. Those processes stay in the browser and name the missing library if you run them."
    fi
  fi

  # The package pedalboard is not installed on purpose. Its wheel 0.9.24 crashes with SIGILL on
  # import on some x86-64 CPUs. So VST_Effect_from_Praat is not in the catalog in any case.
fi

# --- 6. The tui-wave binary ---------------------------------------------------------------
#
# This is the last step, and the long one. All questions come first, so you can leave the
# computer while the build runs.

# BUILD_DIR is the temporary Rust build directory. The script sets it only after mktemp makes
# the directory, so the cleanup below can remove nothing else.
BUILD_DIR=""
cleanup_build_dir() {
  if [ -n "$BUILD_DIR" ] && [ -d "$BUILD_DIR" ]; then
    rm -rf "$BUILD_DIR"
  fi
}
# The trap runs when the script ends for any reason: success, error, or Ctrl+C.
trap cleanup_build_dir EXIT

# The binary that "cargo install" writes. The build is skipped only if this file exists and the
# stamp shows that it was built from the source that is here now.
INSTALLED_BIN="${CARGO_HOME:-$HOME/.cargo}/bin/tui-wave"
SOURCE_STAMP=""
UP_TO_DATE=0
if [ "$BUILDING" = 1 ]; then
  SOURCE_STAMP="$(source_stamp || true)"
  if [ "$FORCE_BUILD" = 0 ] && [ -n "$SOURCE_STAMP" ] && [ -x "$INSTALLED_BIN" ] \
     && [ "$(cat "$STAMP_FILE" 2>/dev/null || true)" = "$SOURCE_STAMP" ]; then
    UP_TO_DATE=1
  fi
fi

if [ "$UP_TO_DATE" = 1 ]; then
  step "Build and install tui-wave"
  ok "already up to date: $INSTALLED_BIN was built from exactly this source"
  info "use --rebuild to build it again"
elif [ "$BUILDING" = 1 ]; then
  step "Build and install tui-wave"
  # Remove the stamp first. If the build fails or stops, the next run must build again.
  [ "$DRY_RUN" = 1 ] || rm -f "$STAMP_FILE"
  # cargo install --path . builds in a target directory and leaves it there. That directory
  # is about 500 MB. The installed binary does not need it, and on a machine where nobody
  # expects it, it fills the disk. So the build goes to a temporary directory that the script
  # deletes at the end. A ./target directory that you already have is not touched.
  # Use --keep-build to build in ./target and keep it, for example to build again often.
  if [ "$KEEP_BUILD" = 1 ]; then
    info "--keep-build: the build files stay in ./target"
    info "cargo install --path . (release build, a few minutes the first time)"
    run cargo install --path . --locked
  else
    if [ "$DRY_RUN" = 1 ]; then
      BUILD_DIR="<temporary build directory>"
    else
      BUILD_DIR="$(mktemp -d "${TMPDIR:-/tmp}/tui-wave-build.XXXXXX")" || die "could not make a temporary build directory"
    fi
    info "build files go to $BUILD_DIR and are deleted when the script ends"
    info "the compiled Airwindows library is kept in $CACHE_HOME/airwindows (28 MB), so it is not compiled again"
    info "(use --keep-build to keep them in ./target instead)"
    info "cargo install --path . (release build, a few minutes the first time)"
    run cargo install --path . --locked --target-dir "$BUILD_DIR"
    # In a dry run BUILD_DIR is only a label. Clear it so the trap has nothing to remove.
    [ "$DRY_RUN" = 1 ] && BUILD_DIR=""
  fi
  if [ "$DRY_RUN" = 0 ]; then
    ok "installed: $INSTALLED_BIN"
    # Another tui-wave that comes first on PATH (for example one from a .deb) hides this one.
    first_on_path="$(command -v tui-wave || true)"
    if [ -z "$first_on_path" ]; then
      warn "$(dirname "$INSTALLED_BIN") is not on your PATH. Add it to your shell profile."
    elif [ "$first_on_path" != "$INSTALLED_BIN" ]; then
      warn "another tui-wave comes first on your PATH: $first_on_path"
      info "it hides the one that was just installed. Remove it, or put $(dirname "$INSTALLED_BIN") first on PATH."
    fi
    # Save the stamp. It says which source the installed binary was built from.
    if [ -n "$SOURCE_STAMP" ]; then
      mkdir -p "$CACHE_HOME" && printf '%s\n' "$SOURCE_STAMP" > "$STAMP_FILE" || true
    fi
    if [ "$KEEP_BUILD" = 0 ]; then
      cleanup_build_dir
      BUILD_DIR=""
      ok "build directory removed"
    fi
  fi

elif [ "$MODE" = tarball ] && [ "$WANT_BUILD" = 1 ]; then
  step "Install the tui-wave binary"
  # Install to ~/.local/bin. This needs no sudo. Copy to a temporary name and then rename.
  # Linux refuses to overwrite a binary that is running ("Text file busy"), but it allows a
  # rename over it.
  DEST_DIR="$HOME/.local/bin"
  run mkdir -p "$DEST_DIR"
  run cp "$HERE/tui-wave" "$DEST_DIR/.tui-wave.new"
  run chmod 755 "$DEST_DIR/.tui-wave.new"
  # macOS marks a downloaded file as quarantined, and Gatekeeper then blocks the first run.
  # This is best-effort: the attribute is often absent, and then xattr reports an error.
  if [ "$PLATFORM" = macos ]; then
    run xattr -d com.apple.quarantine "$DEST_DIR/.tui-wave.new" 2>/dev/null || true
  fi
  run mv "$DEST_DIR/.tui-wave.new" "$DEST_DIR/tui-wave"
  ok "installed: $DEST_DIR/tui-wave"
  case ":$PATH:" in
    *":$DEST_DIR:"*) ;;
    *) warn "$DEST_DIR is not on your PATH. Add this line to your shell profile:"
       info "    export PATH=\"\$HOME/.local/bin:\$PATH\"" ;;
  esac

elif [ "$WANT_BUILD" = 1 ] && [ "$MODE" = environment ] && ! have tui-wave; then
  # A .deb or .rpm installs the binary before this script can run, so this case is a lone
  # download of the script.
  step "tui-wave binary"
  warn "no tui-wave binary found beside this script or on your PATH"
  info "Install a release package from https://github.com/biomassa/tui-wave/releases,"
  info "or run this script from a source checkout. The setup above is already done."
fi

# --- 7. What is left for you --------------------------------------------------------------
step "Done"
if [ "$DRY_RUN" = 1 ]; then
  info "dry run — nothing was changed"
  exit 0
fi
ok "the Praat process group is ready"
info ""
info "CDP is not installed by this script. Its ~250 programs are a separate download from"
info "https://www.composersdesktop.com/. Unpack them in any folder. Then use CDP Setup in"
info "tui-wave to point at that folder. Everything else works without them."
printf '\n    Run %stui-wave <file.wav>%s to start. Press Ctrl+P to browse processes.\n\n' "$BOLD" "$RESET"
