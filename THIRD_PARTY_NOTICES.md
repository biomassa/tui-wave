# Third-Party Notices

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
