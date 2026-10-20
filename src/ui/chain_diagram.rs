//! A block diagram of an ExtProcess chain: every step a box, the signal flowing left to right,
//! parallel inputs stacked under the step they feed, and a reused output (`BranchSource::From`)
//! drawn as a line from the box that made it.
//!
//! Pure layout, no rendering: [`layout`] turns a chain into boxes, line cells and text tags on a
//! character grid, and the chain editor draws that grid (`render_chain_diagram` in `app.rs`). The
//! cursor is moved by [`nearest_in_direction`], which picks the box visually nearest in the
//! pressed direction, so it behaves the same on any shape.
//!
//! **One input stack per step.** A step's inputs are laid out as *lanes*, top to bottom, ending
//! at a join column just before its box. For an ordinary step the top lane is the running audio
//! itself, so a plain chain reads as one straight line; a dual-input or multi-input step adds one
//! lane per branch below it. A native combiner (Mixer, Crossfade) reads only its branches, so its
//! first branch is the top lane and the running audio splits into every `Tap` lane. Nesting is the
//! same function again: a branch's own steps are a lane with their own input stacks.
//!
//! **Lines are bits, not glyphs.** Each cell records which of its four sides a line leaves by
//! (left, right, up, down), and the glyph comes from the union at draw time. Junctions and
//! crossings then come out right however many segments meet, without any segment knowing about
//! the others. Boxes are drawn over lines, so a routed line that passes behind a box is hidden
//! there rather than writing over its text.

use std::collections::HashMap;

use crate::model::cdp::chain::{BranchSource, CdpChain, ChainStep, Path, PathSeg};
use crate::model::cdp::CdpCatalog;

/// Rows a box takes: border, name line, preset line, border.
pub const BOX_HEIGHT: u16 = 4;
/// The row of a box (and of the lane it sits in) that the signal enters and leaves on.
pub const SIGNAL_ROW: u16 = 1;
/// The widest a box's text may be. Longer names are cut with an ellipsis, so one long title does
/// not stretch every column of the picture.
pub const MAX_TEXT_WIDTH: usize = 24;
/// Rows between two stacked input lanes.
const LANE_GAP: u16 = 1;
/// Columns between the left edge of an input stack and where its lanes start: room for the
/// split that feeds `Tap` lanes and for the start of a routed line.
const LANE_LEAD: u16 = 3;

pub const LEFT: u8 = 1;
pub const RIGHT: u8 = 2;
pub const UP: u8 = 4;
pub const DOWN: u8 = 8;

/// One process, as drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct DiagramBox {
    pub path: Path,
    pub x: u16,
    pub y: u16,
    pub width: u16,
    /// The name, cut to fit, followed by the backend badge.
    pub title: String,
    /// The step's own Internal Preset, or empty for a process that has none.
    pub preset: String,
    /// Makes its own sound and ignores the audio reaching it.
    pub generates: bool,
}

impl DiagramBox {
    pub fn center(&self) -> (i32, i32) {
        (self.x as i32 + self.width as i32 / 2, self.y as i32 + SIGNAL_ROW as i32)
    }
}

/// Text placed on the grid: the IN and OUT ends, and where a branch reads a picked file.
#[derive(Debug, Clone, PartialEq)]
pub struct Tag {
    pub x: u16,
    pub y: u16,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Diagram {
    pub width: u16,
    pub height: u16,
    pub boxes: Vec<DiagramBox>,
    /// Which sides each line cell connects, as `LEFT | RIGHT | UP | DOWN` bits.
    pub lines: HashMap<(u16, u16), u8>,
    /// Arrowheads, pointing right, at the cell just before what they feed.
    pub arrows: Vec<(u16, u16)>,
    pub tags: Vec<Tag>,
}

impl Diagram {
    fn grow(&mut self, x: u16, y: u16) {
        self.width = self.width.max(x + 1);
        self.height = self.height.max(y + 1);
    }

    fn mark(&mut self, x: u16, y: u16, bits: u8) {
        *self.lines.entry((x, y)).or_insert(0) |= bits;
        self.grow(x, y);
    }

    /// A horizontal line from `x0` to `x1` (either order) on row `y`.
    fn hline(&mut self, x0: u16, x1: u16, y: u16) {
        let (a, b) = if x0 <= x1 { (x0, x1) } else { (x1, x0) };
        for x in a..=b {
            let mut bits = 0;
            if x > a {
                bits |= LEFT;
            }
            if x < b {
                bits |= RIGHT;
            }
            if bits != 0 {
                self.mark(x, y, bits);
            }
        }
    }

    /// A vertical line from `y0` to `y1` (either order) in column `x`.
    fn vline(&mut self, x: u16, y0: u16, y1: u16) {
        let (a, b) = if y0 <= y1 { (y0, y1) } else { (y1, y0) };
        for y in a..=b {
            let mut bits = 0;
            if y > a {
                bits |= UP;
            }
            if y < b {
                bits |= DOWN;
            }
            if bits != 0 {
                self.mark(x, y, bits);
            }
        }
    }

    /// The box at `path`, if the diagram drew one.
    pub fn box_at(&self, path: &[PathSeg]) -> Option<&DiagramBox> {
        self.boxes.iter().find(|b| b.path == path)
    }
}

/// The glyph for a line cell with these sides connected.
pub fn line_glyph(bits: u8) -> char {
    match bits & (LEFT | RIGHT | UP | DOWN) {
        b if b == LEFT | RIGHT => '─',
        b if b == UP | DOWN => '│',
        b if b == RIGHT | DOWN => '┌',
        b if b == LEFT | DOWN => '┐',
        b if b == RIGHT | UP => '└',
        b if b == LEFT | UP => '┘',
        b if b == UP | DOWN | RIGHT => '├',
        b if b == UP | DOWN | LEFT => '┤',
        b if b == LEFT | RIGHT | DOWN => '┬',
        b if b == LEFT | RIGHT | UP => '┴',
        b if b == LEFT | RIGHT | UP | DOWN => '┼',
        b if b == LEFT || b == RIGHT => '─',
        b if b == UP || b == DOWN => '│',
        _ => ' ',
    }
}

/// What the diagram needs to know about the session beyond the chain itself.
pub struct Context<'a> {
    pub catalog: &'a CdpCatalog,
    /// The name of the open document a `Buffer` branch reads, if one is picked.
    pub picked_buffer: &'a dyn Fn(&[PathSeg]) -> Option<String>,
    /// The pale engine tag a step carries elsewhere (`[cdp]`, `[pr]`, `[air]`).
    pub badge: &'a dyn Fn(&crate::model::cdp::ProcessDef) -> String,
}

struct Builder<'a, 'b> {
    ctx: &'b Context<'a>,
    d: Diagram,
    /// Where a branch's output leaves: the end of its lane, on its signal row.
    lane_ends: HashMap<Path, (u16, u16)>,
    /// `(source, target)` of every reused output, routed once the whole picture is laid out.
    routes: Vec<(Path, (u16, u16))>,
}

/// Lays `chain` out as a diagram.
pub fn layout(chain: &CdpChain, ctx: &Context) -> Diagram {
    let mut b = Builder { ctx, d: Diagram::default(), lane_ends: HashMap::new(), routes: Vec::new() };
    let in_tag = "IN ";
    b.d.tags.push(Tag { x: 0, y: SIGNAL_ROW, text: in_tag.to_string() });
    let start = in_tag.chars().count() as u16;
    let (width, _) = b.lane(&chain.steps, &[], start, 0, true);
    let end = start + width;
    // The running audio leaves the last step and reaches OUT.
    b.d.hline(end, end + 2, SIGNAL_ROW);
    b.d.arrows.push((end + 2, SIGNAL_ROW));
    b.d.tags.push(Tag { x: end + 4, y: SIGNAL_ROW, text: "OUT".to_string() });
    b.d.grow(end + 6, SIGNAL_ROW);
    b.route_reused_outputs();
    b.d
}

impl Builder<'_, '_> {
    /// A list of steps, left to right, starting at `x` with its top row at `y`. The signal arrives
    /// at `(x, y + SIGNAL_ROW)`, if anything feeds the lane at all (`fed`): a branch that starts
    /// with a generator has nothing coming in. Returns the width and height it took.
    fn lane(&mut self, steps: &[ChainStep], parent: &[PathSeg], x: u16, y: u16, fed: bool) -> (u16, u16) {
        let mut cursor = x;
        let mut height = 1u16;
        for (i, step) in steps.iter().enumerate() {
            let mut path = parent.to_vec();
            path.push(PathSeg::Step(i));
            // Every step after the first is fed by the one before it.
            let (w, h) = self.unit(step, &path, cursor, y, fed || i > 0);
            cursor += w;
            height = height.max(h);
        }
        (cursor - x, height)
    }

    /// One step and the stack of inputs that feeds it.
    fn unit(&mut self, step: &ChainStep, path: &[PathSeg], x: u16, y: u16, fed: bool) -> (u16, u16) {
        let def = self.ctx.catalog.processes.iter().find(|p| p.key == step.process_key);
        // A combiner reads only its branches; everything else reads the running audio first.
        let reads_running = def.is_none_or(|d| d.consumes_running_buffer());
        let generates = def.is_some_and(|d| d.generates());
        let sig = y + SIGNAL_ROW;

        // (branch index, or None for the running audio) per lane, top to bottom.
        let mut lanes: Vec<Option<usize>> = Vec::new();
        // A generator has no lane for the running audio: it does not read it.
        if reads_running && !generates {
            lanes.push(None);
        }
        lanes.extend((0..step.branches.len()).map(Some));

        let split_x = x + 1;
        let mut lane_top = y;
        let mut ends: Vec<(u16, u16)> = Vec::new();
        let mut bottom = y + BOX_HEIGHT;
        for lane in &lanes {
            let lane_sig = lane_top + SIGNAL_ROW;
            match lane {
                // The running audio: nothing on it, it goes straight to the join.
                None => {
                    ends.push((x, lane_sig));
                    lane_top += BOX_HEIGHT + LANE_GAP;
                }
                Some(b) => {
                    let branch = &step.branches[*b];
                    let mut branch_path = path.to_vec();
                    branch_path.push(PathSeg::Branch(*b));
                    let mut content_x = x + LANE_LEAD;
                    // A branch that starts with a generator reads nothing, so it needs no feed.
                    let first_generates = branch.steps.first().is_some_and(|s| {
                        self.ctx.catalog.processes.iter().any(|p| p.key == s.process_key && p.generates())
                    });
                    match &branch.source {
                        _ if first_generates => {}
                        BranchSource::Tap => {
                            // Split off the audio arriving at this step.
                            self.d.vline(split_x, sig, lane_sig);
                            self.d.hline(split_x, content_x, lane_sig);
                        }
                        BranchSource::Buffer => {
                            let name = (self.ctx.picked_buffer)(&branch_path)
                                .unwrap_or_else(|| "(none picked)".to_string());
                            let text = format!("file: {}", truncate(&name, 18));
                            let w = text.chars().count() as u16;
                            self.d.tags.push(Tag { x: content_x, y: lane_sig, text });
                            self.d.grow(content_x + w, lane_sig);
                            content_x += w + 1;
                            self.d.hline(content_x - 1, content_x, lane_sig);
                        }
                        BranchSource::From(source) => {
                            // Routed after everything is placed, up into this lane's start.
                            self.routes.push((source.clone(), (x + 1, lane_sig)));
                            self.d.hline(x + 1, content_x, lane_sig);
                        }
                    }
                    let (w, h) = self.lane(&branch.steps, &branch_path, content_x, lane_top, !first_generates);
                    let end = (content_x + w, lane_sig);
                    self.lane_ends.insert(branch_path, end);
                    ends.push(end);
                    let lane_height = h.max(2);
                    bottom = bottom.max(lane_top + lane_height);
                    lane_top += lane_height + LANE_GAP;
                }
            }
        }

        // Everything meets in the join column, then enters the box on its signal row.
        let join_x = ends.iter().map(|(ex, _)| *ex).max().unwrap_or(x) + 1;
        for &(ex, ey) in &ends {
            self.d.hline(ex, join_x, ey);
            if ey != sig {
                self.d.vline(join_x, sig, ey);
            }
        }
        let box_x = join_x + 2;
        if generates {
            // A generator reads nothing. Audio that reaches one stops at a cross in front of
            // it; with nothing coming in, nothing is drawn.
            if fed {
                self.d.hline(x, box_x - 2, sig);
                self.d.tags.push(Tag { x: box_x - 1, y: sig, text: "\u{2573}".to_string() });
            }
        } else {
            self.d.hline(join_x, box_x - 1, sig);
            self.d.arrows.push((box_x - 1, sig));
            if lanes.first() == Some(&None) {
                // The running audio's own lane.
                self.d.hline(x, join_x, sig);
            } else {
                // A combiner: the audio arriving here feeds its first lane.
                self.d.hline(x, split_x, sig);
            }
        }

        let (title, preset) = self.box_text(step, def);
        let text_width = title.chars().count().max(preset.chars().count()) as u16;
        let width = text_width + 4;
        self.d.boxes.push(DiagramBox {
            path: path.to_vec(),
            x: box_x,
            y,
            width,
            title,
            preset,
            generates,
        });
        self.d.grow(box_x + width, y + BOX_HEIGHT - 1);
        // The running audio leaves the box on its signal row; the next unit picks it up.
        self.d.hline(box_x + width - 1, box_x + width, sig);
        (box_x + width - x, bottom.max(y + BOX_HEIGHT) - y)
    }

    fn box_text(&self, step: &ChainStep, def: Option<&crate::model::cdp::ProcessDef>) -> (String, String) {
        let Some(def) = def else {
            return (truncate(&step.process_key, MAX_TEXT_WIDTH), String::new());
        };
        let mut badge = (self.ctx.badge)(def);
        if def.generates() {
            badge.push_str(" gen");
        }
        let room = MAX_TEXT_WIDTH.saturating_sub(badge.chars().count() + 1).max(4);
        let title = format!("{} {}", truncate(&def.title, room), badge);
        let preset = def
            .preset_param
            .and_then(|pp| match (step.values.get(pp), def.params.get(pp).map(|p| &p.kind)) {
                (
                    Some(crate::model::cdp::ParamValue::Choice(i)),
                    Some(crate::model::cdp::ParamKind::Choice { options, .. }),
                ) => options.get(*i).cloned(),
                _ => None,
            })
            .map(|p| truncate(&p, MAX_TEXT_WIDTH))
            .unwrap_or_default();
        (title, preset)
    }

    /// Draws every reused output as a line: down from where it leaves its source, along a row
    /// of its own below the picture, and up into the lane that reads it.
    fn route_reused_outputs(&mut self) {
        let routes = std::mem::take(&mut self.routes);
        let base = self.d.height + 1;
        for (k, (source, (tx, ty))) in routes.into_iter().enumerate() {
            let start = match source.last() {
                Some(PathSeg::Branch(_)) => self.lane_ends.get(&source).copied(),
                _ => self
                    .d
                    .box_at(&source)
                    .map(|b| (b.x + b.width / 2, b.y + BOX_HEIGHT - 1)),
            };
            let Some((sx, sy)) = start else { continue };
            let row = base + k as u16;
            self.d.vline(sx, sy, row);
            self.d.hline(sx, tx, row);
            self.d.vline(tx, row, ty);
        }
    }
}

/// `text` cut to `max` characters, with an ellipsis where it was cut.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// An arrow key, for [`nearest_in_direction`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// The box to move to from `from` when an arrow is pressed: the one visually nearest in that
/// direction. A box **in line** with the current one (its rows overlap for Left/Right, its
/// columns for Up/Down) wins over one off to the side, so Right follows the row it is on; only
/// when nothing is in line does the nearest box off to the side count, with sideways drift
/// costing twice as much as distance ahead. `None` at an edge.
pub fn nearest_in_direction(diagram: &Diagram, from: &[PathSeg], dir: Direction) -> Option<Path> {
    let here = diagram.box_at(from)?;
    let (hx, hy) = here.center();
    let horizontal = matches!(dir, Direction::Left | Direction::Right);
    let in_line = |b: &DiagramBox| {
        if horizontal {
            b.y < here.y + BOX_HEIGHT && here.y < b.y + BOX_HEIGHT
        } else {
            b.x < here.x + here.width && here.x < b.x + b.width
        }
    };
    let ahead_of = |b: &DiagramBox| {
        let (cx, cy) = b.center();
        let (dx, dy) = (cx - hx, cy - hy);
        match dir {
            Direction::Right => (dx, dy),
            Direction::Left => (-dx, dy),
            Direction::Down => (dy, dx),
            Direction::Up => (-dy, dx),
        }
    };
    let candidates: Vec<&DiagramBox> = diagram
        .boxes
        .iter()
        .filter(|b| b.path != from && ahead_of(b).0 > 0)
        .collect();
    let lined: Vec<&&DiagramBox> = candidates.iter().filter(|b| in_line(b)).collect();
    let pick = if lined.is_empty() {
        candidates.iter().min_by_key(|b| {
            let (ahead, aside) = ahead_of(b);
            ahead + 2 * aside.abs()
        })
    } else {
        lined.into_iter().min_by_key(|b| ahead_of(b).0)
    };
    pick.map(|b| b.path.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::cdp::chain::Branch;
    use crate::model::cdp::PathSeg::{Branch as B, Step as S};

    fn catalog() -> CdpCatalog {
        CdpCatalog::load(None).0
    }

    fn step(catalog: &CdpCatalog, key: &str) -> ChainStep {
        let def = catalog.find(key).unwrap_or_else(|| panic!("{key}"));
        ChainStep::new(key, def.params.iter().map(|p| p.kind.default_value()).collect())
    }

    fn chain(steps: Vec<ChainStep>) -> CdpChain {
        CdpChain { name: "t".into(), steps, bank: Default::default(), output: Default::default() }
    }

    fn draw(chain: &CdpChain, catalog: &CdpCatalog) -> Diagram {
        let ctx = Context {
            catalog,
            picked_buffer: &|_| Some("take.wav".to_string()),
            badge: &|d| format!("[{:?}]", d.backend()),
        };
        layout(chain, &ctx)
    }

    fn overlaps(a: &DiagramBox, b: &DiagramBox) -> bool {
        a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + BOX_HEIGHT && b.y < a.y + BOX_HEIGHT
    }

    fn assert_no_overlap(d: &Diagram) {
        for (i, a) in d.boxes.iter().enumerate() {
            for b in &d.boxes[i + 1..] {
                assert!(!overlaps(a, b), "{:?} overlaps {:?}", a.path, b.path);
            }
        }
    }

    /// A plain chain is one straight line: every box on the same row, left to right in run order,
    /// with the signal entering each one.
    #[test]
    fn a_plain_chain_runs_left_to_right_on_one_row() {
        let c = catalog();
        let d = draw(&chain(vec![step(&c, "blur_avrg"), step(&c, "praat_distortion_hard_clip")]), &c);
        assert_eq!(d.boxes.len(), 2);
        assert_eq!(d.boxes[0].y, d.boxes[1].y);
        assert!(d.boxes[0].x + d.boxes[0].width < d.boxes[1].x);
        for b in &d.boxes {
            assert!(d.arrows.contains(&(b.x - 1, b.y + SIGNAL_ROW)), "{:?} is fed", b.path);
        }
        assert!(d.tags.iter().any(|t| t.text == "IN ") && d.tags.iter().any(|t| t.text == "OUT"));
    }

    /// A multi-input step stacks its extra inputs under it, each lane's steps drawn as boxes, and
    /// every lane's line reaches the join in front of the step.
    #[test]
    fn a_multi_input_step_stacks_its_inputs_under_it() {
        let c = catalog();
        let mut mixer = step(&c, "praat_spatial_surround_stereo_mixer");
        let generator = step(&c, "praat_generative_synthesis_risset_s_mutations");
        mixer.branches = vec![
            Branch { source: BranchSource::Tap, steps: vec![generator.clone()] },
            Branch { source: BranchSource::Tap, steps: vec![generator] },
        ];
        let d = draw(&chain(vec![step(&c, "blur_avrg"), mixer]), &c);
        assert_no_overlap(&d);
        let m = d.box_at(&[S(1)]).expect("mixer box");
        let g0 = d.box_at(&[S(1), B(0), S(0)]).expect("first generator");
        let g1 = d.box_at(&[S(1), B(1), S(0)]).expect("second generator");
        assert!(g0.y > m.y && g1.y > g0.y, "inputs stack downward under the step");
        assert!(g0.x + g0.width < m.x && g1.x + g1.width < m.x, "and sit before it");
        assert!(g0.generates && g0.title.contains("gen"));
        // The join column carries the lower lanes up to the step's signal row.
        let join = m.x - 2;
        assert!(d.lines.get(&(join, g1.y + SIGNAL_ROW)).is_some_and(|b| b & UP != 0));
    }

    /// A generator reads nothing: one heading a branch has no line coming in at all, and audio
    /// that reaches one mid-chain ends at a cross in front of it instead of an arrow into it.
    #[test]
    fn nothing_flows_into_a_generator() {
        let c = catalog();
        let mut mixer = step(&c, "praat_spatial_surround_stereo_mixer");
        let generator = step(&c, "praat_generative_synthesis_risset_s_mutations");
        mixer.branches = vec![Branch { source: BranchSource::Tap, steps: vec![generator.clone()] }];
        let d = draw(&chain(vec![step(&c, "blur_avrg"), generator, mixer]), &c);
        let in_branch = d.box_at(&[S(2), B(0), S(0)]).unwrap();
        let sig = in_branch.y + SIGNAL_ROW;
        assert!(!d.arrows.contains(&(in_branch.x - 1, sig)));
        let lane_start = d.box_at(&[S(2)]).unwrap().x.min(in_branch.x).saturating_sub(40);
        assert!(
            (lane_start..in_branch.x).all(|x| !d.lines.get(&(x, sig)).is_some_and(|b| b & (LEFT | RIGHT) != 0)),
            "nothing feeds a branch that starts with a generator"
        );
        let mid = d.box_at(&[S(1)]).unwrap();
        assert!(!d.arrows.contains(&(mid.x - 1, mid.y + SIGNAL_ROW)));
        assert!(d.tags.iter().any(|t| t.text == "\u{2573}" && t.x == mid.x - 1), "the incoming audio stops at a cross");
    }

    /// A combiner reads only its branches: its first leg is the top lane, the running audio splits
    /// into the legs, and there is no straight line past them into the combiner.
    #[test]
    fn a_combiner_splits_the_audio_into_its_legs() {
        let c = catalog();
        let mut mix = step(&c, crate::model::cdp::native::MIXER_KEY);
        mix.branches = vec![
            Branch { source: BranchSource::Tap, steps: vec![step(&c, "blur_avrg")] },
            Branch { source: BranchSource::Tap, steps: Vec::new() },
        ];
        let d = draw(&chain(vec![mix]), &c);
        assert_no_overlap(&d);
        let leg = d.box_at(&[S(0), B(0), S(0)]).expect("leg A's step");
        let combiner = d.box_at(&[S(0)]).expect("combiner");
        assert_eq!(leg.y, combiner.y, "leg A is the top lane");
    }

    /// A picked file is named where its branch starts, and a reused output is a line that leaves
    /// its source box and ends at the lane that reads it.
    #[test]
    fn sources_are_named_or_routed() {
        let c = catalog();
        let mut dual = step(&c, "distort_interact_1");
        dual.branches = vec![Branch { source: BranchSource::From(vec![S(0)]), steps: Vec::new() }];
        let mut d2 = step(&c, "distort_interact_1");
        d2.branches = vec![Branch { source: BranchSource::Buffer, steps: Vec::new() }];
        let d = draw(&chain(vec![step(&c, "blur_avrg"), dual, d2]), &c);
        assert!(d.tags.iter().any(|t| t.text == "file: take.wav"));
        let source = d.box_at(&[S(0)]).unwrap();
        let below = (source.x + source.width / 2, source.y + BOX_HEIGHT);
        assert!(d.lines.get(&below).is_some_and(|b| b & (UP | DOWN) != 0), "the route leaves the source box");
        // It reaches the lane under the step that reads it.
        let reader = d.box_at(&[S(1)]).unwrap();
        let lane_sig = reader.y + BOX_HEIGHT + LANE_GAP + SIGNAL_ROW;
        assert!(
            d.lines.iter().any(|(&(_, y), &b)| y == lane_sig && b & UP != 0),
            "the route comes up into the lane"
        );
    }

    /// The cursor goes to the box nearest in the pressed direction, and nowhere past an edge.
    #[test]
    fn the_cursor_moves_to_the_nearest_box_in_each_direction() {
        let c = catalog();
        let mut mixer = step(&c, "praat_spatial_surround_stereo_mixer");
        let generator = step(&c, "praat_generative_synthesis_risset_s_mutations");
        mixer.branches = vec![Branch { source: BranchSource::Tap, steps: vec![generator] }];
        let d = draw(&chain(vec![step(&c, "blur_avrg"), mixer, step(&c, "praat_distortion_hard_clip")]), &c);
        let go = |from: &[PathSeg], dir| nearest_in_direction(&d, from, dir);
        // From the first step, Right reaches the next box along the row: the mixer's input
        // lane sits lower, so the step straight ahead is preferred.
        assert_eq!(go(&[S(0)], Direction::Right), Some(vec![S(1)]));
        assert_eq!(go(&[S(1)], Direction::Right), Some(vec![S(2)]));
        assert_eq!(go(&[S(1)], Direction::Down), Some(vec![S(1), B(0), S(0)]));
        assert_eq!(go(&[S(1), B(0), S(0)], Direction::Up).as_deref().map(|p| p.len()), Some(1));
        assert_eq!(go(&[S(2)], Direction::Right), None, "nothing past the last box");
        assert_eq!(go(&[S(0)], Direction::Left), None);
    }

    #[test]
    fn long_names_are_cut_with_an_ellipsis() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("a very long process name", 10), "a very lo…");
        assert_eq!(truncate("a very long process name", 10).chars().count(), 10);
    }

    #[test]
    fn junctions_come_from_the_sides_that_meet() {
        assert_eq!(line_glyph(LEFT | RIGHT), '─');
        assert_eq!(line_glyph(UP | DOWN | RIGHT), '├');
        assert_eq!(line_glyph(LEFT | UP), '┘');
        assert_eq!(line_glyph(LEFT | RIGHT | UP | DOWN), '┼');
    }
}
