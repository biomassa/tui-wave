#!/usr/bin/env bash
# Update the praatAudioTools scripts and rebuild tui-wave with them.
#
#     ./update-praat-scripts.sh              # update, regenerate, rebuild, install
#     ./update-praat-scripts.sh --dry-run    # print every command, change nothing
#     ./update-praat-scripts.sh --yes        # take every prompt as yes
#     ./update-praat-scripts.sh --no-build   # stop after regenerating (for cutting a release)
#
# praatAudioTools is a separate project that gains and reworks scripts constantly. tui-wave
# carries it as a git submodule and generates its process catalog from it, so picking up new
# scripts means three things in step, not one:
#
#   1. move the submodule to upstream's tip,
#   2. re-run scripts/convert_praat_audiotools.py, and
#   3. rebuild, because the catalog is compiled into the binary (`include_str!`).
#
# Doing (1) without (2) leaves the catalog describing scripts that no longer exist — upstream
# renames parameters and reorders form fields, and a stale entry passes the wrong values in the
# wrong order rather than failing cleanly. Doing (1) and (2) without (3) changes nothing you can
# see. This runs all three.
#
# It also deletes leftovers first: several `py` scripts write scratch WAVs and logs next to
# themselves inside the checkout, so a submodule that has run anything is "dirty" to git and
# refuses to update.
#
# Upstream's commit messages are, without exception, "Add files via upload" — so this prints the
# real diff instead: which scripts changed, and which processes appeared or disappeared from the
# catalog. That summary is the only signal there is; read it.

set -euo pipefail

ASSUME_YES=0
DRY_RUN=0
WANT_BUILD=1

for arg in "$@"; do
  case "$arg" in
    -y|--yes)     ASSUME_YES=1 ;;
    --dry-run)    DRY_RUN=1 ;;
    --no-build)   WANT_BUILD=0 ;;
    -h|--help)    sed -n '2,27p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *)            echo "unknown option: $arg (try --help)" >&2; exit 2 ;;
  esac
done

cd "$(dirname "$0")"

if [ -t 1 ]; then
  BOLD=$(printf '\033[1m'); DIM=$(printf '\033[2m'); RED=$(printf '\033[31m')
  GREEN=$(printf '\033[32m'); YELLOW=$(printf '\033[33m'); RESET=$(printf '\033[0m')
else
  BOLD=""; DIM=""; RED=""; GREEN=""; YELLOW=""; RESET=""
fi

step() { printf '\n%s==>%s %s%s%s\n' "$BOLD" "$RESET" "$BOLD" "$*" "$RESET"; }
info() { printf '    %s\n' "$*"; }
ok()   { printf '    %s✓%s %s\n' "$GREEN" "$RESET" "$*"; }
warn() { printf '    %s!%s %s\n' "$YELLOW" "$RESET" "$*"; }
die()  { printf '\n%serror:%s %s\n' "$RED" "$RESET" "$*" >&2; exit 1; }
# `ok` for something that actually happened. Silent under --dry-run, where the command was only
# printed — reporting "rebuilt and installed" after printing `cargo install` is a small lie, and
# a dry run's whole job is to be trusted about what it would do.
did()  { [ "$DRY_RUN" = 1 ] || ok "$*"; }

# Every mutating action goes through this, so --dry-run is honest rather than approximate.
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

SUB=third_party/praat-audiotools
CATALOG=src/model/cdp/praat_catalog.toml

# One process per line as `key<TAB>script.praat`, for diffing the catalog across a regeneration.
# `bin` always follows `key` within a `[[process]]` block, which is what lets this be two awk
# rules rather than a parser. Tab-separated and sorted so `comm` and `join` can both read it.
catalog_processes() {
  [ -f "$CATALOG" ] || return 0
  awk -F'"' '/^key = /{k=$2} /^bin = /{print k"\t"$2}' "$CATALOG" | LC_ALL=C sort
}

# --- 1. Preflight -------------------------------------------------------------------------
step "Checking the working copy"
[ -d .git ] || [ -f .git ] || die "not a git repository — run this from the tui-wave checkout"
[ -f "$CATALOG" ] || die "$CATALOG is missing — is this really the tui-wave repository?"
command -v git >/dev/null 2>&1 || die "git is not installed"
command -v python3 >/dev/null 2>&1 || die "python3 is not installed (the converter needs it)"

if [ ! -d "$SUB/py" ]; then
  info "the submodule is not checked out yet"
  run git submodule update --init "$SUB" || die "could not initialise the submodule"
  did "submodule initialised"
fi

# Refuse over uncommitted edits to *tracked* files, which this would bury. Two exemptions, both
# deliberate: the catalog and the submodule are the things this script rewrites, so re-running
# after an interrupted update — a normal thing to do — must not be blocked by its own output.
#
# Untracked files are not checked at all. Nothing here can touch them, and refusing because
# someone left a notes.txt in the checkout would be a refusal that protects nobody.
DIRTY=$(git status --porcelain --untracked-files=no -- . ":(exclude)$CATALOG" ":(exclude)$SUB" \
        2>/dev/null || true)
if [ -n "$DIRTY" ]; then
  warn "these tracked files have uncommitted changes:"
  printf '%s\n' "$DIRTY" | sed 's/^/      /'
  die "commit or stash them first — this script rewrites $CATALOG and would bury them"
fi
ok "no uncommitted work to lose"

# --- 2. Clear leftovers ------------------------------------------------------------------
step "Clearing leftovers inside the submodule"
LEFTOVERS=$(git -C "$SUB" status --porcelain --untracked-files=all 2>/dev/null || true)
if [ -z "$LEFTOVERS" ]; then
  ok "nothing to clear"
else
  # Shown before anything is deleted, because these are files in *your* checkout and this
  # cannot tell a script's scratch WAV from something you put there on purpose.
  info "several 'py' scripts write scratch files next to themselves; git sees them as changes"
  info "to the submodule and will not update it until they are gone:"
  printf '%s\n' "$LEFTOVERS" | sed 's/^/      /'
  if confirm "Delete these and continue?"; then
    # `-x` as well, since a script's output is exactly the kind of thing a global gitignore
    # tends to cover, and an ignored leftover blocks the update just the same.
    run git -C "$SUB" clean -fdx
    run git -C "$SUB" checkout -- .
    did "leftovers cleared"
  else
    die "cannot update the submodule while it has local changes"
  fi
fi

# --- 3. Update the submodule --------------------------------------------------------------
step "Updating praatAudioTools"
BEFORE_SHA=$(git -C "$SUB" rev-parse HEAD 2>/dev/null || echo unknown)
run git submodule update --remote --init "$SUB" \
  || die "could not update the submodule — check your network connection"
AFTER_SHA=$(git -C "$SUB" rev-parse HEAD 2>/dev/null || echo unknown)

if [ "$DRY_RUN" = 1 ]; then
  info "(dry run — the submodule was not moved, so there is nothing to compare)"
elif [ "$BEFORE_SHA" = "$AFTER_SHA" ]; then
  ok "already at the latest version (${AFTER_SHA:0:7})"
else
  COUNT=$(git -C "$SUB" rev-list --count "$BEFORE_SHA..$AFTER_SHA" 2>/dev/null || echo "?")
  ok "${BEFORE_SHA:0:7} -> ${AFTER_SHA:0:7} ($COUNT commit(s))"
  info ""
  info "scripts changed upstream:"
  # `--stat` rather than the log: every upstream commit message is "Add files via upload", so
  # the file list is the only description of what happened that exists.
  git -C "$SUB" diff --stat "$BEFORE_SHA..$AFTER_SHA" 2>/dev/null \
    | sed 's/^/      /' || true
fi

# --- 4. Regenerate the catalog ------------------------------------------------------------
step "Regenerating the process catalog"
BEFORE_LIST=$(catalog_processes)
BEFORE_COUNT=$(printf '%s' "$BEFORE_LIST" | grep -c . || true)

run python3 scripts/convert_praat_audiotools.py || die "the converter failed — see above"

if [ "$DRY_RUN" = 0 ]; then
  AFTER_LIST=$(catalog_processes)
  AFTER_COUNT=$(printf '%s' "$AFTER_LIST" | grep -c . || true)

  # Compared by *key*, then by script path, so the three cases stay distinct. Comparing whole
  # `key+path` lines instead reports a rename as one process vanishing and an unrelated one
  # arriving, which is the opposite of informative — and upstream renames constantly
  # (`Creative Formant Manipulations.praat` -> `Creative_Formant_Manipulations.praat` in a
  # single update, spaces to underscores).
  # Re-sorted after `cut`, under `LC_ALL=C`, and both are load-bearing.
  #
  # `catalog_processes` sorts whole `key<TAB>path` lines, and cutting field 1 from a
  # line-sorted list does **not** leave a key-sorted list: where one key is a prefix of another
  # (`..._sweeper` and `..._sweeper_2`), the tab separating key from path collates differently
  # from the `_` that follows the prefix, so the two lines order by their paths rather than
  # their keys. `comm` then silently reports nonsense — it printed "not in sorted order" and
  # listed `praat_filter_color_dynamic_formant_sweeper` as both added *and* removed on the
  # 2026-08-08 update, which is how this was noticed. `LC_ALL=C` because `comm` and `sort` must
  # agree on collation and a locale-aware `sort` does not order the way `comm` checks for.
  BEFORE_KEYS=$(printf '%s\n' "$BEFORE_LIST" | cut -f1 | LC_ALL=C sort)
  AFTER_KEYS=$(printf '%s\n' "$AFTER_LIST" | cut -f1 | LC_ALL=C sort)
  ADDED=$(LC_ALL=C comm -13 <(printf '%s\n' "$BEFORE_KEYS") <(printf '%s\n' "$AFTER_KEYS") || true)
  REMOVED=$(LC_ALL=C comm -23 <(printf '%s\n' "$BEFORE_KEYS") <(printf '%s\n' "$AFTER_KEYS") || true)
  RENAMED=$(LC_ALL=C join -t "$(printf '\t')" \
              <(printf '%s\n' "$BEFORE_LIST" | LC_ALL=C sort) \
              <(printf '%s\n' "$AFTER_LIST" | LC_ALL=C sort) 2>/dev/null \
            | awk -F'\t' '$2 != $3 { print $2" -> "$3 }' || true)

  info ""
  if [ -z "$ADDED" ] && [ -z "$REMOVED" ] && [ -z "$RENAMED" ]; then
    ok "$AFTER_COUNT processes, unchanged"
    info "(a script can still have been reworked without its catalog entry appearing or"
    info " disappearing — check the file list above)"
  else
    ok "$BEFORE_COUNT -> $AFTER_COUNT processes"
    if [ -n "$ADDED" ]; then
      info ""
      info "new:"
      printf '%s\n' "$ADDED" | sed "s/^/      ${GREEN}+${RESET} /"
    fi
    if [ -n "$REMOVED" ]; then
      info ""
      # Not necessarily a loss: the converter excludes a script it cannot drive headlessly, and
      # a rework upstream can push one over that line. docs/praat-excluded-scripts.md says which.
      info "gone (removed upstream, or newly un-runnable — see docs/praat-excluded-scripts.md):"
      printf '%s\n' "$REMOVED" | sed "s/^/      ${YELLOW}-${RESET} /"
    fi
    if [ -n "$RENAMED" ]; then
      info ""
      info "same process, renamed file:"
      printf '%s\n' "$RENAMED" | sed "s/^/      ${DIM}~${RESET} /"
    fi
  fi
fi

# --- 5. Rebuild ---------------------------------------------------------------------------
if [ "$WANT_BUILD" = 1 ]; then
  step "Rebuilding tui-wave"
  if ! command -v cargo >/dev/null 2>&1; then
    warn "cargo not found — skipping the rebuild"
    warn "the catalog is compiled into the binary, so nothing changes until you rebuild:"
    warn "  cargo install --path ."
  else
    # The catalog is `include_str!`d, so without this the update has no visible effect at all.
    info "the catalog is compiled in, so this is what actually makes the new scripts appear"
    info "(a few minutes the first time)"
    run cargo install --path . || die "the rebuild failed — the catalog may not match the code"
    did "rebuilt and installed"
  fi
else
  step "Skipping the rebuild (--no-build)"
  info "the catalog is compiled into the binary; nothing changes until you run:"
  info "  cargo install --path ."
fi

# --- 6. What now ---------------------------------------------------------------------------
step "Done"
if [ "$DRY_RUN" = 1 ]; then
  info "dry run — nothing was changed"
  exit 0
fi

CHANGED=$(git status --porcelain -- "$CATALOG" "$SUB" 2>/dev/null || true)
if [ -z "$CHANGED" ]; then
  ok "nothing changed; you were already up to date"
  exit 0
fi

# Both of these are *tracked*, so an ordinary user now has local modifications they did not
# write, and `git pull` will conflict on a 70,000-line generated file. Saying so here is much
# cheaper than working it out from the conflict.
info "two tracked files now differ from the last commit:"
printf '%s\n' "$CHANGED" | sed 's/^/      /'
info ""
info "keep them, and your next 'git pull' will conflict on the generated catalog."
info "to undo and go back to the released set of scripts:"
info ""
info "  git checkout -- $CATALOG && git submodule update --init $SUB"
info ""
info "to keep them permanently (maintainers — this is what ships them to everyone):"
info ""
info "  git add $CATALOG $SUB && git commit"
