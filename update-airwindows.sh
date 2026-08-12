#!/usr/bin/env bash
#
# Bumps the airwin2rack submodule and regenerates the built-in Airwindows catalog.
#
# **Do the two together, always.** `airwindows_catalog.toml` is `include_str!`'d into the
# binary and its entries are matched to plugins by *name* and to controls by *parameter
# index* -- so a submodule bump alone leaves a catalog describing the previous checkout, and
# an upstream plugin that gained, lost or reordered a parameter would quietly point a labelled
# field at a different control. That produces plausible, wrong audio rather than an error,
# which is the same trap `update-praat-scripts.sh` exists to close on the Praat side.
#
# The generator is a Rust binary rather than a script over the sources, because the plugins
# are compiled into it: parameter names come from each plugin's own `getParameterName` and
# defaults from reading `getParameter` back after construction. Nothing is parsed and nothing
# can drift.

set -euo pipefail

cd "$(dirname "$0")"

CATALOG="src/model/cdp/airwindows_catalog.toml"

echo "==> Updating third_party/airwin2rack"
# Not `--recursive`: airwin2rack declares submodules of its own (`libs/airwindows`, the whole
# upstream Airwindows history, and `libs/sst-rackhelpers`) that nothing here reads. build.rs
# compiles the *committed* src/autogen_airwin/ tree, which import.pl already generated from
# that upstream.
git submodule update --init --remote third_party/airwin2rack

SHA="$(git -C third_party/airwin2rack rev-parse HEAD)"
echo "    at ${SHA}"

# The generator links the plugins, so this build is also the check that the new checkout still
# compiles. Release, because a debug build of ~1000 C++ translation units is slow enough to be
# worth avoiding for something run this rarely.
echo "==> Building the catalog generator (compiles the Airwindows sources; takes a few minutes)"
cargo build --release --bin dump-airwindows-catalog

echo "==> Regenerating ${CATALOG}"
# Written via a temp file so a generator that fails partway cannot leave a truncated catalog
# behind -- the same reasoning as `model::atomic::write_atomically` on the save paths.
cargo run --release --quiet --bin dump-airwindows-catalog > "${CATALOG}.new"
mv "${CATALOG}.new" "${CATALOG}"

echo "==> Rebuilding and testing against the new catalog"
cargo test --release

cat <<EOF

Done. ${CATALOG} regenerated at ${SHA}.

Review the diff before committing -- an upstream bump can add plugins, and can also rename or
reorder a plugin's parameters, which is exactly the change that is invisible at run time:

    git diff --stat ${CATALOG}
    git diff ${CATALOG} | head -100

Commit the submodule pointer and the catalog together.
EOF
