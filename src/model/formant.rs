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
    /// The Buffers-panel tag (`buffer_names`' `[Curve]` precedent).
    pub fn tag(self) -> &'static str {
        match self {
            FormantBufferKind::Formant => "[Formant]",
            FormantBufferKind::Snapshot => "[Snapshot]",
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

/// Duration (seconds) and per-window spectral-envelope point count, read from a formant
/// file's RIFF/WAVE `note` chunk — enough for the read-only info popup, without ever
/// needing to understand or expose the envelope values themselves. Mirrors
/// `model::cdp::pipeline::parse_ana_decfactor`'s own "find the literal `note` tag, read
/// `key\nhex\n` pairs" approach (proven against a real `.ana` file's own note chunk),
/// generalized to read *any* named key rather than only `decfactor`, and reading the `fmt`
/// chunk's sample-rate field for `arate` the same way
/// `model::curve::pitch_wav_grid_times`'s fmt-chunk parsing already does for pitch files
/// (confirmed identical convention between the two file types by hex-dumping a real
/// `formants get` output).
pub fn read_formant_duration_secs(bytes: &[u8]) -> Option<f64> {
    let arate = fmt_chunk_sample_rate(bytes)?;
    let data_len = data_chunk_len(bytes)?;
    let specenvcnt = find_note_key_u32(bytes, "specenvcnt")?;
    if specenvcnt == 0 || arate <= 0.0 {
        return None;
    }
    let windows = data_len / (specenvcnt as usize * 4);
    Some(windows as f64 / arate)
}

fn fmt_chunk_sample_rate(bytes: &[u8]) -> Option<f64> {
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
        if chunk_id == b"fmt " && chunk_size >= 8 {
            let sample_rate = u32::from_le_bytes(bytes[body_start + 4..body_start + 8].try_into().ok()?);
            return Some(sample_rate as f64);
        }
        pos = body_start + chunk_size + (chunk_size % 2);
    }
    None
}

fn data_chunk_len(bytes: &[u8]) -> Option<usize> {
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
        if chunk_id == b"data" {
            return Some(chunk_size);
        }
        pos = body_start + chunk_size + (chunk_size % 2);
    }
    None
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
    /// quirk reproduced, to prove the parser tolerates it) + `data` (`window_count *
    /// specenvcnt` floats).
    fn fake_formant_file(arate: u32, specenvcnt: u32, window_count: usize) -> Vec<u8> {
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

        let data_body = vec![0u8; window_count * specenvcnt as usize * 4];

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
    fn new_buffer_carries_its_kind_name_and_bytes() {
        let buffer = FormantBuffer::new(FormantBufferKind::Formant, "my_formants", vec![1, 2, 3, 4], "test source");
        assert_eq!(buffer.kind, FormantBufferKind::Formant);
        assert_eq!(buffer.name, "my_formants");
        assert_eq!(buffer.bytes, vec![1, 2, 3, 4]);
        assert_eq!(buffer.source_label, "test source");
    }

    #[test]
    fn kind_tag_distinguishes_formant_from_snapshot() {
        assert_eq!(FormantBufferKind::Formant.tag(), "[Formant]");
        assert_eq!(FormantBufferKind::Snapshot.tag(), "[Snapshot]");
    }
}
