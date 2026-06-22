/// Horizontal/vertical zoom and scroll state for the waveform view. Pure state — no
/// rendering or terminal dependency, so the zoom-math is unit-testable on its own.
pub struct Viewport {
    pub samples_per_column: f64,
    pub scroll_offset: usize,
    pub amplitude_scale: f32,
    pub min_samples_per_column: f64,
    pub max_samples_per_column: f64,
}

const ZOOM_FACTOR: f64 = 1.5;
const VERTICAL_ZOOM_FACTOR: f32 = 1.25;
const MIN_AMPLITUDE_SCALE: f32 = 0.1;
const MAX_AMPLITUDE_SCALE: f32 = 10.0;

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

    /// Number of samples spanned by `width` terminal columns at the current zoom level.
    pub fn span(&self, width: u16) -> usize {
        (width.max(1) as f64 * self.samples_per_column) as usize
    }

    /// Scroll so `sample` is visible, snapping to the nearest edge rather than re-centering
    /// (keeps the view stable instead of jumping every time the cursor nears an edge).
    pub fn ensure_visible(&mut self, sample: usize, width: u16) {
        let span = self.span(width).max(1);
        if sample < self.scroll_offset {
            self.scroll_offset = sample;
        } else if sample >= self.scroll_offset + span {
            self.scroll_offset = sample + 1 - span;
        }
    }

    /// Zoom by `factor` (>1.0 = zoom in, <1.0 = zoom out) while keeping `anchor_sample`
    /// fixed at the same terminal column — without this, zooming would disorientingly
    /// shift whatever the user is looking at.
    pub fn zoom(&mut self, factor: f64, anchor_sample: usize, width: u16) {
        let anchor_col = (anchor_sample as f64 - self.scroll_offset as f64) / self.samples_per_column;
        self.samples_per_column = (self.samples_per_column / factor)
            .clamp(self.min_samples_per_column, self.max_samples_per_column);
        let new_offset = anchor_sample as f64 - anchor_col * self.samples_per_column;
        self.scroll_offset = new_offset.max(0.0) as usize;
        self.ensure_visible(anchor_sample, width);
    }

    pub fn zoom_in(&mut self, anchor_sample: usize, width: u16) {
        self.zoom(ZOOM_FACTOR, anchor_sample, width);
    }

    pub fn zoom_out(&mut self, anchor_sample: usize, width: u16) {
        self.zoom(1.0 / ZOOM_FACTOR, anchor_sample, width);
    }

    pub fn zoom_in_vertical(&mut self) {
        self.amplitude_scale =
            (self.amplitude_scale * VERTICAL_ZOOM_FACTOR).clamp(MIN_AMPLITUDE_SCALE, MAX_AMPLITUDE_SCALE);
    }

    pub fn zoom_out_vertical(&mut self) {
        self.amplitude_scale =
            (self.amplitude_scale / VERTICAL_ZOOM_FACTOR).clamp(MIN_AMPLITUDE_SCALE, MAX_AMPLITUDE_SCALE);
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

    #[test]
    fn ensure_visible_scrolls_left_when_cursor_before_view() {
        let mut viewport = Viewport::fit_to_width(100_000, 80);
        viewport.scroll_offset = 5_000;
        viewport.ensure_visible(1_000, 80);
        assert_eq!(viewport.scroll_offset, 1_000);
    }

    #[test]
    fn ensure_visible_scrolls_right_when_cursor_past_view() {
        let mut viewport = Viewport::fit_to_width(100_000, 80);
        let span = viewport.span(80);
        viewport.ensure_visible(span + 500, 80);
        assert_eq!(viewport.scroll_offset, span + 500 + 1 - span.max(1));
    }

    #[test]
    fn zoom_in_keeps_anchor_sample_at_same_column() {
        let mut viewport = Viewport::fit_to_width(100_000, 80);
        let anchor = 40_000;
        let col_before = (anchor as f64 - viewport.scroll_offset as f64) / viewport.samples_per_column;

        viewport.zoom_in(anchor, 80);

        let col_after = (anchor as f64 - viewport.scroll_offset as f64) / viewport.samples_per_column;
        assert!((col_before - col_after).abs() < 1.0);
    }

    #[test]
    fn vertical_zoom_clamps_to_bounds() {
        let mut viewport = Viewport::fit_to_width(1000, 80);
        for _ in 0..100 {
            viewport.zoom_in_vertical();
        }
        assert!(viewport.amplitude_scale <= MAX_AMPLITUDE_SCALE);
        for _ in 0..100 {
            viewport.zoom_out_vertical();
        }
        assert!(viewport.amplitude_scale >= MIN_AMPLITUDE_SCALE);
    }
}
