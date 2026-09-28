use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

use crossbeam_channel::{unbounded, Sender};
use rodio::{DeviceSinkBuilder, Player};

use super::source::DocumentSource;
use super::stream_source::StreamedSource;
use crate::model::dsp::Fold;
use crate::model::stream::StreamedSamples;

/// Where the audio thread gets its samples.
///
/// The two arms differ only in how a source is built for them — everything else (the command
/// protocol, the position atomic, loop handling) is shared, which is what keeps a streamed
/// buffer's transport behaving exactly like an ordinary one from the UI's side.
enum PlaybackData {
    /// A fully-loaded document. The engine owns a second copy of its samples. For a lazy engine
    /// (`try_new_lazy`) the copy exists only from Play until playback stops, and is empty
    /// otherwise.
    Resident(Arc<Vec<Vec<f32>>>),
    /// A disk-backed document. The engine owns only a handle; each play spawns a reader thread
    /// that streams blocks in (see `stream_source`), so playback costs a bounded ring buffer
    /// rather than a second copy of a 30GB file.
    Streamed(Arc<StreamedSamples>),
}

enum AudioCmd {
    Play {
        from_frame: usize,
        loop_start: Option<usize>,
        loop_end: Option<usize>,
    },
    Pause,
    Stop,
    Seek {
        frame: usize,
        loop_start: Option<usize>,
        loop_end: Option<usize>,
    },
    Reload(Vec<Vec<f32>>),
    /// Drop the resident copy, unless a source is still queued. Only a lazy engine sends it.
    Release,
    SetFold(Fold),
}

/// Owns the audio device and playback thread. The UI thread only ever talks to this
/// through `cmd_tx` (fire-and-forget) and reads `position`/`playing` atomics — it never
/// blocks on audio, and audio never blocks on the terminal.
pub struct AudioEngine {
    cmd_tx: Sender<AudioCmd>,
    pub position: Arc<AtomicUsize>,
    pub playing: Arc<AtomicBool>,
    /// Holds a copy of the audio only while it plays: see [`Self::try_new_lazy`].
    lazy: bool,
    /// UI-side record of whether the engine thread has (or has been sent) the current audio.
    loaded: Cell<bool>,
    /// UI-side record of a Play not yet followed by a Pause. Kept here, not read from the
    /// `playing` atomic, because that atomic changes only when the audio thread gets to the
    /// command, and an edit can arrive in between.
    active: Cell<bool>,
}

/// Builds the right source for `data` and hands it to `player`. Returns the stop flag for the
/// reader thread it started, or `None` for resident data, which has no thread behind it.
///
/// Play and Seek build a source identically; factoring it out is what keeps a change to one from
/// having to be remembered in the other (they had already been two copies of the same six lines).
#[allow(clippy::too_many_arguments)]
fn append_source(
    player: &Player,
    data: &PlaybackData,
    sample_rate: u32,
    from_frame: usize,
    position: &Arc<AtomicUsize>,
    playing: &Arc<AtomicBool>,
    loop_start: Option<usize>,
    loop_end: Option<usize>,
    fold: Fold,
) -> Option<Arc<AtomicBool>> {
    match data {
        PlaybackData::Resident(channels) => {
            player.append(DocumentSource::new_looped(
                channels.clone(),
                sample_rate,
                from_frame,
                position.clone(),
                playing.clone(),
                loop_start,
                loop_end,
                fold,
            ));
            None
        }
        PlaybackData::Streamed(stream) => {
            let source = StreamedSource::start(
                stream.clone(),
                sample_rate,
                from_frame,
                position.clone(),
                playing.clone(),
                loop_start,
                loop_end,
                fold,
            );
            let stop = source.stop_handle();
            player.append(source);
            Some(stop)
        }
    }
}

impl AudioEngine {
    /// Spawns the audio thread. Returns `None` if no output device is available — callers
    /// should treat that as "playback disabled," not a fatal error, since editing/viewing
    /// a waveform shouldn't require a working audio device.
    pub fn try_new(channels: Vec<Vec<f32>>, sample_rate: u32) -> Option<Self> {
        Self::spawn(PlaybackData::Resident(Arc::new(channels)), sample_rate, false)
    }

    /// A resident engine that holds no copy of the audio until it plays.
    ///
    /// `try_new` keeps a second copy of the document for its whole life and gets a new one on
    /// every edit, so a 4GB buffer cost 8GB even when nothing was playing. This engine gets the
    /// audio in [`Self::load_if_needed`] just before Play, and drops it when playback is paused
    /// or has ended ([`Self::audio_changed`]).
    pub fn try_new_lazy(sample_rate: u32) -> Option<Self> {
        Self::spawn(PlaybackData::Resident(Arc::new(Vec::new())), sample_rate, true)
    }

    /// The streamed counterpart to [`Self::try_new`]: plays a disk-backed document without ever
    /// holding it.
    ///
    /// Takes a handle rather than samples, which is the whole reason playback is possible on a
    /// buffer that is read-only for everything else — the objection to editing a 30GB take is
    /// that every `Command` stores a copy for undo, and the objection to playing it *was* that
    /// `try_new` above takes an owned `Vec<Vec<f32>>`. Streaming the audio in retires only the
    /// second of those.
    pub fn try_new_streamed(stream: Arc<StreamedSamples>, sample_rate: u32) -> Option<Self> {
        Self::spawn(PlaybackData::Streamed(stream), sample_rate, false)
    }

    fn spawn(data: PlaybackData, sample_rate: u32, lazy: bool) -> Option<Self> {
        // Probe device availability on the calling thread so `try_new` can report failure
        // synchronously instead of the caller having to poll the spawned thread. Silence
        // log-on-drop first — otherwise dropping this throwaway probe immediately prints a
        // warning to stderr, which corrupts the raw-mode terminal.
        match DeviceSinkBuilder::open_default_sink() {
            Ok(mut probe) => probe.log_on_drop(false),
            Err(_) => return None,
        }

        let (cmd_tx, cmd_rx) = unbounded::<AudioCmd>();
        let position = Arc::new(AtomicUsize::new(0));
        let playing = Arc::new(AtomicBool::new(false));

        let position_for_thread = position.clone();
        let playing_for_thread = playing.clone();

        thread::spawn(move || {
            let Ok(mut device_sink) = DeviceSinkBuilder::open_default_sink() else {
                return;
            };
            device_sink.log_on_drop(false);
            let player = Player::connect_new(device_sink.mixer());
            let mut data = data;
            // Applied to every source built from here on. An already-playing source keeps the
            // fold it captured, exactly as it keeps the sample data it captured — the same rule
            // `Reload` has always followed, so a channel-count change never alters the levels of
            // a pass already underway.
            let mut fold = Fold::default();
            // The reader thread behind the source currently on the player, if it is a streamed
            // one. Cancelled before every `player.clear()`: rodio decides when a cleared source
            // is actually dropped, and until it is, the outgoing reader would go on pulling
            // blocks off the same file handle the incoming one needs.
            let mut reader_stop: Option<Arc<AtomicBool>> = None;
            let stop_reader = |stop: &mut Option<Arc<AtomicBool>>| {
                if let Some(flag) = stop.take() {
                    flag.store(true, Ordering::Relaxed);
                }
            };

            for cmd in cmd_rx {
                match cmd {
                    AudioCmd::Reload(channels) => {
                        // A streamed engine has nothing to reload: its samples were never copied
                        // in, and a channel-map edit is picked up by the next read. Ignoring the
                        // command rather than switching storage keeps a stray reload (a streamed
                        // document's `channels` is empty) from silently replacing a playable
                        // engine with an empty one.
                        if let PlaybackData::Resident(_) = data {
                            data = PlaybackData::Resident(Arc::new(channels));
                        }
                    }
                    // Ignored while a source is queued: a Play sent just before an edit may
                    // not have been handled when the edit asked for the release, and a Seek
                    // during that playback still needs the data.
                    AudioCmd::Release => {
                        if player.empty() {
                            if let PlaybackData::Resident(_) = data {
                                data = PlaybackData::Resident(Arc::new(Vec::new()));
                            }
                        }
                    }
                    AudioCmd::Play {
                        from_frame,
                        loop_start,
                        loop_end,
                    } => {
                        stop_reader(&mut reader_stop);
                        player.clear();
                        reader_stop = append_source(
                            &player,
                            &data,
                            sample_rate,
                            from_frame,
                            &position_for_thread,
                            &playing_for_thread,
                            loop_start,
                            loop_end,
                            fold,
                        );
                        player.play();
                        playing_for_thread.store(true, Ordering::Relaxed);
                    }
                    AudioCmd::SetFold(new_fold) => {
                        fold = new_fold;
                    }
                    AudioCmd::Pause => {
                        // A lazy engine clears the source too, so its copy of the audio is
                        // freed. Nothing resumes a paused source: Play always builds a new one.
                        if lazy {
                            stop_reader(&mut reader_stop);
                            player.clear();
                            if let PlaybackData::Resident(_) = data {
                                data = PlaybackData::Resident(Arc::new(Vec::new()));
                            }
                        } else {
                            player.pause();
                        }
                        playing_for_thread.store(false, Ordering::Relaxed);
                    }
                    AudioCmd::Stop => {
                        stop_reader(&mut reader_stop);
                        player.clear();
                        playing_for_thread.store(false, Ordering::Relaxed);
                        position_for_thread.store(0, Ordering::Relaxed);
                    }
                    AudioCmd::Seek {
                        frame,
                        loop_start,
                        loop_end,
                    } => {
                        let was_playing = playing_for_thread.load(Ordering::Relaxed);
                        stop_reader(&mut reader_stop);
                        player.clear();
                        position_for_thread.store(frame, Ordering::Relaxed);
                        if was_playing {
                            reader_stop = append_source(
                                &player,
                                &data,
                                sample_rate,
                                frame,
                                &position_for_thread,
                                &playing_for_thread,
                                loop_start,
                                loop_end,
                                fold,
                            );
                            player.play();
                        }
                    }
                }
            }
            stop_reader(&mut reader_stop);
        });

        Some(Self {
            cmd_tx,
            position,
            playing,
            lazy,
            loaded: Cell::new(!lazy),
            active: Cell::new(false),
        })
    }

    /// Sends the audio to a lazy engine that does not have it. Call before any Play. Does
    /// nothing for other engines, which were given their audio at construction.
    pub fn load_if_needed(&self, channels: &[Vec<f32>]) {
        if self.lazy && !self.loaded.get() {
            let _ = self.cmd_tx.send(AudioCmd::Reload(channels.to_vec()));
            self.loaded.set(true);
        }
    }

    /// The document's audio changed. While playing, the engine gets the new audio, so a Seek
    /// plays it. Otherwise a lazy engine drops its copy and gets the audio again at the next
    /// Play; a non-lazy engine gets the new audio at once, as it always did.
    pub fn audio_changed(&self, channels: &[Vec<f32>]) {
        if !self.lazy || (self.active.get() && self.is_playing()) {
            let _ = self.cmd_tx.send(AudioCmd::Reload(channels.to_vec()));
        } else {
            let _ = self.cmd_tx.send(AudioCmd::Release);
            self.loaded.set(false);
        }
    }

    pub fn play(&self, from_frame: usize) {
        debug_assert!(self.loaded.get(), "a lazy engine needs load_if_needed before Play");
        self.active.set(true);
        let _ = self
            .cmd_tx
            .send(AudioCmd::Play { from_frame, loop_start: None, loop_end: None });
    }

    pub fn play_looped(&self, from_frame: usize, loop_start: usize, loop_end: usize) {
        debug_assert!(self.loaded.get(), "a lazy engine needs load_if_needed before Play");
        self.active.set(true);
        let _ = self.cmd_tx.send(AudioCmd::Play {
            from_frame,
            loop_start: Some(loop_start),
            loop_end: Some(loop_end),
        });
    }

    /// Plays once (no wraparound) but stops at `end_frame` instead of the end of the file —
    /// `loop_start: None` with `loop_end: Some` is exactly what `DocumentSource::next`
    /// already treats as "stop here," it just wasn't exposed as its own entry point before.
    /// Used to keep playback from continuing past a selection when loop playback is off.
    pub fn play_bounded(&self, from_frame: usize, end_frame: usize) {
        debug_assert!(self.loaded.get(), "a lazy engine needs load_if_needed before Play");
        self.active.set(true);
        let _ = self.cmd_tx.send(AudioCmd::Play {
            from_frame,
            loop_start: None,
            loop_end: Some(end_frame),
        });
    }

    pub fn pause(&self) {
        self.active.set(false);
        if self.lazy {
            self.loaded.set(false);
        }
        let _ = self.cmd_tx.send(AudioCmd::Pause);
    }

    pub fn seek(&self, frame: usize) {
        let _ = self
            .cmd_tx
            .send(AudioCmd::Seek { frame, loop_start: None, loop_end: None });
    }

    pub fn seek_looped(&self, frame: usize, loop_start: usize, loop_end: usize) {
        let _ = self.cmd_tx.send(AudioCmd::Seek {
            frame,
            loop_start: Some(loop_start),
            loop_end: Some(loop_end),
        });
    }

    /// The seek-time counterpart to `play_bounded`: re-syncs playback to `frame` without
    /// wraparound, stopping at `end_frame`.
    pub fn seek_bounded(&self, frame: usize, end_frame: usize) {
        let _ = self.cmd_tx.send(AudioCmd::Seek {
            frame,
            loop_start: None,
            loop_end: Some(end_frame),
        });
    }

    /// Sets the gains and ceiling the stereo fold-down uses, for every source built from here on.
    ///
    /// Separate from construction because the gains are derived from per-channel peaks, and on a
    /// streamed buffer those do not exist yet when the engine is built — the pyramid that measures
    /// them takes ~53s on a 30GB file, and playback must not wait for it. So the engine starts at
    /// unity (a raw sum, the old behaviour) and is told the real gains when they are known. Also
    /// how Remove Empty Channels updates them: dropping 48 dead channels does not change any
    /// gain, but dropping live ones does.
    pub fn set_fold(&self, fold: crate::model::dsp::Fold) {
        let _ = self.cmd_tx.send(AudioCmd::SetFold(fold));
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(AudioCmd::Stop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Waits until `done` holds, failing after 3 seconds with `what`.
    fn wait_for(what: &str, done: impl Fn() -> bool) {
        let started = Instant::now();
        while !done() {
            assert!(started.elapsed() < Duration::from_secs(3), "timed out waiting: {what}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn stereo_seconds(seconds: f32) -> Vec<Vec<f32>> {
        vec![vec![0.0f32; (48000.0 * seconds) as usize]; 2]
    }

    /// An edit while stopped only drops the engine's copy. The next Play must then play the
    /// *edited* audio, not the copy from before: here the edit shortens 5s to 50ms, so playback
    /// ends at once instead of running for 5s.
    #[test]
    fn play_after_an_edit_while_stopped_plays_the_edited_audio() {
        let Some(engine) = AudioEngine::try_new_lazy(48000) else { return };
        let long = stereo_seconds(5.0);
        engine.load_if_needed(&long);
        engine.play(0);
        wait_for("first playback", || engine.position.load(Ordering::Relaxed) > 4800);
        engine.pause();

        wait_for("the pause", || !engine.is_playing());
        // Overwritten by the audio thread when it handles the next Play, so the waits below
        // cannot be satisfied by the state left over from the first playback.
        engine.position.store(usize::MAX, Ordering::Relaxed);

        let short = stereo_seconds(0.05);
        engine.audio_changed(&short);
        engine.load_if_needed(&short);
        engine.play(0);
        wait_for("the Play to be handled", || engine.position.load(Ordering::Relaxed) != usize::MAX);
        wait_for("the 50ms edit to finish", || !engine.is_playing());
        assert!(engine.position.load(Ordering::Relaxed) <= 2400, "played past the edited end");
    }

    /// The race `Release` guards against: Play is sent, and an edit asks for a release before
    /// the audio thread has handled the Play. The release must be ignored while a source is
    /// queued, or a Seek during that playback would build its source from empty data and stop.
    #[test]
    fn a_release_while_a_source_is_queued_keeps_the_data_for_seek() {
        let Some(engine) = AudioEngine::try_new_lazy(48000) else { return };
        let audio = stereo_seconds(5.0);
        engine.load_if_needed(&audio);
        engine.play(0);
        let _ = engine.cmd_tx.send(AudioCmd::Release);
        wait_for("playing", || engine.is_playing());
        engine.seek(48000);
        wait_for("playback after the seek", || engine.position.load(Ordering::Relaxed) > 48000 + 4800);
        assert!(engine.is_playing(), "the seek played the data, not an empty buffer");
    }

    /// Playback that ended by itself leaves the engine's copy in place, and the next edit drops
    /// it rather than sending new audio to an engine that is not playing.
    #[test]
    fn an_edit_after_playback_ended_drops_the_copy() {
        let Some(engine) = AudioEngine::try_new_lazy(48000) else { return };
        let short = stereo_seconds(0.05);
        engine.load_if_needed(&short);
        engine.play(0);
        wait_for("playing", || engine.is_playing() || engine.position.load(Ordering::Relaxed) > 0);
        wait_for("the end", || !engine.is_playing());
        engine.audio_changed(&short);
        assert!(!engine.loaded.get(), "the next Play must send the audio again");
    }

    /// Preview and audition engines are built with their audio (`try_new`) and never given it
    /// again. Pausing one must not drop it, or its next Play would be silent.
    #[test]
    fn a_non_lazy_engine_still_plays_after_a_pause() {
        let Some(engine) = AudioEngine::try_new(stereo_seconds(5.0), 48000) else { return };
        engine.play(0);
        wait_for("first playback", || engine.position.load(Ordering::Relaxed) > 4800);
        engine.pause();
        engine.play(0);
        wait_for("replay", || engine.is_playing() && engine.position.load(Ordering::Relaxed) > 4800);
    }

    /// Needs an output device, and passes without checking anything when there is none (CI).
    ///
    /// A lazy engine starts with no audio, gets it just before Play, and loses it at Pause. The
    /// playhead moving is what shows the audio arrived: empty data ends playback at once.
    #[test]
    fn a_lazy_engine_plays_the_audio_it_is_given_at_play_time() {
        let Some(engine) = AudioEngine::try_new_lazy(48000) else {
            eprintln!("no audio device; skipped");
            return;
        };
        assert!(!engine.loaded.get(), "a lazy engine starts without a copy");
        let audio = vec![vec![0.0f32; 48000 * 5]; 2];

        engine.audio_changed(&audio);
        assert!(!engine.loaded.get(), "an edit while stopped does not send the audio");

        engine.load_if_needed(&audio);
        assert!(engine.loaded.get());
        engine.play(0);
        let started = Instant::now();
        while engine.position.load(Ordering::Relaxed) < 4800 {
            assert!(started.elapsed() < Duration::from_secs(3), "playback did not advance");
            thread::sleep(Duration::from_millis(10));
        }
        assert!(engine.is_playing());

        engine.audio_changed(&audio);
        assert!(engine.loaded.get(), "an edit while playing sends the new audio");

        engine.pause();
        assert!(!engine.loaded.get(), "pausing drops the copy");
        engine.load_if_needed(&audio);
        engine.play(0);
        let started = Instant::now();
        while !engine.is_playing() || engine.position.load(Ordering::Relaxed) < 4800 {
            assert!(started.elapsed() < Duration::from_secs(3), "replay did not advance");
            thread::sleep(Duration::from_millis(10));
        }
    }
}
