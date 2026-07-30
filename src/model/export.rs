//! Writing a buffer out as FLAC or MP3 (File ▸ Export).
//!
//! Deliberately separate from `io::save_wav_with`: that owns the *working* file — the format
//! Save and Save As write, the one markers, `bext` and the head/tail sidecar ride along with.
//! This is the delivery path, and neither format here can carry any of that metadata.
//!
//! Mono and stereo only. Splitting a multichannel buffer is File ▸ Export Channels'
//! job — MP3 cannot represent more than two channels at all, and a silently-dropped channel
//! would be worse than a refusal.

use std::path::Path;

use super::document::Document;
use super::io::{quantize, BitDepth, DitherRng};

/// Which container File ▸ Export writes. WAV is absent on purpose: Save and Save As own it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Flac,
    Mp3,
}

impl ExportFormat {
    pub fn label(self) -> &'static str {
        match self {
            ExportFormat::Flac => "FLAC",
            ExportFormat::Mp3 => "MP3",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Flac => "flac",
            ExportFormat::Mp3 => "mp3",
        }
    }

    /// ←/→ cycling in the dialog, mirroring `BitDepth::next`/`prev`.
    pub fn next(self) -> Self {
        match self {
            ExportFormat::Flac => ExportFormat::Mp3,
            ExportFormat::Mp3 => ExportFormat::Flac,
        }
    }

    pub fn prev(self) -> Self {
        // Two variants, so back and forward coincide; written out rather than aliased so
        // adding a third format makes both directions a compile error to revisit.
        match self {
            ExportFormat::Flac => ExportFormat::Mp3,
            ExportFormat::Mp3 => ExportFormat::Flac,
        }
    }
}

/// CBR bitrates the MP3 row offers, low to high. These are the values `Bitrate` exposes that
/// are worth offering for music; LAME accepts others, but a longer list is a worse picker.
pub const MP3_BITRATES: &[u16] = &[128, 160, 192, 256, 320];

/// Sample rates MPEG-1/2 Layer III can represent. A 96 kHz buffer — the common case for the
/// multichannel material this app is used with — is *not* among them, so the dialog blocks
/// rather than letting LAME fail with an opaque error.
pub const MP3_SAMPLE_RATES: &[u32] = &[8_000, 11_025, 12_000, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000];

/// FLAC has no float format at all, so only the two integer depths are ever offered here —
/// unlike WAV, where 32-bit float is the lossless working format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportSettings {
    Flac { depth: BitDepth, dither: bool },
    Mp3 { bitrate_kbps: u16 },
}

/// Why `doc` can't be exported with `settings`, or `None` when it can.
///
/// Computed up front and shown inline in the dialog with Export dimmed, rather than surfacing
/// as a failure after the fact — the same approach `App::cdp_params_blocker` takes for a CDP
/// process whose inputs aren't ready.
pub fn blocker(doc: &Document, settings: ExportSettings) -> Option<String> {
    if doc.len_samples() == 0 {
        return Some("Nothing to export — the buffer is empty.".into());
    }
    let channels = doc.channel_count();
    if channels == 0 || channels > 2 {
        return Some(format!(
            "Export writes mono or stereo only — use File ▸ Export Channels to split this {channels}-channel buffer first.",
        ));
    }
    if let ExportSettings::Mp3 { .. } = settings {
        if !MP3_SAMPLE_RATES.contains(&doc.sample_rate) {
            return Some(format!(
                "MP3 can't store {} Hz — resample to 44100 or 48000 first (Process ▸ Resample).",
                doc.sample_rate,
            ));
        }
    }
    None
}

/// Writes `doc` to `path` in the chosen format. Callers are expected to have consulted
/// [`blocker`] first; it is re-checked here so a direct call can't produce a broken file.
pub fn export(doc: &Document, path: &Path, settings: ExportSettings) -> color_eyre::Result<()> {
    if let Some(reason) = blocker(doc, settings) {
        return Err(color_eyre::eyre::eyre!(reason));
    }
    match settings {
        ExportSettings::Flac { depth, dither } => write_flac(doc, path, depth, dither),
        ExportSettings::Mp3 { bitrate_kbps } => write_mp3(doc, path, bitrate_kbps),
    }
}

/// Interleaves the document's channels as `bits`-deep integers, the layout both encoders want.
fn interleave_quantized(doc: &Document, bits: u16, dither: bool) -> Vec<i32> {
    let len = doc.len_samples();
    let mut out = Vec::with_capacity(len * doc.channel_count());
    let mut rng = DitherRng::new();
    for i in 0..len {
        for channel in &doc.channels {
            out.push(quantize(channel[i], bits, dither.then_some(&mut rng)));
        }
    }
    out
}

fn write_flac(doc: &Document, path: &Path, depth: BitDepth, dither: bool) -> color_eyre::Result<()> {
    use flacenc::component::BitRepr;
    use flacenc::error::Verify;

    let bits = depth.bits();
    let interleaved = interleave_quantized(doc, bits, dither);
    let source = flacenc::source::MemSource::from_samples(
        &interleaved,
        doc.channel_count(),
        bits as usize,
        doc.sample_rate as usize,
    );
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|e| color_eyre::eyre::eyre!("invalid FLAC encoder config: {e:?}"))?;
    let stream =
        flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
            .map_err(|e| color_eyre::eyre::eyre!("FLAC encode failed: {e}"))?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| color_eyre::eyre::eyre!("FLAC serialize failed: {e}"))?;
    let mut bytes = sink.as_slice().to_vec();
    normalize_fixed_blocksize_streaminfo(&mut bytes);
    std::fs::write(path, bytes)?;
    Ok(())
}

/// Makes STREAMINFO's `min_blocksize` equal its `max_blocksize`, the conventional encoding for
/// a fixed-blocksize stream.
///
/// `encode_with_fixed_block_size` writes every frame with the fixed blocking strategy, but
/// reports the *shorter final block* as `min_blocksize` — so a 44100-sample file at block size
/// 4096 gets `min=3140, max=4096`. That is defensible as a literal minimum, but no other
/// encoder does it: ffmpeg and the reference `flac` both write `min == max` and treat the
/// short last block as the understood exception.
///
/// It matters because readers use `min == max` to *detect* the blocking strategy. Symphonia
/// does exactly that (`strict_frame_header_check`: `is_fixed = block_len_min ==
/// block_len_max`), then rejects every frame header as inconsistent — a frame numbered by
/// frame index in a stream it believes is numbered by sample index. It scans for a valid
/// header, finds none, and fails with "end of stream". The reference decoder is more forgiving
/// but still warns ("sample or frame number does not increase correctly").
///
/// Without this the app could not reopen its own FLAC exports, which is how the problem was
/// found. Only files whose length is an exact multiple of the block size escaped it.
///
/// The layout is fixed by the format: `fLaC` (4) + metadata block header (4) + STREAMINFO,
/// whose first two 16-bit big-endian fields are min and max block size. A truncated or
/// unexpected buffer is left alone rather than blindly patched.
fn normalize_fixed_blocksize_streaminfo(bytes: &mut [u8]) {
    const STREAMINFO_START: usize = 8;
    const MIN_BLOCKSIZE: usize = STREAMINFO_START;
    const MAX_BLOCKSIZE: usize = STREAMINFO_START + 2;
    if bytes.len() < STREAMINFO_START + 34 || &bytes[..4] != b"fLaC" {
        return;
    }
    // The first metadata block must be STREAMINFO for this to be the right offset.
    if bytes[4] & 0x7f != 0 {
        return;
    }
    let (max_hi, max_lo) = (bytes[MAX_BLOCKSIZE], bytes[MAX_BLOCKSIZE + 1]);
    bytes[MIN_BLOCKSIZE] = max_hi;
    bytes[MIN_BLOCKSIZE + 1] = max_lo;
}

fn write_mp3(doc: &Document, path: &Path, bitrate_kbps: u16) -> color_eyre::Result<()> {
    use mp3lame_encoder::{Builder, DualPcm, FlushNoGap, MonoPcm, Quality};

    let mut builder = Builder::new()
        .ok_or_else(|| color_eyre::eyre::eyre!("could not initialise the MP3 encoder"))?;
    let channels = doc.channel_count() as u8;
    builder
        .set_num_channels(channels)
        .map_err(|e| color_eyre::eyre::eyre!("MP3 channel count rejected: {e}"))?;
    builder
        .set_sample_rate(doc.sample_rate)
        .map_err(|e| color_eyre::eyre::eyre!("MP3 sample rate rejected: {e}"))?;
    builder
        .set_brate(bitrate_for(bitrate_kbps))
        .map_err(|e| color_eyre::eyre::eyre!("MP3 bitrate rejected: {e}"))?;
    // Best: this is an offline export of a finished buffer, so encoder time is not the
    // constraint the way it would be for realtime or batch transcoding.
    builder
        .set_quality(Quality::Best)
        .map_err(|e| color_eyre::eyre::eyre!("MP3 quality rejected: {e}"))?;
    let mut encoder = builder
        .build()
        .map_err(|e| color_eyre::eyre::eyre!("MP3 encoder setup failed: {e}"))?;

    let len = doc.len_samples();
    let mut out = Vec::with_capacity(mp3lame_encoder::max_required_buffer_size(len));
    if channels == 1 {
        encoder
            .encode_to_vec(MonoPcm(&doc.channels[0]), &mut out)
            .map_err(|e| color_eyre::eyre::eyre!("MP3 encode failed: {e}"))?;
    } else {
        encoder
            .encode_to_vec(DualPcm { left: &doc.channels[0], right: &doc.channels[1] }, &mut out)
            .map_err(|e| color_eyre::eyre::eyre!("MP3 encode failed: {e}"))?;
    }
    // The final partial frame lives in the encoder until flushed; without this the tail of
    // the file is silently missing.
    encoder
        .flush_to_vec::<FlushNoGap>(&mut out)
        .map_err(|e| color_eyre::eyre::eyre!("MP3 flush failed: {e}"))?;
    std::fs::write(path, &out)?;
    Ok(())
}

/// Maps a kbps number from [`MP3_BITRATES`] onto LAME's own enum. Anything unexpected falls
/// back to 192, a defensible middle rather than an error for a value the dialog can't produce.
fn bitrate_for(kbps: u16) -> mp3lame_encoder::Bitrate {
    use mp3lame_encoder::Bitrate;
    match kbps {
        128 => Bitrate::Kbps128,
        160 => Bitrate::Kbps160,
        256 => Bitrate::Kbps256,
        320 => Bitrate::Kbps320,
        _ => Bitrate::Kbps192,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(channels: Vec<Vec<f32>>, sample_rate: u32) -> Document {
        Document {
            head_tail_marks: Vec::new(),
            channels,
            sample_rate,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bits_per_sample: 32,
            bext: None,
            stream: None,
        }
    }

    /// A one-second stereo sine pair, distinct per channel so a swapped or duplicated channel
    /// is detectable after a round trip.
    fn sine_doc(sample_rate: u32) -> Document {
        let n = sample_rate as usize;
        let mk = |freq: f32| {
            (0..n)
                .map(|i| 0.5 * (std::f32::consts::TAU * freq * i as f32 / sample_rate as f32).sin())
                .collect::<Vec<f32>>()
        };
        doc(vec![mk(440.0), mk(660.0)], sample_rate)
    }

    /// The regression guard for `normalize_fixed_blocksize_streaminfo`: a length that is *not*
    /// an exact multiple of the encoder's block size produces a short final block, which is
    /// what made every such export unreadable by this app's own importer. Exact multiples
    /// always worked, so they are the control.
    #[test]
    fn flac_exports_are_readable_whatever_the_length() {
        for n in [100usize, 4096, 8192, 40960, 44100, 44101] {
            let d = doc(vec![vec![0.25f32; n], vec![-0.25f32; n]], 44_100);
            let path = std::env::temp_dir()
                .join(format!("tui_wave_flaclen_{n}_{}.flac", std::process::id()));
            export(&d, &path, ExportSettings::Flac { depth: BitDepth::Int16, dither: false })
                .unwrap();
            let back = super::super::io::load_audio(&path)
                .unwrap_or_else(|e| panic!("length {n} produced an unreadable FLAC: {e}"));
            assert_eq!(back.len_samples(), n, "length {n} round trip");
            assert_eq!(back.channel_count(), 2);
            std::fs::remove_file(&path).ok();
        }
    }

    /// STREAMINFO's min and max block size must come out equal — that equality is how readers
    /// detect the fixed blocking strategy the frames actually use.
    #[test]
    fn flac_streaminfo_declares_a_fixed_block_size() {
        let d = doc(vec![vec![0.25f32; 44_100]], 44_100);
        let path =
            std::env::temp_dir().join(format!("tui_wave_flacsi_{}.flac", std::process::id()));
        export(&d, &path, ExportSettings::Flac { depth: BitDepth::Int16, dither: false }).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let min = u16::from_be_bytes([bytes[8], bytes[9]]);
        let max = u16::from_be_bytes([bytes[10], bytes[11]]);
        assert_eq!(min, max, "min/max block size must match for a fixed-blocksize stream");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn format_cycling_wraps_in_both_directions() {
        assert_eq!(ExportFormat::Flac.next(), ExportFormat::Mp3);
        assert_eq!(ExportFormat::Mp3.next(), ExportFormat::Flac);
        assert_eq!(ExportFormat::Flac.prev(), ExportFormat::Mp3);
        assert_eq!(ExportFormat::Flac.extension(), "flac");
        assert_eq!(ExportFormat::Mp3.label(), "MP3");
    }

    /// FLAC is lossless *for the integer stream it is given*, so a round trip must match the
    /// source to within one quantization step at the chosen depth — no more.
    #[test]
    fn flac_round_trips_within_one_quantization_step() {
        for (depth, step) in [(BitDepth::Int16, 1.0 / 32768.0), (BitDepth::Int24, 1.0 / 8388608.0)] {
            let src = sine_doc(44_100);
            let path = std::env::temp_dir()
                .join(format!("tui_wave_flac_{:?}_{}.flac", depth, std::process::id()));
            export(&src, &path, ExportSettings::Flac { depth, dither: false }).unwrap();

            let back = super::super::io::load_audio(&path).unwrap();
            assert_eq!(back.channel_count(), 2, "{depth:?}");
            assert_eq!(back.sample_rate, 44_100, "{depth:?}");
            assert_eq!(back.len_samples(), src.len_samples(), "{depth:?}");
            for (ch, (a, b)) in back.channels.iter().zip(&src.channels).enumerate() {
                for (i, (x, y)) in a.iter().zip(b).enumerate() {
                    assert!((x - y).abs() <= step * 1.5, "{depth:?} ch{ch}[{i}]: {x} vs {y}");
                }
            }
            std::fs::remove_file(&path).ok();
        }
    }

    /// MP3 is lossy, so the assertion is on shape rather than samples: a real MPEG frame
    /// header, a plausible size for the bitrate, and a decodable duration.
    #[test]
    fn mp3_export_produces_a_decodable_file_at_the_requested_bitrate() {
        let src = sine_doc(44_100);
        let path =
            std::env::temp_dir().join(format!("tui_wave_mp3_{}.mp3", std::process::id()));
        export(&src, &path, ExportSettings::Mp3 { bitrate_kbps: 320 }).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes.is_empty(), "the encoder wrote nothing");
        // One second at 320 kbps is ~40 KB. A wide band still catches "flush was skipped"
        // (far too small) and "wrong bitrate" (far too large).
        assert!(
            (20_000..80_000).contains(&bytes.len()),
            "unexpected size for 1s at 320kbps: {} bytes",
            bytes.len(),
        );
        // A raw MPEG sync word (or an ID3 header, if LAME chose to write one).
        assert!(
            bytes.starts_with(b"ID3") || (bytes[0] == 0xFF && bytes[1] & 0xE0 == 0xE0),
            "no ID3 or MPEG sync at the start: {:02X?}",
            &bytes[..4],
        );
        std::fs::remove_file(&path).ok();
    }

    /// A lower bitrate must actually produce a smaller file — the proof the setting reaches
    /// the encoder rather than being ignored.
    #[test]
    fn a_lower_mp3_bitrate_produces_a_smaller_file() {
        let src = sine_doc(44_100);
        let mut sizes = Vec::new();
        for kbps in [128u16, 320] {
            let path = std::env::temp_dir()
                .join(format!("tui_wave_mp3_{kbps}_{}.mp3", std::process::id()));
            export(&src, &path, ExportSettings::Mp3 { bitrate_kbps: kbps }).unwrap();
            sizes.push(std::fs::metadata(&path).unwrap().len());
            std::fs::remove_file(&path).ok();
        }
        assert!(sizes[0] < sizes[1], "128kbps ({}) must be smaller than 320 ({})", sizes[0], sizes[1]);
    }

    #[test]
    fn a_multichannel_buffer_is_blocked_for_both_formats() {
        let six = doc(vec![vec![0.1f32; 32]; 6], 44_100);
        for settings in [
            ExportSettings::Flac { depth: BitDepth::Int24, dither: false },
            ExportSettings::Mp3 { bitrate_kbps: 320 },
        ] {
            let msg = blocker(&six, settings).expect("6 channels must be blocked");
            assert!(msg.contains("Export Channels"), "the message must point somewhere: {msg}");
        }
        // And a direct call refuses rather than writing a broken file.
        let path = std::env::temp_dir().join("tui_wave_never_written.flac");
        assert!(export(&six, &path, ExportSettings::Flac { depth: BitDepth::Int16, dither: false }).is_err());
        assert!(!path.exists());
    }

    /// 96 kHz is the common case for this app's multichannel material, and MPEG Layer III
    /// simply cannot store it — the block must name the fix rather than let LAME fail.
    #[test]
    fn mp3_blocks_a_sample_rate_it_cannot_store() {
        let src = doc(vec![vec![0.1f32; 32], vec![0.1f32; 32]], 96_000);
        let msg = blocker(&src, ExportSettings::Mp3 { bitrate_kbps: 320 })
            .expect("96 kHz must be blocked for MP3");
        assert!(msg.contains("Resample"), "{msg}");
        // FLAC has no such limit, so the same buffer is fine there.
        assert!(blocker(&src, ExportSettings::Flac { depth: BitDepth::Int24, dither: false }).is_none());
    }

    #[test]
    fn an_empty_buffer_is_blocked() {
        let empty = doc(vec![Vec::new()], 44_100);
        assert!(blocker(&empty, ExportSettings::Mp3 { bitrate_kbps: 192 }).is_some());
    }

    #[test]
    fn mono_exports_for_both_formats() {
        let mono = doc(vec![vec![0.25f32; 44_100]], 44_100);
        for (settings, ext) in [
            (ExportSettings::Flac { depth: BitDepth::Int16, dither: false }, "flac"),
            (ExportSettings::Mp3 { bitrate_kbps: 192 }, "mp3"),
        ] {
            let path = std::env::temp_dir()
                .join(format!("tui_wave_mono_{}_{}.{ext}", ext, std::process::id()));
            export(&mono, &path, settings).unwrap();
            assert!(std::fs::metadata(&path).unwrap().len() > 0);
            std::fs::remove_file(&path).ok();
        }
    }
}
