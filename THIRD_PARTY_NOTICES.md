# Third-Party Notices

## Airwindows (effect processing) — MIT, statically linked and redistributed

The Airwindows processes in ExtProcess are **compiled into the tui-wave binary** and are
therefore redistributed in every release artifact. This makes them unlike CDP, Praat and the
praatAudioTools scripts below, none of which is bundled — and unlike LAME, the only other
statically linked component, Airwindows is permissively licensed and imposes no relinking
obligation. Attribution (this notice, retaining the copyright and permission text) is the
whole of it.

[Airwindows](https://www.airwindows.com) is by **Chris Johnson**, released under the MIT
license — stated in the repository's `LICENSE` and repeated in the header comment of
essentially every source file ("Airwindows uses the MIT license"). Source:
https://github.com/airwindows/airwindows.

The sources actually compiled come from **airwin2rack** ("Airwindows Consolidated") by
**Paul Walker**, also MIT, vendored as the `third_party/airwin2rack` submodule and built by
`build.rs`. That project's `scripts/import.pl` is what makes the DSP usable outside a VST
host: upstream Airwindows includes the Steinberg VST2 SDK header `audioeffectx.h`, which is
discontinued and not redistributable, and import.pl replaces it with airwin2rack's own
~90-line `airwin_consolidated_base.h` shim, namespaces each plugin, and commits the result to
`src/autogen_airwin/`. **No Steinberg SDK code is used, required, or distributed**, and
nothing is downloaded at build time.

Only airwin2rack's plugin sources, its shim header, and its `ModuleAdd.h` registry are
compiled. Its DAW-plugin targets — which pull in JUCE and the VST3 SDK, and carry GPL
obligations as a result — are **not** built and not present. Source:
https://github.com/baconpaul/airwin2rack.

`src/model/cdp/airwindows_catalog.toml` is generated from those compiled plugins by
`src/bin/dump-airwindows-catalog.rs`, and its header records the exact airwin2rack commit it
was generated against.

```
MIT License

Copyright (c) Chris Johnson (Airwindows)
Copyright (c) 2019-2026 Paul Walker (airwin2rack / Airwindows Consolidated)

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
macOS downloads: https://www.unstablesound.net/cdp.html.

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

**No release artifact redistributes these scripts.** Every package carries the binary alone and
leaves `setup-environment.sh` to fetch the scripts from upstream, at the pinned commit the
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
