# Third-Party Notices

## LAME (MP3 encoding) — LGPL-2.1+

File ▸ Export's MP3 output uses [LAME](https://lame.sourceforge.io/) through the
[`mp3lame-encoder`](https://crates.io/crates/mp3lame-encoder) crate, whose `mp3lame-sys`
dependency **builds libmp3lame from source and links it statically into the binary**. LAME is
released under the [GNU Lesser General Public License, version 2.1 or later
(LGPL-2.1+)](https://github.com/lameproject/lame/blob/master/COPYING).

Because it is statically linked, anyone distributing a compiled binary of this project must
also satisfy the LGPL's relinking requirement — in practice, by making available the object
files or source needed to relink against a modified LAME, alongside this notice and a copy of
the LGPL. This is the only non-permissive component in the build; everything else is MIT or
Apache-2.0. Building from source (the normal case for this project) is unaffected.

MP3 is also patent-unencumbered as of 2017, when the last relevant patents expired.

## FLAC encoding and decoding

FLAC output uses [`flacenc`](https://crates.io/crates/flacenc) (Apache-2.0), a pure-Rust FLAC
encoder. FLAC, AIFF and WAV *decoding* uses
[Symphonia](https://github.com/pdeljanov/Symphonia) (MPL-2.0). Neither is statically linked C
code and neither imposes obligations beyond attribution.

The FLAC format itself is developed by the Xiph.Org Foundation and is royalty-free.

## SoundThread process catalog data

`src/model/cdp/catalog.toml` (the built-in CDP process definitions — parameter names,
ranges, defaults, and descriptions) is derived from `process_help.json` in
[SoundThread](https://github.com/j-p-higgins/SoundThread) by Jonathan Higgins, via
`scripts/convert_soundthread_catalog.py`. SoundThread is provided under the following
license:

```
MIT License

Copyright (c) 2025 Jonathan Higgins

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## CDP (Composer's Desktop Project)

This project's CDP integration invokes external CDP command-line binaries (not bundled or
distributed with this repository) that the user installs or builds separately and points the
app at via configuration (`Ctrl+P`, or **Options → Configure CDP Directory…**).

CDP is developed and maintained by the Composer's Desktop Project — founded in 1986 by Andrew
Bentley, Archer Endrich, Richard Orton, and Trevor Wishart — and has been free software since
2014, released under the [GNU Lesser General Public License, version 2.1 or later
(LGPL-2.1+)](https://github.com/ComposersDesktop/CDP8/blob/main/LICENSE). Homepage:
https://www.composersdesktop.com. Source: https://github.com/ComposersDesktop/CDP8 (current)
and https://github.com/ComposersDesktop/CDP7 (previous release, also compatible). Prebuilt
Windows/macOS downloads: https://www.unstablesound.net/cdp.html.

## Praat

This project's Praat integration invokes an external `praat` executable (not bundled or
distributed with this repository) that the user installs separately — it is packaged for Arch
(`extra/praat`), Debian/Ubuntu (`praat`) and Homebrew (`brew install --cask praat`), and is
also available as a standalone download. The app finds it on `PATH` by default; a
`praat_bin` config setting overrides that (on macOS the executable lives inside the bundle, at
`/Applications/Praat.app/Contents/MacOS/Praat`).

Praat is by Paul Boersma and David Weenink of the University of Amsterdam, and is released
under the [GNU General Public License, version 3 or later
(GPL-3.0+)](https://github.com/praat/praat/blob/master/README.md#21-license). Homepage:
https://www.praat.org. Source: https://github.com/praat/praat.

**No Praat code is linked into or distributed with this project.** The integration shells out
to the separate executable exactly as the CDP integration does, which is not a derivative work
under the GPL.

## praatAudioTools process scripts

`third_party/praat-audiotools` is a **git submodule**, not vendored content: this repository
records a commit reference, and `git submodule update --init` fetches the scripts from
upstream. `src/model/cdp/praat_catalog.toml` (the built-in Praat process definitions —
parameter names, ranges, defaults) is derived from those scripts by
`scripts/convert_praat_audiotools.py`, and its header records the exact commit it was
generated against.

praatAudioTools is by Shai Cohen (Department of Music, Bar-Ilan University, Israel). The
repository states the MIT License in its README, and essentially every script carries a
`# License: MIT License` header; there is no top-level `LICENSE` file at the pinned commit.
Source: https://github.com/ShaiCohen-ops/Praat-plugin_AudioTools.

The scripts are executed as-is, by absolute path — they are neither modified nor installed into
the user's Praat setup, and tui-wave never writes to the Praat preferences folder. The catalog
additionally carries, per process, the parameter values each script defines for its own presets,
extracted from the same sources by the converter above.

**The Windows release archive redistributes these scripts**, which the source repository and
every other release artifact do not: the macOS and Linux packages carry only the binary and
leave `setup-environment.sh` to fetch the scripts from upstream, but that script is bash and
Windows has no bash, so the `.zip` bundles the checkout beside the executable. The MIT licence
below permits this, and it is reproduced in the archive by shipping this file alongside the
binary. Two directories of the checkout are omitted as irrelevant to this program: `.git` (a
submodule gitlink meaningful only inside this repository) and `Max-MSP` (Max/MSP patches, which
tui-wave never reads). Nothing is modified — the scripts are the ones at the pinned commit the
bundled catalog was generated against.

```
MIT License

Copyright (c) Shai Cohen

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
