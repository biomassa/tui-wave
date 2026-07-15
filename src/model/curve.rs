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
}

impl PitchCurve {
    pub fn new(name: impl Into<String>, points: Vec<(f64, f64)>) -> Self {
        PitchCurve { name: name.into(), points, path: None, dirty: true }
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
    Ok(PitchCurve { name, points, path: Some(path), dirty: false })
}

pub fn save_curve(curve: &PitchCurve, path: impl AsRef<Path>) -> color_eyre::Result<()> {
    fs::write(path, format_breakpoints(&curve.points))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
