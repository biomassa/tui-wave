/// Horizontal/vertical zoom and scroll state for the waveform view. Pure state — no
/// rendering or terminal dependency, so the zoom-math is unit-testable on its own.
pub struct Viewport {
    pub samples_per_column: f64,
    pub scroll_offset: usize,
    pub amplitude_scale: f32,
    pub min_samples_per_column: f64,
    pub max_samples_per_column: f64,
    /// Total sample count of the document, kept in sync by the caller (it can shrink/grow
    /// after edits). Used to clamp `scroll_offset` so the visible window never overhangs
    /// past end-of-file — without this, certain scroll/zoom states leave a blank gap
    /// between the right edge of the waveform and the right border of the window.
    pub total_len: usize,
    /// Off by default. When on, vertical zoom auto-fits to the document's peak amplitude
    /// (and re-fits after edits); the dB scale gutters switch from absolute dBFS to
    /// dB-relative-to-peak to match.
    pub auto_vertical_zoom: bool,
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
            total_len,
            auto_vertical_zoom: false,
        }
    }

    /// Number of samples spanned by `width` terminal columns at the current zoom level.
    pub fn span(&self, width: u16) -> usize {
        (width.max(1) as f64 * self.samples_per_column) as usize
    }

    /// Largest `scroll_offset` that doesn't let the window overhang past `total_len`.
    fn max_scroll_offset(&self, width: u16) -> usize {
        self.total_len.saturating_sub(self.span(width))
    }

    /// Scroll so `sample` is visible, snapping to the nearest edge rather than re-centering
    /// (keeps the view stable instead of jumping every time the cursor nears an edge), then
    /// clamps so the window never overhangs past end-of-file.
    pub fn ensure_visible(&mut self, sample: usize, width: u16) {
        let span = self.span(width).max(1);
        if sample < self.scroll_offset {
            self.scroll_offset = sample;
        } else if sample >= self.scroll_offset + span {
            self.scroll_offset = sample + 1 - span;
        }
        self.scroll_offset = self.scroll_offset.min(self.max_scroll_offset(width));
    }

    /// Zoom by `factor` (>1.0 = zoom in, <1.0 = zoom out) while keeping `anchor_sample`
    /// fixed at the same terminal column — without this, zooming would disorientingly
    /// shift whatever the user is looking at.
    pub fn zoom(&mut self, factor: f64, anchor_sample: usize, width: u16) {
        let anchor_col = (anchor_sample as f64 - self.scroll_offset as f64) / self.samples_per_column;
        let max_zoom_out = (self.total_len as f64 / width.max(1) as f64).max(1.0);
        self.samples_per_column = (self.samples_per_column / factor)
            .clamp(self.min_samples_per_column, self.max_samples_per_column.min(max_zoom_out));
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

    /// Sets the amplitude scale directly, clamped to the same bounds as the zoom-vertical
    /// actions. Used by auto vertical zoom to fit the display to a file's peak amplitude.
    pub fn set_amplitude_scale(&mut self, scale: f32) {
        self.amplitude_scale = scale.clamp(MIN_AMPLITUDE_SCALE, MAX_AMPLITUDE_SCALE);
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
        assert!(!viewport.auto_vertical_zoom);
    }

    #[test]
    fn clamps_samples_per_column_for_tiny_files() {
        // A file shorter than the terminal width must not produce samples_per_column < 1.
        let viewport = Viewport::fit_to_width(10, 80);
        assert!(viewport.samples_per_column >= 1.0);
    }

    /// A zoomed-in viewport (span well under total_len) used to exercise scroll behavior
    /// without the anti-overhang clamp trivially forcing scroll_offset to 0.
    fn zoomed_in_viewport(total_len: usize, samples_per_column: f64) -> Viewport {
        Viewport {
            samples_per_column,
            scroll_offset: 0,
            amplitude_scale: 1.0,
            min_samples_per_column: 1.0,
            max_samples_per_column: total_len as f64,
            total_len,
            auto_vertical_zoom: false,
        }
    }

    #[test]
    fn ensure_visible_scrolls_left_when_cursor_before_view() {
        let mut viewport = zoomed_in_viewport(1_000_000, 100.0);
        viewport.scroll_offset = 5_000;
        viewport.ensure_visible(1_000, 80);
        assert_eq!(viewport.scroll_offset, 1_000);
    }

    #[test]
    fn ensure_visible_scrolls_right_when_cursor_past_view() {
        let mut viewport = zoomed_in_viewport(1_000_000, 100.0);
        let span = viewport.span(80);
        viewport.ensure_visible(span + 500, 80);
        assert_eq!(viewport.scroll_offset, span + 500 + 1 - span.max(1));
    }

    #[test]
    fn ensure_visible_never_overhangs_past_end_of_file() {
        // total_len only slightly larger than one window's span: requesting a sample near
        // the end must not push scroll_offset far enough to leave blank space on the right.
        let mut viewport = zoomed_in_viewport(8_500, 100.0); // span(80) = 8000
        viewport.ensure_visible(8_499, 80);
        assert_eq!(viewport.scroll_offset, 500); // total_len - span, not 8499+1-8000=500 too — but never more
        assert!(viewport.scroll_offset + viewport.span(80) <= viewport.total_len);
    }

    #[test]
    fn whole_file_fits_in_one_window_forces_scroll_to_zero() {
        // When span >= total_len, there's no room to scroll without overhanging — any
        // nonzero scroll_offset would leave a gap on the right.
        let mut viewport = Viewport::fit_to_width(100_000, 80); // span == total_len here
        viewport.scroll_offset = 12_345; // simulate a stale/manual scroll position
        viewport.ensure_visible(1_000, 80);
        assert_eq!(viewport.scroll_offset, 0);
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
    fn zoom_never_leaves_a_trailing_gap() {
        let mut viewport = Viewport::fit_to_width(1_000_000, 80);
        for _ in 0..5 {
            viewport.zoom_in(900_000, 80);
            assert!(viewport.scroll_offset + viewport.span(80) <= viewport.total_len);
        }
    }

    #[test]
    fn zoom_in_bottoms_out_at_one_sample_per_column() {
        // Past max zoom, samples_per_column must stay pinned at 1.0 (one terminal column
        // == one sample) rather than going sub-pixel — further zoom-in attempts are no-ops.
        let mut viewport = Viewport::fit_to_width(1_000, 80);
        for _ in 0..100 {
            viewport.zoom_in(500, 80);
        }
        assert_eq!(viewport.samples_per_column, 1.0);
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
