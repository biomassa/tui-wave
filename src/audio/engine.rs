use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

use crossbeam_channel::{unbounded, Sender};
use rodio::{DeviceSinkBuilder, Player};

use super::source::DocumentSource;
use super::stream_source::StreamedSource;
use crate::model::stream::StreamedSamples;

/// Where the audio thread gets its samples.
///
/// The two arms differ only in how a source is built for them — everything else (the command
/// protocol, the position atomic, loop handling) is shared, which is what keeps a streamed
/// buffer's transport behaving exactly like an ordinary one from the UI's side.
enum PlaybackData {
    /// A fully-loaded document. The engine owns a second copy of its samples.
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
}

/// Owns the audio device and playback thread. The UI thread only ever talks to this
/// through `cmd_tx` (fire-and-forget) and reads `position`/`playing` atomics — it never
/// blocks on audio, and audio never blocks on the terminal.
pub struct AudioEngine {
    cmd_tx: Sender<AudioCmd>,
    pub position: Arc<AtomicUsize>,
    pub playing: Arc<AtomicBool>,
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
        Self::spawn(PlaybackData::Resident(Arc::new(channels)), sample_rate)
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
        Self::spawn(PlaybackData::Streamed(stream), sample_rate)
    }

    fn spawn(data: PlaybackData, sample_rate: u32) -> Option<Self> {
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
                        );
                        player.play();
                        playing_for_thread.store(true, Ordering::Relaxed);
                    }
                    AudioCmd::Pause => {
                        player.pause();
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
        })
    }

    pub fn play(&self, from_frame: usize) {
        let _ = self
            .cmd_tx
            .send(AudioCmd::Play { from_frame, loop_start: None, loop_end: None });
    }

    pub fn play_looped(&self, from_frame: usize, loop_start: usize, loop_end: usize) {
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
        let _ = self.cmd_tx.send(AudioCmd::Play {
            from_frame,
            loop_start: None,
            loop_end: Some(end_frame),
        });
    }

    pub fn pause(&self) {
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

    /// Refreshes the audio thread's sample data after a document edit (cut/paste/etc).
    /// Only affects future `play`/`seek` calls — a source already playing keeps the data it
    /// captured when it started.
    pub fn reload(&self, channels: Vec<Vec<f32>>) {
        let _ = self.cmd_tx.send(AudioCmd::Reload(channels));
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
