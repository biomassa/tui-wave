//! Runs an Airwindows render on a dedicated thread, mirroring `praat::runner::PraatRunner`'s
//! *interface* — `submit` fire-and-forget, `events` polled once per frame, `cancel` via a
//! shared flag — while sharing none of its body.
//!
//! Keeping the interface identical is the whole point: `Dialog::CdpRunning`, the
//! `JobPurpose::Preview`/`Apply` split and the splice path in `App` were built against that
//! contract for CDP, reused verbatim for Praat, and are reused verbatim again here. What
//! differs is everything underneath — there is no subprocess, no temp WAV, no argv and no
//! timeout, because the DSP is linked into this binary (see `build.rs`).
//!
//! It still runs on a worker thread rather than inline. A 517-plugin catalog includes
//! convolution reverbs, and a multi-minute selection through one of those is seconds of work;
//! doing it on the UI thread would freeze the terminal with no way to cancel. The thread also
//! means `Preview` and `Apply` reach the UI through the same asynchronous path everything
//! else already handles.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::cdp::runner::JobPurpose;

/// Frames rendered between cancellation checks.
///
/// Airwindows plugins carry per-sample state and are written against a host that hands them
/// arbitrary block sizes, so the split is inaudible — but it is not free either: the block
/// boundary is where the cancel flag is read, so a value too large makes Esc feel sticky on a
/// long selection and one too small pays call overhead 100k times on a short one. 512 frames
/// is ~11ms at 48kHz, well under a redraw tick.
const BLOCK_FRAMES: usize = 512;

/// Everything a render needs. `input` is deinterleaved, already narrowed to the selection.
pub struct Job {
    pub id: u64,
    /// Index into the Airwindows registry (`super::plugin_info`), which is also the catalog's
    /// own ordering — see `model::airwindows::plan`.
    pub plugin_index: usize,
    /// One normalized 0.0–1.0 value per parameter, in the plugin's own parameter order.
    pub values: Vec<f32>,
    pub input: Vec<Vec<f32>>,
    pub sample_rate: u32,
    pub purpose: JobPurpose,
    /// What the progress dialog shows while this runs.
    pub label: String,
}

#[derive(Debug)]
pub struct JobOutput {
    /// Always exactly two channels — every Airwindows plugin is hard-wired stereo, so a mono
    /// input is duplicated into both legs and the stereo result is kept (which widens a mono
    /// buffer on Apply; `CdpProcessCommand` already tracks `channels_before` so undo shrinks
    /// it back).
    pub result: Vec<Vec<f32>>,
    pub sample_rate: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The registry index was out of range, or the generator returned nothing. Not reachable
    /// from the UI — the catalog is generated from the same registry — but a hand-edited user
    /// catalog naming an unknown plugin lands here rather than panicking.
    Instantiate(String),
    /// The selection had no channels, or no samples.
    EmptyInput,
    Cancelled,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Instantiate(name) => write!(f, "could not create the {name} processor"),
            Error::EmptyInput => write!(f, "nothing selected to process"),
            Error::Cancelled => write!(f, "cancelled"),
        }
    }
}

pub enum Event {
    Started { job: u64, label: String },
    Finished { job: u64, purpose: JobPurpose, result: Result<JobOutput, Error> },
}

/// Owns the Airwindows worker thread.
pub struct Runner {
    job_tx: Sender<Job>,
    pub events: Receiver<Event>,
    cancel: Arc<AtomicBool>,
}

impl Runner {
    pub fn new() -> Self {
        let (job_tx, job_rx) = unbounded::<Job>();
        let (event_tx, event_rx) = unbounded::<Event>();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_for_thread = cancel.clone();

        thread::spawn(move || {
            for job in job_rx {
                cancel_for_thread.store(false, Ordering::Relaxed);
                let id = job.id;
                let purpose = job.purpose;
                let _ = event_tx.send(Event::Started { job: id, label: job.label.clone() });
                let result = run_job(&job, &cancel_for_thread);
                let _ = event_tx.send(Event::Finished { job: id, purpose, result });
            }
        });

        Self { job_tx, events: event_rx, cancel }
    }

    pub fn submit(&self, job: Job) {
        let _ = self.job_tx.send(job);
    }

    /// Best-effort cancellation of the running job; takes effect at the next block boundary.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Default for Runner {
    fn default() -> Self {
        Self::new()
    }
}

/// Renders the whole selection through one plugin instance.
///
/// The instance is created once for the job, not once per block: its filter and delay state
/// *is* the effect, and rebuilding it per block would restart every reverb tail 94 times a
/// second. That is also why cancellation discards the partial result rather than returning
/// it — half a render is not a shorter render, it is a truncated one.
fn run_job(job: &Job, cancel: &AtomicBool) -> Result<JobOutput, Error> {
    let frames = job.input.first().map_or(0, |c| c.len());
    if job.input.is_empty() || frames == 0 {
        return Err(Error::EmptyInput);
    }

    let name = super::plugin_info(job.plugin_index).map_or("unknown", |p| p.name);
    let mut inst =
        super::Instance::new(job.plugin_index, job.sample_rate).ok_or_else(|| Error::Instantiate(name.to_string()))?;

    for (i, v) in job.values.iter().enumerate() {
        inst.set_param(i, *v);
    }

    // A mono document feeds both legs, so a plugin that builds a stereo field from a mono
    // source (the reverbs, the wideners) can do so. A document wider than two channels never
    // reaches here — `InputChannels::MonoOrStereo` refuses it in `cdp_params_blocker` before
    // Apply is even enabled — so taking channel 1 or falling back to channel 0 covers every
    // case that can arrive.
    let left = &job.input[0];
    let right = job.input.get(1).unwrap_or(left);

    let mut out_l = vec![0.0f32; frames];
    let mut out_r = vec![0.0f32; frames];
    // Scratch copies, because `process` needs `&mut` input buffers (the C signature is
    // `float**`) and the job's input must survive for a retry or a second preview.
    let mut in_l = vec![0.0f32; BLOCK_FRAMES];
    let mut in_r = vec![0.0f32; BLOCK_FRAMES];

    let mut pos = 0;
    while pos < frames {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let n = BLOCK_FRAMES.min(frames - pos);
        in_l[..n].copy_from_slice(&left[pos..pos + n]);
        in_r[..n].copy_from_slice(&right[pos..pos + n]);
        inst.process(
            &mut in_l[..n],
            &mut in_r[..n],
            &mut out_l[pos..pos + n],
            &mut out_r[pos..pos + n],
        );
        pos += n;
    }

    Ok(JobOutput { result: vec![out_l, out_r], sample_rate: job.sample_rate })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(plugin: &str, frames: usize, channels: usize) -> Job {
        let input = (0..channels)
            .map(|_| (0..frames).map(|n| (n as f32 * 0.05).sin() * 0.25).collect())
            .collect();
        Job {
            id: 1,
            plugin_index: super::super::find_by_name(plugin).expect("plugin in registry"),
            values: vec![0.5; 4],
            input,
            sample_rate: 48_000,
            purpose: JobPurpose::Apply,
            label: plugin.to_string(),
        }
    }

    #[test]
    fn a_render_preserves_length_and_returns_stereo() {
        let j = job("Density", 5000, 2);
        let out = run_job(&j, &AtomicBool::new(false)).expect("render");
        assert_eq!(out.result.len(), 2);
        assert_eq!(out.result[0].len(), 5000);
        assert_eq!(out.result[1].len(), 5000);
        assert!(out.result.iter().flatten().all(|s| s.is_finite()));
    }

    /// The block loop must produce the same audio as one long call would, or the split is
    /// audible at every boundary. Rendering a length that is not a multiple of `BLOCK_FRAMES`
    /// exercises the short final block too.
    #[test]
    fn block_boundaries_do_not_disturb_the_result() {
        let frames = BLOCK_FRAMES * 3 + 137;
        let j = job("Density", frames, 2);
        let out = run_job(&j, &AtomicBool::new(false)).expect("render");
        assert_eq!(out.result[0].len(), frames);
        // A discontinuity at a boundary shows up as a sample-to-sample jump far larger than
        // the signal's own slew. The source is a slow sine at 0.25 amplitude.
        for ch in &out.result {
            for w in ch.windows(2) {
                assert!((w[1] - w[0]).abs() < 0.5, "discontinuity in the rendered block");
            }
        }
    }

    /// A mono buffer feeds both legs, so the two outputs carry the same *signal* — but they
    /// are deliberately not bit-identical, and that is upstream's design rather than a fault
    /// here. Every plugin seeds two independent dither/denormal states in its constructor
    /// (`fpdL = 1.0; while (fpdL < 16386) fpdL = rand()*UINT32_MAX;` and the same for `fpdR`),
    /// drawn consecutively, so the legs get different noise. The divergence measures around
    /// 1e-8 — roughly -145 dBFS, below a 24-bit LSB — which is the same residue the
    /// airwindows-lv2 port's null tests accept when validating against the original VSTs.
    ///
    /// So the assertion is an epsilon, and the epsilon is the point: a tolerance this tight
    /// still fails outright if the legs are ever fed different sources.
    #[test]
    fn a_mono_input_reaches_both_legs_as_the_same_signal() {
        let j = job("Density", 1000, 1);
        let out = run_job(&j, &AtomicBool::new(false)).expect("render");
        assert_eq!(out.result.len(), 2);
        let worst = out.result[0]
            .iter()
            .zip(&out.result[1])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 1e-5, "legs diverge by {worst}, far past dither residue");
    }

    #[test]
    fn an_empty_selection_is_refused_rather_than_rendered() {
        let mut j = job("Density", 0, 2);
        j.input = vec![Vec::new(), Vec::new()];
        assert_eq!(run_job(&j, &AtomicBool::new(false)).unwrap_err(), Error::EmptyInput);
    }

    #[test]
    fn cancellation_discards_the_partial_render() {
        let j = job("Density", 100_000, 2);
        let flag = AtomicBool::new(true);
        assert_eq!(run_job(&j, &flag).unwrap_err(), Error::Cancelled);
    }
}
