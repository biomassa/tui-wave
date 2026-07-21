/// Which of the two formant-family document shapes a `FormantBuffer` holds — the two are
/// structurally identical (opaque CDP-produced bytes, nothing to hand-edit) but must never
/// be interchangeable: `formants put`'s required input is always a whole time-varying
/// `Formant` curve, `oneform put`'s is always a single frozen `Snapshot` instant
/// (`oneform get`'s own output). Buffers-panel tagging and the "pick a buffer" picker both
/// filter by this so a process is only ever offered the kind it actually needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FormantBufferKind {
    /// A whole time-varying formant envelope (`formants get`'s output).
    Formant,
    /// A single frozen instant of one (`oneform get`'s output).
    Snapshot,
}

impl FormantBufferKind {
    /// The Buffers-panel tag (`buffer_names`' `[p]` precedent).
    pub fn tag(self) -> &'static str {
        match self {
            FormantBufferKind::Formant => "[f]",
            FormantBufferKind::Snapshot => "[s]",
        }
    }
}

/// An opaque, CDP-produced binary buffer from the formant-data family (CDP-Ext-Plan.md
/// Phase 5) — either a whole `formants get`-shaped curve or a `oneform get`-shaped single
/// snapshot (`kind` distinguishes which). Deliberately much thinner than
/// `model::curve::PitchCurve`: there is no hand-editable representation at all. Confirmed
/// by reading CDP's own `formants` binary's *complete* subprogram list end to end
/// (`get`/`put`/`vocode`/`see`/`getsee`) that it offers no curve-to-curve transform family
/// the way `repitch` does for pitch curves — every real formant workflow is
/// extract-from-one-sound, apply-to-another, never hand-adjusting the raw per-window
/// spectral-envelope numbers. So `FormantBuffer` only ever needs to *carry* the bytes CDP
/// produced (for display: `source_label`, a plain human-readable note, never parsed back
/// out of `bytes`) and hand them to a CDP process that needs one, never expose them for
/// point-by-point editing the way `PitchCurve.points` does.
pub struct FormantBuffer {
    pub kind: FormantBufferKind,
    pub name: String,
    /// The exact bytes of the real CDP-produced file (`formants get`/`oneform get`'s raw
    /// output) — same RIFF/WAVE container convention as `model::curve::PitchCurve`'s
    /// `binary_template`, just never spliced or resampled since there's no editable points
    /// to bake back in.
    pub bytes: Vec<u8>,
    /// Human-readable note for the read-only info popup (`Dialog::FormantInfo`) — e.g. the
    /// source buffer's name and the extraction settings used. Purely descriptive.
    pub source_label: String,
    /// Where this buffer's timeline begins, in seconds, within the audio it was extracted
    /// from — i.e. the extraction selection's start (0.0 for a whole-file extraction). A
    /// `Formant` buffer's own time axis always starts at 0, so mapping a document cursor
    /// position back onto it ("Freeze Snapshot at Cursor", `App::freeze_snapshot_at_cursor`)
    /// needs this offset: `buffer_time = cursor_seconds − source_start_seconds`. Left 0.0 for
    /// a `Snapshot` (a single frozen instant with no meaningful timeline).
    pub source_start_seconds: f64,
    /// The Buffers-panel name of the document this `Formant` buffer was extracted from, so
    /// "Freeze Snapshot at Cursor" can tell whether the current document already has a formant
    /// extraction to reuse (vs. needing to auto-extract). Empty for a loaded/`Snapshot` buffer
    /// with no originating document.
    pub source_document_name: String,
}

impl FormantBuffer {
    /// Deliberately no `path`/`dirty`/save-load counterpart to `model::curve::PitchCurve`'s —
    /// unlike a pitch curve (a hand-editable artifact worth persisting across sessions), a
    /// formant buffer is purely an extraction result with no hand-editable representation at
    /// all (this module's own doc comments); re-extracting one costs one CDP run, not lost
    /// work, so there's nothing here worth building session persistence for until a real need
    /// shows up. `source_start_seconds` defaults to 0.0 — set it with `with_source_start` when
    /// the buffer was extracted from a mid-file selection.
    pub fn new(kind: FormantBufferKind, name: impl Into<String>, bytes: Vec<u8>, source_label: impl Into<String>) -> Self {
        FormantBuffer {
            kind,
            name: name.into(),
            bytes,
            source_label: source_label.into(),
            source_start_seconds: 0.0,
            source_document_name: String::new(),
        }
    }

    /// Records where this buffer's timeline begins within its source audio (the extraction
    /// selection's start, in seconds) — see `source_start_seconds`.
    pub fn with_source_start(mut self, seconds: f64) -> Self {
        self.source_start_seconds = seconds;
        self
    }

    /// Records the originating document's Buffers-panel name — see `source_document_name`.
    pub fn with_source_document(mut self, name: impl Into<String>) -> Self {
        self.source_document_name = name.into();
        self
    }
}

/// Duration (seconds), read from a formant file's RIFF/WAVE `note` chunk — enough for the
/// read-only info popup's "Duration" row without needing the full envelope. A thin wrapper
/// over `read_formant_envelope` (the two are computed from the same `windows`/`arate`) so
/// there's exactly one place that understands the byte layout.
pub fn read_formant_duration_secs(bytes: &[u8]) -> Option<f64> {
    let envelope = read_formant_envelope(bytes)?;
    Some(envelope.windows as f64 / envelope.arate)
}

/// A formant/snapshot buffer's raw per-window spectral envelope, decoded from its `data`
/// chunk for the read-only visualization in `Dialog::FormantInfo`
/// (`ui::app::render_formant_info_dialog`) — CDP's own layout is a number of consecutive
/// blocks of `specenvcnt` little-endian `f32` amplitudes each, one block per analysis window
/// in time order (confirmed by hex-dumping a real `formants get` output: `data_len /
/// (specenvcnt * 4)` windows, which `read_formant_duration_secs` already relied on before
/// this struct existed). `values[w * specenvcnt + b]` is window `w`'s amplitude at spectral
/// bin `b`, where `w`/`windows` already exclude the file's own leading header blocks — see
/// `read_formant_envelope`'s doc comment. A `Snapshot` buffer (`oneform get`'s output) is the
/// `windows == 1` case — the same shape, just a single time slice.
pub struct FormantEnvelope {
    pub windows: usize,
    pub specenvcnt: usize,
    /// Analysis window rate (windows per second) — same role as a pitch file's own sample
    /// rate, read from the `fmt` chunk.
    pub arate: f64,
    pub values: Vec<f32>,
}

impl FormantEnvelope {
    /// Amplitude at window `w`, bin `b` (row-major, see `values`' own doc comment).
    pub fn get(&self, w: usize, b: usize) -> f32 {
        self.values[w * self.specenvcnt + b]
    }

    /// Median-filters along the time axis only, independently per bin, with a window of
    /// `2 * radius + 1` consecutive windows (`radius` in frames) — the pure, directly
    /// testable building block behind `despiked_for_display`, which picks `radius` from a
    /// fixed time duration rather than a fixed frame count; see that method's doc comment
    /// for why.
    pub fn temporal_median_filtered(&self, radius: usize) -> FormantEnvelope {
        if self.windows <= 1 {
            return FormantEnvelope { windows: self.windows, specenvcnt: self.specenvcnt, arate: self.arate, values: self.values.clone() };
        }
        let mut values = vec![0.0f32; self.values.len()];
        let mut window_buf: Vec<f32> = Vec::with_capacity(2 * radius + 1);
        for bin in 0..self.specenvcnt {
            for w in 0..self.windows {
                window_buf.clear();
                let lo = w.saturating_sub(radius);
                let hi = (w + radius).min(self.windows - 1);
                for nw in lo..=hi {
                    window_buf.push(self.get(nw, bin));
                }
                window_buf.sort_by(f32::total_cmp);
                values[w * self.specenvcnt + bin] = window_buf[window_buf.len() / 2];
            }
        }
        FormantEnvelope { windows: self.windows, specenvcnt: self.specenvcnt, arate: self.arate, values }
    }

    /// Smooths the envelope for display in `ui::app::render_formant_info_dialog`/
    /// `ui::widgets::formant_image` (never stored back into the buffer) via
    /// `temporal_median_filtered`, with `radius` chosen as a fixed *time* window rather than
    /// a fixed frame count — a given frame count spans very different real time at `arate`
    /// 344Hz vs. 750Hz (both seen across real `formants get` runs in this app), so a
    /// frame-count constant would under-smooth some files and over-smooth others. Regression
    /// fix (user report, 2026-07-21: "still full of black squares" — a heatmap from real
    /// voice content, after the leading-header-row fix already removed CDP's own reference
    /// tables, showing regular vertical dark bands rather than random speckle). CDP's formant
    /// tracker (`formants get`) momentarily loses lock on individual analysis frames fairly
    /// often on real speech — a normal, expected behavior of per-frame pitch/formant
    /// tracking, not an error — dropping that frame's reported amplitude, which reads as
    /// visible noise once rendered (my first synthetic test tone never exercised this: a
    /// clean, stable sine gives the tracker nothing to lose lock on). A real formant contour
    /// moves continuously frame to frame (tracking a vocal tract shape that can't teleport),
    /// so an isolated dropped frame is exactly the noise a short temporal median absorbs
    /// without blurring genuine, much slower formant movement (tens of milliseconds per
    /// transition). `HALF_WINDOW_MS = 15.0` was picked empirically against a real user
    /// recording (`arate` 750Hz, ~6.7% of cells flagged as isolated per-bin dropouts): a
    /// 15ms half-window (~11-frame radius there) cut that to well under 1%, and diminishing
    /// returns set in well before that point on the same data.
    pub fn despiked_for_display(&self) -> FormantEnvelope {
        const HALF_WINDOW_MS: f64 = 15.0;
        let radius = ((HALF_WINDOW_MS / 1000.0) * self.arate).round().max(1.0) as usize;
        self.temporal_median_filtered(radius)
    }
}

/// A row that (a) never decreases from one bin to the next and (b) ends higher than it
/// started is one of CDP's own embedded reference tables, not real per-time amplitude data —
/// see `read_formant_envelope`'s doc comment for how this was found and confirmed across
/// three different analysis configs (`-p8`, `-p4`, `-f8`). One row is unmistakably a
/// per-pitch-band center-frequency lookup: strictly increasing except for a tie in its very
/// last one or two bins, where the highest pitch-band(s) clamp to exactly the source audio's
/// own Nyquist frequency (hence "never decreases," not "strictly increases" — a plain strict
/// check rejects this row over that one clamped tie). A second row of unknown purpose
/// precedes it, ending only around 4 (nowhere near Nyquist, why this can't key off magnitude
/// alone) but strictly increasing throughout. Genuine spectral envelope data fluctuates
/// window to window — silence is *flat* (fails "ends higher than it started", the guard that
/// keeps an all-zero silent window from matching just because a flat row is trivially
/// "never decreasing") and any real signal decreases somewhere among dozens of bins — so
/// this reliably tells the two apart without hardcoding how many header rows a given CDP
/// version/mode emits or what units its magnitudes are in.
fn looks_like_header_row(row: &[f32]) -> bool {
    row.len() >= 2 && row.windows(2).all(|pair| pair[1] >= pair[0]) && *row.last().unwrap() > row[0]
}

/// Parses a formant/snapshot buffer's `data` chunk into a `FormantEnvelope` — see that
/// struct's doc comment for the byte layout. `None` for anything that doesn't parse as a
/// well-formed formant file, or whose `data` chunk length isn't an exact multiple of
/// `specenvcnt` floats (a truncated/corrupt file), mirroring `read_formant_duration_secs`'s
/// own existing error handling.
///
/// Skips a leading run of header rows (`looks_like_header_row`) before treating the rest as
/// real per-time data. Regression fix (user report, 2026-07-21: "thin yellow lines"/"black
/// squares" in the heatmap, and separately "why do we have two steps? it's a snapshot"):
/// hex-inspecting a real `formants get -p8` output found its first *two* "windows" are
/// always a fixed pair of reference rows — one ending around 4 (unexplained, possibly a
/// bandwidth/confidence table) and one that's unmistakably a per-pitch-band center-frequency
/// table (monotonically increasing in ~2^(1/8) steps, matching `-p8`'s 8-bands-per-octave
/// spacing, capping out at exactly the source audio's own Nyquist frequency for the highest
/// bands) — before any genuine per-time amplitude data begins. Confirmed identical in a real
/// `oneform get` snapshot file too (`windows` there parsed to 3, not the expected 1, with the
/// *actual* frozen instant sitting at index 2 — this is what the earlier "two steps" fix
/// misdiagnosed as CDP writing the instant twice). Trimming here means every consumer of
/// `FormantEnvelope` — the heatmap, the snapshot curve, `read_formant_duration_secs` — always
/// sees only genuine data with no special-casing of its own.
pub fn read_formant_envelope(bytes: &[u8]) -> Option<FormantEnvelope> {
    let arate = fmt_chunk_sample_rate(bytes)?;
    let data = find_chunk(bytes, b"data")?;
    let specenvcnt = find_note_key_u32(bytes, "specenvcnt")? as usize;
    if specenvcnt == 0 || arate <= 0.0 {
        return None;
    }
    let bytes_per_window = specenvcnt * 4;
    if data.len() % bytes_per_window != 0 {
        return None;
    }
    let raw_windows = data.len() / bytes_per_window;
    if raw_windows == 0 {
        return None;
    }
    let values: Vec<f32> = data.chunks_exact(4).map(|w| f32::from_le_bytes(w.try_into().unwrap())).collect();

    let mut header_rows = 0;
    while header_rows < raw_windows.saturating_sub(1) && looks_like_header_row(&values[header_rows * specenvcnt..(header_rows + 1) * specenvcnt]) {
        header_rows += 1;
    }

    let windows = raw_windows - header_rows;
    let values = values[header_rows * specenvcnt..].to_vec();
    Some(FormantEnvelope { windows, specenvcnt, arate, values })
}

/// Walks the top-level RIFF/WAVE chunk list looking for `id`, returning its body slice.
/// Shared by `fmt_chunk_sample_rate` (`fmt `) and `read_formant_envelope` (`data`) — the
/// same walk, just a different target chunk ID.
fn find_chunk<'a>(bytes: &'a [u8], id: &[u8; 4]) -> Option<&'a [u8]> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let chunk_id = &bytes[pos..pos + 4];
        let chunk_size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().ok()?) as usize;
        let body_start = pos + 8;
        if body_start + chunk_size > bytes.len() {
            break;
        }
        if chunk_id == id {
            return Some(&bytes[body_start..body_start + chunk_size]);
        }
        pos = body_start + chunk_size + (chunk_size % 2);
    }
    None
}

fn fmt_chunk_sample_rate(bytes: &[u8]) -> Option<f64> {
    let body = find_chunk(bytes, b"fmt ")?;
    if body.len() < 8 {
        return None;
    }
    Some(u32::from_le_bytes(body[4..8].try_into().ok()?) as f64)
}

/// Finds the literal `note` tag anywhere in `bytes` (same pragmatic byte-search
/// `parse_ana_decfactor` already uses rather than walking the full RIFF/LIST hierarchy —
/// works the same regardless of whatever wrapper chunk the note happens to sit inside) and
/// reads its `key\nhex-u32\n` pairs looking for `key_name`. The real file's first key has
/// CDP's own 4-byte `"sfif"` marker glued directly onto it with no separator (confirmed by
/// hex dump — e.g. `"sfifis a formant file"` as one literal line) — harmless here since
/// this only ever searches for keys *other* than the first one.
fn find_note_key_u32(bytes: &[u8], key_name: &str) -> Option<u32> {
    let idx = bytes.windows(4).position(|w| w == b"note")?;
    let body_start = idx + 4;
    let size = u32::from_le_bytes(bytes.get(body_start..body_start + 4)?.try_into().ok()?) as usize;
    let body = bytes.get(body_start + 4..body_start + 4 + size)?;
    let text = std::str::from_utf8(body).ok()?;
    let mut lines = text.split('\n');
    while let Some(key) = lines.next() {
        let Some(value_hex) = lines.next() else { break };
        if key.trim() == key_name {
            let hex = value_hex.trim();
            if hex.len() != 8 {
                return None;
            }
            let mut arr = [0u8; 4];
            for i in 0..4 {
                arr[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
            }
            return Some(u32::from_le_bytes(arr));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal but structurally real RIFF/WAVE formant file: `fmt ` (arate as the
    /// sample-rate field) + a `note`-tagged chunk carrying `specenvcnt` in CDP's own
    /// `key\nhex-u32\n` convention (with the real file's `"sfif"`-glued-onto-the-first-key
    /// quirk reproduced, to prove the parser tolerates it) + `data` (`values`, `windows *
    /// specenvcnt` floats in row-major window order).
    fn fake_formant_file_with_values(arate: u32, specenvcnt: u32, values: &[f32]) -> Vec<u8> {
        let mut fmt_body = Vec::new();
        fmt_body.extend_from_slice(&3u16.to_le_bytes());
        fmt_body.extend_from_slice(&1u16.to_le_bytes());
        fmt_body.extend_from_slice(&arate.to_le_bytes());
        fmt_body.extend_from_slice(&(arate * 4).to_le_bytes());
        fmt_body.extend_from_slice(&4u16.to_le_bytes());
        fmt_body.extend_from_slice(&32u16.to_le_bytes());

        // CDP's own LE-u32-as-hex convention, matching `find_note_key_u32`'s decoder.
        let hex_le = |v: u32| v.to_le_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>();
        let note_text = format!("sfifis a formant file\n01000000\nspecenvcnt\n{}\n", hex_le(specenvcnt));
        let note_body = note_text.into_bytes();

        let mut data_body = Vec::with_capacity(values.len() * 4);
        for v in values {
            data_body.extend_from_slice(&v.to_le_bytes());
        }

        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&(fmt_body.len() as u32).to_le_bytes());
        out.extend_from_slice(&fmt_body);
        out.extend_from_slice(b"note");
        out.extend_from_slice(&(note_body.len() as u32).to_le_bytes());
        out.extend_from_slice(&note_body);
        if note_body.len() % 2 == 1 {
            out.push(0); // RIFF chunks are word-aligned -- real CDP files pad odd sizes
        }
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data_body.len() as u32).to_le_bytes());
        out.extend_from_slice(&data_body);
        out
    }

    fn fake_formant_file(arate: u32, specenvcnt: u32, window_count: usize) -> Vec<u8> {
        fake_formant_file_with_values(arate, specenvcnt, &vec![0.0f32; window_count * specenvcnt as usize])
    }

    #[test]
    fn read_formant_duration_secs_computes_windows_over_arate() {
        let bytes = fake_formant_file(344, 66, 353);
        let duration = read_formant_duration_secs(&bytes).unwrap();
        assert!((duration - 353.0 / 344.0).abs() < 1e-9);
    }

    #[test]
    fn read_formant_duration_secs_returns_none_for_non_riff_bytes() {
        assert_eq!(read_formant_duration_secs(b"not a riff file"), None);
    }

    #[test]
    fn read_formant_envelope_decodes_windows_and_bins_in_row_major_order() {
        // 2 windows x 3 bins: window 0 = [1,3,2], window 1 = [4,6,5] -- deliberately not
        // monotonic within a window (unlike a real sequential 1,2,3 fixture would be), so
        // this doesn't accidentally look like `looks_like_header_row`'s reference-table shape.
        let values = [1.0f32, 3.0, 2.0, 4.0, 6.0, 5.0];
        let bytes = fake_formant_file_with_values(100, 3, &values);
        let envelope = read_formant_envelope(&bytes).unwrap();
        assert_eq!(envelope.windows, 2);
        assert_eq!(envelope.specenvcnt, 3);
        assert_eq!(envelope.arate, 100.0);
        assert_eq!(envelope.get(0, 0), 1.0);
        assert_eq!(envelope.get(0, 2), 2.0);
        assert_eq!(envelope.get(1, 0), 4.0);
        assert_eq!(envelope.get(1, 2), 5.0);
    }

    #[test]
    fn read_formant_envelope_handles_a_single_window_snapshot() {
        let bytes = fake_formant_file_with_values(200, 4, &[10.0, 20.0, 30.0, 40.0]);
        let envelope = read_formant_envelope(&bytes).unwrap();
        assert_eq!(envelope.windows, 1);
        assert_eq!(envelope.specenvcnt, 4);
    }

    #[test]
    fn read_formant_envelope_returns_none_for_non_riff_bytes() {
        assert_eq!(read_formant_envelope(b"not a riff file").map(|_| ()), None);
    }

    /// Regression (user report, 2026-07-21: "thin yellow lines"/"black squares", and
    /// separately "why do we have two steps? it's a snapshot"): a leading row that
    /// monotonically increases up into the tens of thousands (CDP's own embedded
    /// per-pitch-band center-frequency table, confirmed against real `formants get`/
    /// `oneform get` output — see `read_formant_envelope`'s doc comment) must be trimmed off,
    /// not exposed as if it were real per-time data.
    #[test]
    fn read_formant_envelope_trims_a_leading_header_row() {
        let header = [40.0f32, 80.0, 160.0, 22050.0]; // monotonic, ends far above 1000
        let real1 = [0.01f32, 0.5, 0.2, 0.05];
        let real2 = [0.02f32, 0.3, 0.4, 0.01];
        let mut values = Vec::new();
        values.extend_from_slice(&header);
        values.extend_from_slice(&real1);
        values.extend_from_slice(&real2);
        let bytes = fake_formant_file_with_values(100, 4, &values);
        let envelope = read_formant_envelope(&bytes).unwrap();
        assert_eq!(envelope.windows, 2, "the header row must not be counted as a real window");
        assert_eq!(envelope.get(0, 0), 0.01);
        assert_eq!(envelope.get(1, 0), 0.02);
    }

    /// Two leading header rows (the shape actually observed in a real `formants get -p8`
    /// file: one row ending ~4, one row ending exactly at Nyquist) both get trimmed, leaving
    /// only the genuine per-time data.
    #[test]
    fn read_formant_envelope_trims_two_leading_header_rows() {
        let header1 = [0.0f32, 1.8, 2.1, 2.4];
        let header2 = [43.0f32, 86.0, 172.0, 22050.0];
        let real = [0.01f32, 0.5, 0.2, 0.05];
        let mut values = Vec::new();
        values.extend_from_slice(&header1);
        values.extend_from_slice(&header2);
        values.extend_from_slice(&real);
        let bytes = fake_formant_file_with_values(100, 4, &values);
        let envelope = read_formant_envelope(&bytes).unwrap();
        assert_eq!(envelope.windows, 1);
        assert_eq!(envelope.get(0, 0), 0.01);
    }

    /// A file with no header rows at all (every window's own values fluctuate, none
    /// monotonically increasing to Nyquist) must be left completely untouched.
    #[test]
    fn read_formant_envelope_leaves_a_file_with_no_header_rows_alone() {
        let values = [0.5f32, 0.1, 0.9, 0.2, 0.3, 0.7, 0.4, 0.6];
        let bytes = fake_formant_file_with_values(100, 4, &values);
        let envelope = read_formant_envelope(&bytes).unwrap();
        assert_eq!(envelope.windows, 2);
        assert_eq!(envelope.get(0, 0), 0.5);
    }

    /// Even if every window in a pathological file "looks like" a header row, at least one
    /// window must survive so the buffer doesn't parse to nothing.
    #[test]
    fn read_formant_envelope_never_trims_every_window() {
        let values = [10.0f32, 20.0, 30.0, 22050.0, 11.0, 21.0, 31.0, 22050.0];
        let bytes = fake_formant_file_with_values(100, 4, &values);
        let envelope = read_formant_envelope(&bytes).unwrap();
        assert_eq!(envelope.windows, 1, "at least one window must always remain");
    }

    /// Regression (user report, 2026-07-21: "still full of black squares"): a single window
    /// where the tracker dropped out (near-zero across every bin) surrounded by otherwise
    /// steady real data must be absorbed by the temporal median, not rendered as a solid dark
    /// column.
    #[test]
    fn temporal_median_filtered_absorbs_an_isolated_dropout_frame() {
        // 5 windows x 2 bins: windows 0,1,3,4 hold steady data; window 2 is a dropout (both
        // bins near zero).
        let values = vec![
            0.5f32, 0.6, // window 0
            0.5, 0.6, // window 1
            0.0, 0.0, // window 2 (dropout)
            0.5, 0.6, // window 3
            0.5, 0.6, // window 4
        ];
        let env = FormantEnvelope { windows: 5, specenvcnt: 2, arate: 344.0, values };
        let cleaned = env.temporal_median_filtered(3);
        assert_eq!(cleaned.get(2, 0), 0.5, "the dropout frame's bin 0 should be replaced by its neighbors' value");
        assert_eq!(cleaned.get(2, 1), 0.6, "the dropout frame's bin 1 should be replaced by its neighbors' value");
        assert_eq!(cleaned.get(0, 0), 0.5, "an untouched frame should stay the same");
    }

    #[test]
    fn temporal_median_filtered_preserves_shape_and_handles_a_single_window() {
        let env = FormantEnvelope { windows: 1, specenvcnt: 3, arate: 100.0, values: vec![0.1, 0.2, 0.3] };
        let cleaned = env.temporal_median_filtered(3);
        assert_eq!(cleaned.windows, 1);
        assert_eq!(cleaned.specenvcnt, 3);
        assert_eq!(cleaned.values, vec![0.1, 0.2, 0.3]);
    }

    /// `despiked_for_display`'s radius must scale with `arate`, not be a fixed frame count --
    /// a real 750Hz-`arate` file needs roughly double the frame radius a 344Hz-`arate` file
    /// does to cover the same 15ms. Checked by placing a 3-frame-wide dropout (still within a
    /// single 15ms half-window at either rate) and confirming both get fully absorbed.
    #[test]
    fn despiked_for_display_scales_its_radius_with_arate() {
        for arate in [344.0f64, 750.0] {
            let windows = 41;
            let specenvcnt = 1;
            let mut values = vec![0.5f32; windows];
            // A 3-frame dropout run in the middle.
            values[19] = 0.0;
            values[20] = 0.0;
            values[21] = 0.0;
            let env = FormantEnvelope { windows, specenvcnt, arate, values };
            let cleaned = env.despiked_for_display();
            assert_eq!(cleaned.get(20, 0), 0.5, "arate {arate}: the dropout run should be fully absorbed");
        }
    }

    #[test]
    fn new_buffer_carries_its_kind_name_and_bytes() {
        let buffer = FormantBuffer::new(FormantBufferKind::Formant, "my_formants", vec![1, 2, 3, 4], "test source");
        assert_eq!(buffer.kind, FormantBufferKind::Formant);
        assert_eq!(buffer.name, "my_formants");
        assert_eq!(buffer.bytes, vec![1, 2, 3, 4]);
        assert_eq!(buffer.source_label, "test source");
    }

    #[test]
    fn kind_tag_distinguishes_formant_from_snapshot() {
        assert_eq!(FormantBufferKind::Formant.tag(), "[f]");
        assert_eq!(FormantBufferKind::Snapshot.tag(), "[s]");
    }
}
