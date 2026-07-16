use std::fs;
use std::path::{Path, PathBuf};

/// An open pitch curve: named time/Hz breakpoint pairs, held separately from any audio
/// `Document`. Produced by extracting pitch from a selection (`repitch getpitch` mode 2)
/// or generated/transformed by the `repitch` family's curve-in/curve-out subprograms
/// (`exag`, `invert`, `quantise`, ...). Deliberately much thinner than `Document` — a
/// curve has no channels, markers, or bext, since it holds no audio at all; the model
/// layer's "no ratatui/cpal/crossterm" rule (see CLAUDE.md) applies here just the same.
pub struct PitchCurve {
    pub name: String,
    /// (time_seconds, hz) pairs, kept sorted by time — the same shape CDP's own text
    /// breakpoint format (and this app's existing `required_envelope` datafile writer)
    /// already uses, so a curve round-trips through CDP with no conversion.
    pub points: Vec<(f64, f64)>,
    pub path: Option<PathBuf>,
    pub dirty: bool,
    /// The exact bytes of the last real CDP binary pitchfile this curve is descended from
    /// (`repitch getpitch` mode 1's raw output, or a prior transform's output) — `None` for
    /// a curve created by hand or loaded from a plain-text save file, which has no such
    /// lineage.
    ///
    /// CDP's curve-in/curve-out `repitch` family (`invert`, `smooth`, `quantise`, ...)
    /// rejects plain text outright — confirmed against the real binary: even CDP's own
    /// `pchtotext` round-trip output is refused with "Application doesn't work with this
    /// type of infile." Only this binary WAV-container format works. Rather than trying to
    /// synthesize one from nothing (CDP's own `repitch generate` was tried as a text→binary
    /// bridge and produced two unexplained anomalies — a silently `.wav`-suffixed filename
    /// and a wildly oversized result — before this template approach was found), this app
    /// never constructs a pitch-WAV's *header* itself: it always starts from a real one CDP
    /// produced, and only ever overwrites its `data` chunk's float values (verified: CDP
    /// accepts a template with every value replaced, so nothing else needs to match the
    /// original recording). See `splice_pitch_wav_data`/`resample_to_grid` for how a
    /// hand-edited curve gets baked back into this template before being fed to a
    /// transform, and `pitch_wav_grid_times` for reading the template's own time grid
    /// (`fmt` chunk's sample rate field *is* the analysis rate, one "sample" per analysis
    /// window — not an audio sample rate at all, despite living in the same field).
    pub binary_template: Option<Vec<u8>>,
}

impl PitchCurve {
    pub fn new(name: impl Into<String>, points: Vec<(f64, f64)>) -> Self {
        PitchCurve { name: name.into(), points, path: None, dirty: true, binary_template: None }
    }

    pub fn with_binary_template(mut self, template: Vec<u8>) -> Self {
        self.binary_template = Some(template);
        self
    }
}

/// Parses CDP's plain-text breakpoint format: one `time value` pair per line, whitespace
/// separated, blank lines ignored. The same shape `pipeline.rs` already writes for
/// `Breakpoints` params and `repitch getpitch` mode 2 (`bfil`) produces directly.
pub fn parse_breakpoints(text: &str) -> color_eyre::Result<Vec<(f64, f64)>> {
    let mut points = Vec::new();
    for (line_no, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split_whitespace();
        let (Some(t), Some(v)) = (fields.next(), fields.next()) else {
            color_eyre::eyre::bail!("breakpoint line {}: expected \"time value\", got {:?}", line_no + 1, line);
        };
        let t: f64 = t.parse().map_err(|_| {
            color_eyre::eyre::eyre!("breakpoint line {}: invalid time {:?}", line_no + 1, t)
        })?;
        let v: f64 = v.parse().map_err(|_| {
            color_eyre::eyre::eyre!("breakpoint line {}: invalid value {:?}", line_no + 1, v)
        })?;
        points.push((t, v));
    }
    Ok(points)
}

pub fn format_breakpoints(points: &[(f64, f64)]) -> String {
    points.iter().map(|(t, v)| format!("{t} {v}")).collect::<Vec<_>>().join("\n")
}

pub fn load_curve(path: impl AsRef<Path>) -> color_eyre::Result<PitchCurve> {
    let path: PathBuf = path.as_ref().to_path_buf();
    let text = fs::read_to_string(&path)?;
    let points = parse_breakpoints(&text)?;
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "curve".to_string());
    Ok(PitchCurve { name, points, path: Some(path), dirty: false, binary_template: None })
}

pub fn save_curve(curve: &PitchCurve, path: impl AsRef<Path>) -> color_eyre::Result<()> {
    fs::write(path, format_breakpoints(&curve.points))?;
    Ok(())
}

/// Where a real CDP binary pitchfile's `data` chunk lives, and the analysis rate (`fmt`
/// chunk's sample-rate field — one "sample" per analysis window, not an audio rate) needed
/// to know what time each of those values falls at.
struct PitchWavInfo {
    data_offset: usize,
    data_len: usize,
    arate: f64,
}

/// Walks a real CDP binary pitchfile's RIFF chunks (`fmt `/`data`, ignoring `PEAK`/`cue `/
/// `LIST` and anything else — this app never needs to understand those, only preserve them
/// byte-for-byte) far enough to locate the `data` chunk's payload and the `fmt` chunk's
/// sample-rate field. `None` for anything that isn't a well-formed RIFF/WAVE file with both
/// chunks present — this app only ever calls it on bytes a real CDP tool just wrote, never
/// on arbitrary user input.
fn parse_pitch_wav(bytes: &[u8]) -> Option<PitchWavInfo> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut pos = 12;
    let mut arate = None;
    let mut data = None;
    while pos + 8 <= bytes.len() {
        let chunk_id = &bytes[pos..pos + 4];
        let chunk_size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().ok()?) as usize;
        let body_start = pos + 8;
        if body_start + chunk_size > bytes.len() {
            break;
        }
        if chunk_id == b"fmt " && chunk_size >= 8 {
            let sample_rate = u32::from_le_bytes(bytes[body_start + 4..body_start + 8].try_into().ok()?);
            arate = Some(sample_rate as f64);
        }
        if chunk_id == b"data" {
            data = Some((body_start, chunk_size));
        }
        pos = body_start + chunk_size + (chunk_size % 2); // RIFF chunks are word-aligned
    }
    let (data_offset, data_len) = data?;
    Some(PitchWavInfo { data_offset, data_len, arate: arate? })
}

/// The exact time (seconds) of every analysis window a binary pitchfile's `data` chunk
/// holds one value for — `resample_to_grid`'s target grid when baking a hand-edited curve
/// back into this template before running a transform on it.
pub fn pitch_wav_grid_times(template: &[u8]) -> Option<Vec<f64>> {
    let info = parse_pitch_wav(template)?;
    let n = info.data_len / 4;
    Some((0..n).map(|i| i as f64 / info.arate).collect())
}

/// Returns a copy of `template` with its `data` chunk's float values replaced by
/// `new_values` — every other chunk (`fmt `, `PEAK`, `cue `, the `LIST`/`adtl`/`note` chunk
/// carrying CDP's own "is a pitch file" marker) stays byte-identical, which is exactly what
/// the real binary was confirmed to accept (every value replaced, header/metadata
/// untouched). `None` if `new_values.len()` doesn't match the template's own point count —
/// callers always get that count via `pitch_wav_grid_times(template).len()` first
/// (`resample_to_grid` guarantees a matching length), so a mismatch here would mean a
/// caller bug, not a real runtime condition.
pub fn splice_pitch_wav_data(template: &[u8], new_values: &[f64]) -> Option<Vec<u8>> {
    let info = parse_pitch_wav(template)?;
    if new_values.len() * 4 != info.data_len {
        return None;
    }
    let mut out = template.to_vec();
    for (i, &v) in new_values.iter().enumerate() {
        let offset = info.data_offset + i * 4;
        out[offset..offset + 4].copy_from_slice(&(v as f32).to_le_bytes());
    }
    Some(out)
}

/// Linearly interpolates `points` (this app's own, possibly hand-edited, breakpoint curve)
/// onto `grid_times` (a binary pitchfile template's own per-window times) — how a
/// hand-edited curve gets baked back into CDP's binary format before being fed to a
/// transform (`splice_pitch_wav_data`'s doc comment). Clamps to the first/last point's
/// value outside `points`' own time range, same convention as every envelope-shaped
/// parameter elsewhere in this app.
pub fn resample_to_grid(points: &[(f64, f64)], grid_times: &[f64]) -> Vec<f64> {
    grid_times
        .iter()
        .map(|&t| {
            let Some(&(first_t, first_v)) = points.first() else { return 0.0 };
            if t <= first_t {
                return first_v;
            }
            let Some(&(last_t, last_v)) = points.last() else { return first_v };
            if t >= last_t {
                return last_v;
            }
            for pair in points.windows(2) {
                let (t0, v0) = pair[0];
                let (t1, v1) = pair[1];
                if t >= t0 && t <= t1 {
                    if (t1 - t0).abs() < 1e-12 {
                        return v0;
                    }
                    return v0 + (v1 - v0) * (t - t0) / (t1 - t0);
                }
            }
            last_v
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal but structurally real RIFF/WAVE pitch file: `fmt ` (IEEE float,
    /// mono, `arate` as the sample-rate field — CDP's own convention, confirmed against a
    /// real `repitch getpitch` mode-1 output) + an unrelated odd-sized filler chunk (to
    /// confirm `parse_pitch_wav` correctly skips over chunks it doesn't care about,
    /// including the RIFF word-alignment padding an odd chunk size requires) + `data`
    /// (`values`, as float32 LE). Deliberately omits the real file's `PEAK`/`cue `/`LIST`
    /// chunks — `parse_pitch_wav` never needs them, only preserves them byte-for-byte when
    /// they're present in a real template.
    fn fake_pitch_wav(arate: u32, values: &[f32]) -> Vec<u8> {
        let mut fmt_body = Vec::new();
        fmt_body.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
        fmt_body.extend_from_slice(&1u16.to_le_bytes()); // mono
        fmt_body.extend_from_slice(&arate.to_le_bytes());
        fmt_body.extend_from_slice(&(arate * 4).to_le_bytes()); // byte rate
        fmt_body.extend_from_slice(&4u16.to_le_bytes()); // block align
        fmt_body.extend_from_slice(&32u16.to_le_bytes()); // bits per sample

        let mut data_body = Vec::new();
        for &v in values {
            data_body.extend_from_slice(&v.to_le_bytes());
        }

        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&0u32.to_le_bytes()); // placeholder, never read by parse_pitch_wav
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&(fmt_body.len() as u32).to_le_bytes());
        out.extend_from_slice(&fmt_body);
        out.extend_from_slice(b"jUNK");
        out.extend_from_slice(&5u32.to_le_bytes());
        out.extend_from_slice(&[0xABu8; 5]);
        out.push(0); // word-alignment pad byte for the odd-sized filler chunk
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data_body.len() as u32).to_le_bytes());
        out.extend_from_slice(&data_body);
        out
    }

    #[test]
    fn pitch_wav_grid_times_reads_arate_from_fmt_and_one_time_per_data_value() {
        let wav = fake_pitch_wav(344, &[219.7, 219.8, 219.9]);
        let times = pitch_wav_grid_times(&wav).expect("valid pitch wav");
        assert_eq!(times.len(), 3);
        assert!((times[0] - 0.0).abs() < 1e-9);
        assert!((times[1] - 1.0 / 344.0).abs() < 1e-9);
        assert!((times[2] - 2.0 / 344.0).abs() < 1e-9);
    }

    #[test]
    fn parse_pitch_wav_returns_none_for_non_riff_bytes() {
        assert_eq!(pitch_wav_grid_times(b"not a riff file"), None);
    }

    #[test]
    fn splice_pitch_wav_data_replaces_every_value_leaving_other_bytes_untouched() {
        let wav = fake_pitch_wav(344, &[219.7, 219.8, 219.9]);
        let spliced = splice_pitch_wav_data(&wav, &[100.0, 200.0, 300.0]).expect("same length");

        assert_eq!(spliced.len(), wav.len(), "splicing must never change the file's size");
        assert_eq!(&spliced[..wav.len() - 12], &wav[..wav.len() - 12], "only the data chunk's bytes should change");

        let info_offset = wav.len() - 12; // data chunk payload starts here in this fixture
        let vals: Vec<f32> = spliced[info_offset..]
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        assert_eq!(vals, vec![100.0, 200.0, 300.0]);
    }

    #[test]
    fn splice_pitch_wav_data_rejects_a_length_mismatch() {
        let wav = fake_pitch_wav(344, &[219.7, 219.8, 219.9]);
        assert_eq!(splice_pitch_wav_data(&wav, &[1.0, 2.0]), None);
    }

    #[test]
    fn resample_to_grid_linearly_interpolates_between_points() {
        let points = vec![(0.0, 200.0), (1.0, 400.0)];
        let grid = vec![0.0, 0.25, 0.5, 0.75, 1.0];
        let resampled = resample_to_grid(&points, &grid);
        assert_eq!(resampled, vec![200.0, 250.0, 300.0, 350.0, 400.0]);
    }

    #[test]
    fn resample_to_grid_clamps_outside_the_points_own_range() {
        let points = vec![(0.5, 200.0), (1.5, 400.0)];
        let grid = vec![0.0, 0.5, 1.0, 1.5, 2.0];
        let resampled = resample_to_grid(&points, &grid);
        assert_eq!(resampled, vec![200.0, 200.0, 300.0, 400.0, 400.0]);
    }

    #[test]
    fn extract_then_transform_round_trip_preserves_a_hand_edit() {
        // Simulates the real workflow: extract a curve (a template + its decoded points),
        // hand-edit one point, resample onto the template's own grid, splice, and confirm
        // the edited value survives — the exact sequence `plan_curve_transform_job` (once
        // built) will perform before ever invoking a real CDP transform.
        let template = fake_pitch_wav(344, &[219.7, 219.7, 219.7]);
        let grid = pitch_wav_grid_times(&template).unwrap();
        let mut points: Vec<(f64, f64)> = grid.iter().map(|&t| (t, 219.7)).collect();
        points[1].1 = 440.0; // hand edit: double the middle point's pitch

        let resampled = resample_to_grid(&points, &grid);
        let spliced = splice_pitch_wav_data(&template, &resampled).unwrap();

        let info_offset = spliced.len() - 12;
        let vals: Vec<f32> = spliced[info_offset..]
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        assert_eq!(vals, vec![219.7, 440.0, 219.7]);
    }

    #[test]
    fn parse_breakpoints_reads_time_value_pairs() {
        let points = parse_breakpoints("0.0 220.0\n0.5 440.5\n1.0 330").unwrap();
        assert_eq!(points, vec![(0.0, 220.0), (0.5, 440.5), (1.0, 330.0)]);
    }

    #[test]
    fn parse_breakpoints_ignores_blank_lines() {
        let points = parse_breakpoints("0.0 100\n\n\n1.0 200\n").unwrap();
        assert_eq!(points, vec![(0.0, 100.0), (1.0, 200.0)]);
    }

    #[test]
    fn parse_breakpoints_rejects_a_line_missing_a_value() {
        assert!(parse_breakpoints("0.0 100\n0.5\n").is_err());
    }

    #[test]
    fn parse_breakpoints_rejects_non_numeric_fields() {
        assert!(parse_breakpoints("0.0 abc").is_err());
    }

    #[test]
    fn format_breakpoints_round_trips_through_parse() {
        let points = vec![(0.0, 220.0), (0.25, 233.08), (1.0, 440.0)];
        let text = format_breakpoints(&points);
        assert_eq!(parse_breakpoints(&text).unwrap(), points);
    }

    #[test]
    fn save_then_load_round_trips_and_derives_name_from_filename() {
        let dir = std::env::temp_dir().join(format!("tui-wave-curve-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("my_curve.txt");
        let curve = PitchCurve::new("my_curve", vec![(0.0, 100.0), (1.0, 200.0)]);

        save_curve(&curve, &path).unwrap();
        let loaded = load_curve(&path).unwrap();

        assert_eq!(loaded.points, curve.points);
        assert_eq!(loaded.name, "my_curve");
        assert_eq!(loaded.path, Some(path.clone()));
        assert!(!loaded.dirty);

        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn new_curve_is_dirty_with_no_path() {
        let curve = PitchCurve::new("untitled", vec![(0.0, 220.0)]);
        assert!(curve.dirty);
        assert_eq!(curve.path, None);
    }
}
