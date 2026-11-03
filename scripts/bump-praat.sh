#!/usr/bin/env bash
# Bump third_party/praat-audiotools to upstream main and redo everything derived from it, in order:
#
#   1. move the submodule pin to upstream main
#   2. set PINNED_COMMIT in setup.sh (praat_setup_commit_matches_the_catalog checks it)
#   3. regenerate the catalog and the excluded-scripts report (convert_praat_audiotools.py)
#   4. regenerate the Vector Chain presets into docs/vector-chain-presets/
#      (the_vector_chain_presets_match_the_catalog checks them) and install them into
#      your chain folder, saying which files were added or overwritten
#   5. list the scripts upstream changed, which each need a real run through Praat
#
# It does not commit, run the tests, or judge hoists and new pause dialogs: those stay manual.
# Run from a clean working tree; nothing outside the repo is touched except the chain folder.
set -euo pipefail
cd "$(dirname "$0")/.."

SUB=third_party/praat-audiotools
PRESETS=docs/vector-chain-presets
CHAINS_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/tui-wave/cdp_chains"

if [ -n "$(git status --porcelain)" ]; then
    echo "error: the working tree is not clean; commit or stash first" >&2
    exit 1
fi

old=$(git -C "$SUB" rev-parse HEAD)
git submodule update --remote "$SUB"
new=$(git -C "$SUB" rev-parse HEAD)
if [ "$old" = "$new" ]; then
    echo "== praatAudioTools: already at ${new:0:7}, regenerating anyway"
else
    echo "== praatAudioTools: ${old:0:7} -> ${new:0:7}"
fi

sed -i.bak "s/^PINNED_COMMIT=.*/PINNED_COMMIT=$new/" setup.sh && rm setup.sh.bak

echo "== catalog"
python3 scripts/convert_praat_audiotools.py 2>&1 | grep -v -e SyntaxWarning -e 'Parsed with'

echo "== Vector Chain presets"
rm -f "$PRESETS"/*.toml
python3 scripts/explicate_vector_chains.py --chains "$PRESETS"
mkdir -p "$CHAINS_DIR"
for f in "$PRESETS"/*.toml; do
    target="$CHAINS_DIR/$(basename "$f")"
    if [ ! -f "$target" ]; then
        echo "  added        $(basename "$f")"
    elif ! cmp -s "$f" "$target"; then
        echo "  overwritten  $(basename "$f")"
    fi
    cp "$f" "$target"
done
for f in "$CHAINS_DIR"/AudioTools_*.toml; do
    [ -e "$f" ] || continue
    if [ ! -f "$PRESETS/$(basename "$f")" ]; then
        echo "  NOT REGENERATED (left in place, may be stale): $(basename "$f")"
    fi
done

if [ "$old" != "$new" ]; then
    echo "== scripts changed upstream (run each through Praat)"
    git -C "$SUB" diff --name-status "$old" "$new" -- '*.praat' '*.py' | sed 's/^/  /'
fi

if git -C third_party/airwin2rack fetch -q origin 2>/dev/null; then
    behind=$(git -C third_party/airwin2rack rev-list --count HEAD..origin/HEAD 2>/dev/null || echo 0)
    if [ "$behind" != 0 ]; then
        echo "== Airwindows: $behind new upstream commit(s); run ./update-airwindows.sh"
    fi
fi

echo
echo "Next: cargo test, run the changed processes through Praat, update CHANGELOG.md, commit."
