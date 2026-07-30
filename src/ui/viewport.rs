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
    /// Index of the topmost channel pane currently drawn. Always 0 while every channel fits
    /// (see [`VISIBLE_CHANNELS`]) — the vertical counterpart to `scroll_offset`, and kept
    /// here rather than on `App` for the same reason: it's pure view state, so the clamping
    /// math is unit-testable without a terminal.
    pub channel_scroll: usize,
}

const ZOOM_FACTOR: f64 = 1.5;
const VERTICAL_ZOOM_FACTOR: f32 = 1.25;
const MIN_AMPLITUDE_SCALE: f32 = 0.1;
const MAX_AMPLITUDE_SCALE: f32 = 10.0;

/// Fewest channel panes a high-channel-count file is ever windowed down to.
///
/// Beyond this the waveform area scrolls vertically (`Viewport::channel_scroll`) instead of
/// subdividing further: `channel_pane_rects` splits the waveform area evenly, so at 30 channels
/// every pane gets a single row — no centre row for the zero line, and a dB gutter that can show
/// one mark — and past ~42 channels the split degenerates to zero-height panes entirely.
///
/// A floor rather than a cap: see [`visible_channels`], which shows *more* than this when the
/// terminal is tall enough to give each pane a usable height.
pub const VISIBLE_CHANNELS: usize = 6;

/// Rows a pane needs before another pane is worth adding.
///
/// Six panes of this is 42 rows, which is about the waveform height of a maximised terminal at a
/// typical font size — i.e. the size [`VISIBLE_CHANNELS`] was chosen against. Sized so a pane keeps
/// an odd height with a real centre row for the zero line (`channel_pane_rects` rounds down to
/// odd), plus room for a few dB gutter marks either side of it.
pub const MIN_PANE_ROWS: usize = 7;

/// How many channel panes actually get drawn.
///
/// At or below [`VISIBLE_CHANNELS`] this is simply the channel count — every channel is drawn and
/// nothing scrolls, including on a terminal too short to give each pane a row, where
/// `channel_pane_rects`'s own degrade path takes over exactly as it did before channel scrolling
/// existed.
///
/// Above it, the count **grows with the available height** rather than staying at six. Panes are
/// forced to an odd height so amplitude zero lands on a real centre row, so six panes can only ever
/// occupy `6 * odd` rows — on a tall display that left up to eleven unused rows below the last pane
/// and a conspicuous empty band under the waveform (user report, on a higher-resolution screen).
/// Fitting as many panes as [`MIN_PANE_ROWS`] allows both closes that gap and shows more of a
/// 58-channel file at once, which is the more useful answer anyway.
///
/// Never fewer than [`VISIBLE_CHANNELS`], so no terminal that worked before shows less than it did;
/// never more than the channel count, and never more than there are rows. Returns at least 1 so
/// callers can divide by it.
pub fn visible_channels(height: u16, channel_count: usize) -> usize {
    let count = channel_count.max(1);
    if count <= VISIBLE_CHANNELS {
        return count;
    }
    let height = (height as usize).max(1);
    let fits = height / MIN_PANE_ROWS;
    fits.max(VISIBLE_CHANNELS).min(count).min(height).max(1)
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
            total_len,
            auto_vertical_zoom: false,
            channel_scroll: 0,
        }
    }

    /// Largest `channel_scroll` that leaves the last window full rather than trailing blank
    /// panes: 30 channels showing 6 at a time tops out at 24, not 29.
    pub fn max_channel_scroll(channel_count: usize, height: u16) -> usize {
        channel_count.saturating_sub(visible_channels(height, channel_count))
    }

    /// Pins `channel_scroll` inside range. Called every frame, so shrinking the terminal,
    /// switching to a document with fewer channels, or an edit that removes channels
    /// (Remove Empty Channels) can never leave the window pointing past the end.
    pub fn clamp_channel_scroll(&mut self, channel_count: usize, height: u16) {
        self.channel_scroll =
            self.channel_scroll.min(Self::max_channel_scroll(channel_count, height));
    }

    /// Moves the channel window by `delta` panes, clamped at both ends. Returns whether it
    /// actually moved, so a caller can leave the event unconsumed when it didn't.
    pub fn scroll_channels(&mut self, delta: isize, channel_count: usize, height: u16) -> bool {
        let max = Self::max_channel_scroll(channel_count, height) as isize;
        let next = (self.channel_scroll as isize + delta).clamp(0, max) as usize;
        let moved = next != self.channel_scroll;
        self.channel_scroll = next;
        moved
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

    /// Scrolls so `sample` sits at the horizontal center of the view (clamped so the window
    /// never overhangs past end-of-file, same as `ensure_visible`) — used by "Viewport
    /// Follows Playback" once the playhead reaches the right edge, so it then stays
    /// centered while the view scrolls continuously alongside it.
    pub fn center_on(&mut self, sample: usize, width: u16) {
        let half_span = self.span(width) as f64 / 2.0;
        let target = (sample as f64 - half_span).max(0.0) as usize;
        self.scroll_offset = target.min(self.max_scroll_offset(width));
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

    /// Clamps `samples_per_column` so the visible window can never span more audio than the
    /// document actually holds — i.e. zoomed out no further than "the whole file exactly fills
    /// the pane".
    ///
    /// `zoom` already applies this bound as it goes, but nothing re-applied it when the
    /// *document* changed underneath a fixed zoom level. Any process or undo that shortens the
    /// audio (and CDP results are rarely the same length as their input) therefore left the
    /// view zoomed out for the old, longer file, drawing the whole waveform squeezed into the
    /// left edge with an expanse of empty pane to its right — user report, 2026-07-27, at
    /// 5196.5 samples/column on a file far shorter than that spanned.
    ///
    /// Returns `true` if it actually changed anything, so a caller that needs to re-run
    /// dependent work (rebuilding a waveform cache, re-fitting auto vertical zoom) can tell.
    /// Leaves `scroll_offset` alone — callers pair this with `ensure_visible`, which is what
    /// re-clamps that against the new span.
    pub fn clamp_zoom_to_content(&mut self, width: u16) -> bool {
        let max_zoom_out = (self.total_len as f64 / width.max(1) as f64).max(1.0);
        let clamped = self
            .samples_per_column
            .clamp(self.min_samples_per_column, self.max_samples_per_column.min(max_zoom_out));
        if clamped == self.samples_per_column {
            return false;
        }
        self.samples_per_column = clamped;
        true
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

    /// Below the cap every channel is drawn, so nothing about a mono/stereo file changes.
    #[test]
    fn every_channel_is_visible_below_the_window_cap() {
        for count in 1..=VISIBLE_CHANNELS {
            assert_eq!(visible_channels(40, count), count);
            assert_eq!(Viewport::max_channel_scroll(count, 40), 0);
        }
    }

    /// On the height `VISIBLE_CHANNELS` was chosen against, the window is exactly that — so no
    /// terminal that worked before this became height-dependent shows a different number of panes.
    #[test]
    fn the_window_is_the_floor_on_an_ordinary_height() {
        assert_eq!(visible_channels(40, 30), VISIBLE_CHANNELS);
        assert_eq!(visible_channels(40, 120), VISIBLE_CHANNELS);
        // Anything below `VISIBLE_CHANNELS * MIN_PANE_ROWS` stays at the floor.
        for height in 8..=(VISIBLE_CHANNELS * MIN_PANE_ROWS) as u16 {
            assert_eq!(
                visible_channels(height, 30),
                VISIBLE_CHANNELS,
                "height {height} must not drop below the floor"
            );
        }
    }

    /// On a taller display the window *grows*, which is what stops six odd-height panes leaving a
    /// conspicuous empty band under the waveform — and shows more of a high-channel file at once.
    #[test]
    fn the_window_grows_with_the_available_height() {
        assert_eq!(visible_channels(63, 30), 9, "63 rows fits nine 7-row panes");
        assert_eq!(visible_channels(70, 30), 10);
        assert_eq!(visible_channels(140, 30), 20);
        // Never more channels than exist, however tall the terminal.
        assert_eq!(visible_channels(400, 30), 30);
        assert_eq!(visible_channels(400, 8), 8);
    }

    /// Whatever the height, a grown window's panes still get at least `MIN_PANE_ROWS` rows — which
    /// is the whole point of sizing by it, since a pane needs an odd height with a real centre row
    /// for the zero line plus room for dB marks.
    #[test]
    fn a_grown_window_never_starves_its_panes() {
        for height in (VISIBLE_CHANNELS * MIN_PANE_ROWS) as u16..300 {
            let n = visible_channels(height, 200);
            let pane = height as usize / n;
            assert!(
                pane >= MIN_PANE_ROWS,
                "height {height} -> {n} panes of {pane} rows, below the {MIN_PANE_ROWS} minimum"
            );
        }
    }

    /// The gap under the last pane is what prompted this: panes are forced to an odd height, so
    /// `n * pane` can fall short of the area. Growing the count keeps that shortfall small
    /// relative to the area instead of letting it reach eleven rows on a tall screen.
    #[test]
    fn the_unused_band_below_the_panes_stays_small() {
        for height in 42u16..200 {
            let n = visible_channels(height, 200) as u16;
            let base = height / n;
            let pane = if base % 2 == 1 { base } else { base - 1 };
            let leftover = height - n * pane;
            assert!(
                leftover * 5 <= height,
                "height {height}: {leftover} rows unused below {n} panes of {pane} — over a fifth"
            );
        }
    }

    /// Past the cap, a terminal too short to give each pane a row shrinks the window rather
    /// than asking for more panes than there are rows. Never zero — callers divide by it.
    #[test]
    fn the_window_shrinks_on_a_very_short_terminal() {
        assert_eq!(visible_channels(3, 30), 3);
        assert_eq!(visible_channels(1, 30), 1);
        assert_eq!(visible_channels(0, 30), 1);
    }

    /// ...but at or below the cap the height is irrelevant: every channel is still drawn, and
    /// a too-short area is `channel_pane_rects`'s own degrade path to handle, exactly as it
    /// was before channel scrolling existed. A stereo file must never scroll.
    #[test]
    fn a_short_terminal_never_hides_channels_below_the_cap() {
        assert_eq!(visible_channels(1, 2), 2);
        assert_eq!(visible_channels(1, 6), 6);
        assert_eq!(Viewport::max_channel_scroll(2, 1), 0);
    }

    /// The last window is flush with the end: 30 channels showing 6 tops out at 24, so the
    /// final view is channels 25-30 rather than 30 plus five blank panes.
    #[test]
    fn the_last_channel_window_is_full_not_trailing_blanks() {
        assert_eq!(Viewport::max_channel_scroll(30, 40), 24);
        let mut v = Viewport::fit_to_width(1_000, 80);
        v.channel_scroll = 29;
        v.clamp_channel_scroll(30, 40);
        assert_eq!(v.channel_scroll, 24);
    }

    /// The clamp is what protects against a channel count that *shrinks* underneath the
    /// scroll position — a terminal resize, a buffer switch, or Remove Empty Channels.
    #[test]
    fn clamping_recovers_when_the_channel_count_shrinks() {
        let mut v = Viewport::fit_to_width(1_000, 80);
        v.channel_scroll = 24;
        v.clamp_channel_scroll(4, 40);
        assert_eq!(v.channel_scroll, 0, "4 channels all fit, so there is nothing to scroll");
    }

    #[test]
    fn scrolling_channels_clamps_at_both_ends_and_reports_movement() {
        let mut v = Viewport::fit_to_width(1_000, 80);
        assert!(v.scroll_channels(1, 30, 40));
        assert_eq!(v.channel_scroll, 1);
        assert!(v.scroll_channels(-5, 30, 40), "overshooting the top still moves to it");
        assert_eq!(v.channel_scroll, 0);
        assert!(!v.scroll_channels(-1, 30, 40), "already at the top, nothing moved");
        assert!(v.scroll_channels(100, 30, 40));
        assert_eq!(v.channel_scroll, 24);
        assert!(!v.scroll_channels(1, 30, 40), "already at the bottom, nothing moved");
    }

    #[test]
    fn scrolling_channels_is_inert_when_every_channel_fits() {
        let mut v = Viewport::fit_to_width(1_000, 80);
        assert!(!v.scroll_channels(1, 2, 40));
        assert_eq!(v.channel_scroll, 0);
    }

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
            channel_scroll: 0,
        }
    }

    /// A document that got shorter (a process, or an undo of one) must not leave the view
    /// zoomed out for the old length — that draws the whole waveform crushed into the left
    /// edge with empty pane beside it (user report, 2026-07-27).
    #[test]
    fn shrinking_the_document_clamps_the_zoom_back_to_fit() {
        let mut viewport = zoomed_in_viewport(1_000_000, 5_000.0);
        // The file is cut down to a twentieth of its old length.
        viewport.total_len = 50_000;
        assert!(viewport.clamp_zoom_to_content(80), "the zoom needed correcting");
        // 50000 / 80 = 625: exactly "the whole file fills the pane", never more.
        assert!((viewport.samples_per_column - 625.0).abs() < 0.01, "{}", viewport.samples_per_column);
        assert!(
            viewport.span(80) <= viewport.total_len,
            "the visible span never exceeds the audio that exists"
        );
    }

    /// A zoom that already fits is left exactly as it is — the clamp must never *change* the
    /// user's zoom level just because it ran.
    #[test]
    fn clamping_a_zoom_that_already_fits_is_a_no_op() {
        let mut viewport = zoomed_in_viewport(1_000_000, 100.0);
        assert!(!viewport.clamp_zoom_to_content(80), "nothing to correct");
        assert!((viewport.samples_per_column - 100.0).abs() < f64::EPSILON);
    }

    /// Growing the document (undoing a cut, pasting) must not zoom *out* on its own — the
    /// clamp is a ceiling, not a fit.
    #[test]
    fn growing_the_document_leaves_the_zoom_alone() {
        let mut viewport = zoomed_in_viewport(10_000, 50.0);
        viewport.total_len = 1_000_000;
        assert!(!viewport.clamp_zoom_to_content(80));
        assert!((viewport.samples_per_column - 50.0).abs() < f64::EPSILON);
    }

    /// The floor still wins: a file shorter than the pane is wide must not drive
    /// samples_per_column below 1, which would mean fractional samples per column.
    #[test]
    fn a_file_shorter_than_the_pane_clamps_to_one_sample_per_column() {
        let mut viewport = zoomed_in_viewport(1_000_000, 900.0);
        viewport.total_len = 10;
        viewport.clamp_zoom_to_content(80);
        assert!(viewport.samples_per_column >= 1.0, "{}", viewport.samples_per_column);
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
    fn center_on_puts_sample_at_the_middle_column() {
        let mut viewport = zoomed_in_viewport(1_000_000, 100.0);
        viewport.center_on(50_000, 80);
        let span = viewport.span(80);
        // sample 50_000 should now sit at column span/2 from scroll_offset.
        assert_eq!(viewport.scroll_offset + span / 2, 50_000);
    }

    #[test]
    fn center_on_clamps_to_end_of_file() {
        let mut viewport = zoomed_in_viewport(10_000, 100.0); // span(80) = 8000
        viewport.center_on(9_999, 80);
        assert!(viewport.scroll_offset + viewport.span(80) <= viewport.total_len);
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
