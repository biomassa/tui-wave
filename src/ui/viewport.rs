/// Horizontal/vertical zoom and scroll state for the waveform view. Pure state — no
/// rendering or terminal dependency, so the zoom-math is unit-testable on its own.
pub struct Viewport {
    pub samples_per_column: f64,
    pub scroll_offset: usize,
    pub amplitude_scale: f32,
    pub min_samples_per_column: f64,
    pub max_samples_per_column: f64,
}

impl Viewport {
    /// Fit the whole file into `width` columns.
    pub fn fit_to_width(total_len: usize, width: usize) -> Self {
        let width = width.max(1);
        let max_samples_per_column = (total_len as f64 / 4.0).max(1.0);
        let samples_per_column = (total_len as f64 / width as f64)
            .max(1.0)
            .min(max_samples_per_column);
        Self {
            samples_per_column,
            scroll_offset: 0,
            amplitude_scale: 1.0,
            min_samples_per_column: 1.0,
            max_samples_per_column,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_whole_file_into_width() {
        let viewport = Viewport::fit_to_width(44_100, 80);
        // 44100 / 80 = 551.25
        assert!((viewport.samples_per_column - 551.25).abs() < 0.01);
        assert_eq!(viewport.scroll_offset, 0);
    }

    #[test]
    fn clamps_samples_per_column_for_tiny_files() {
        // A file shorter than the terminal width must not produce samples_per_column < 1.
        let viewport = Viewport::fit_to_width(10, 80);
        assert!(viewport.samples_per_column >= 1.0);
    }
}
