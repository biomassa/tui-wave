use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

use crossbeam_channel::{unbounded, Sender};
use rodio::{DeviceSinkBuilder, Player};

use super::source::DocumentSource;

enum AudioCmd {
    Play { from_frame: usize },
    Pause,
    Stop,
    Seek(usize),
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

impl AudioEngine {
    /// Spawns the audio thread. Returns `None` if no output device is available — callers
    /// should treat that as "playback disabled," not a fatal error, since editing/viewing
    /// a waveform shouldn't require a working audio device.
    pub fn try_new(channels: Vec<Vec<f32>>, sample_rate: u32) -> Option<Self> {
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
        let data = Arc::new(channels);

        let position_for_thread = position.clone();
        let playing_for_thread = playing.clone();

        thread::spawn(move || {
            let Ok(device_sink) = DeviceSinkBuilder::open_default_sink() else {
                return;
            };
            let player = Player::connect_new(device_sink.mixer());
            let mut data = data;

            for cmd in cmd_rx {
                match cmd {
                    AudioCmd::Reload(channels) => {
                        data = Arc::new(channels);
                    }
                    AudioCmd::Play { from_frame } => {
                        player.clear();
                        let source = DocumentSource::new(
                            data.clone(),
                            sample_rate,
                            from_frame,
                            position_for_thread.clone(),
                        );
                        player.append(source);
                        player.play();
                        playing_for_thread.store(true, Ordering::Relaxed);
                    }
                    AudioCmd::Pause => {
                        player.pause();
                        playing_for_thread.store(false, Ordering::Relaxed);
                    }
                    AudioCmd::Stop => {
                        player.clear();
                        playing_for_thread.store(false, Ordering::Relaxed);
                        position_for_thread.store(0, Ordering::Relaxed);
                    }
                    AudioCmd::Seek(frame) => {
                        let was_playing = playing_for_thread.load(Ordering::Relaxed);
                        player.clear();
                        position_for_thread.store(frame, Ordering::Relaxed);
                        if was_playing {
                            let source = DocumentSource::new(
                                data.clone(),
                                sample_rate,
                                frame,
                                position_for_thread.clone(),
                            );
                            player.append(source);
                            player.play();
                        }
                    }
                }
            }
        });

        Some(Self {
            cmd_tx,
            position,
            playing,
        })
    }

    pub fn play(&self, from_frame: usize) {
        let _ = self.cmd_tx.send(AudioCmd::Play { from_frame });
    }

    pub fn pause(&self) {
        let _ = self.cmd_tx.send(AudioCmd::Pause);
    }

    pub fn stop(&self) {
        let _ = self.cmd_tx.send(AudioCmd::Stop);
    }

    pub fn seek(&self, frame: usize) {
        let _ = self.cmd_tx.send(AudioCmd::Seek(frame));
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
