use std::path::{Path, PathBuf};

use hound::{SampleFormat, WavSpec, WavWriter};

use super::document::Document;

/// Extensions the Files panel lists and [`load_audio`] can open, lowercase, no leading dot.
///
/// MP3 is deliberately absent: it is an export-only delivery format here (see
/// `model::export`), never something a session's working buffer is decoded from.
pub const IMPORT_EXTENSIONS: &[&str] = &["wav", "flac", "aif", "aiff"];

/// Opens any supported audio file, dispatching on extension.
///
/// `.wav` goes to [`load_wav`] rather than through symphonia, and that routing is
/// load-bearing: `load_wav` is the only reader that picks up the BWF `cue `/`adtl` markers and
/// the `bext` chunk (`model::bwf`), which symphonia would silently drop. Everything else goes
/// to [`load_symphonia`].
///
/// CDP's runner keeps calling `load_wav` directly — its outputs are always WAV, so a probe
/// there would buy nothing.
pub fn load_audio(path: impl AsRef<Path>) -> color_eyre::Result<Document> {
    let path: PathBuf = path.as_ref().to_path_buf();
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "wav" => load_wav(&path),
        "flac" | "aif" | "aiff" => load_symphonia(&path),
        // Named rather than generic: the Files panel only ever offers `IMPORT_EXTENSIONS`, so
        // reaching this means a path typed or passed on the command line.
        other => Err(color_eyre::eyre::eyre!(
            "unsupported audio format '{other}' — this build opens {}",
            IMPORT_EXTENSIONS.join(", "),
        )),
    }
}

/// Decodes a non-WAV file (FLAC, AIFF) into the same deinterleaved f32 `Document` every other
/// path produces.
///
/// `markers` and `bext` come back empty — both are RIFF chunks with no equivalent here.
/// (FLAC's CUESHEET block and AIFF's `MARK` chunk could carry markers; reading them is a
/// deliberate follow-up, not part of this.) The `.headstails` sidecar *is* read, because it
/// sits next to the audio file rather than inside it and so is format-agnostic by
/// construction.
fn load_symphonia(path: &Path) -> color_eyre::Result<Document> {
    use symphonia::core::audio::Signal;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::errors::Error as SymphoniaError;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path)?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe().format(
        &hint,
        stream,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| color_eyre::eyre::eyre!("no audio track in {}", path.display()))?;
    let track_id = track.id;
    let params = track.codec_params.clone();
    let sample_rate = params
        .sample_rate
        .ok_or_else(|| color_eyre::eyre::eyre!("{} declares no sample rate", path.display()))?;
    let channel_count = params
        .channels
        .map(|c| c.count())
        .ok_or_else(|| color_eyre::eyre::eyre!("{} declares no channels", path.display()))?;
    // 32 matches `Document::bits_per_sample`'s "synthesized buffer" default — a format that
    // declares no source depth is closer to that than to a guess.
    let bits_per_sample = params.bits_per_sample.unwrap_or(32) as u16;

    let mut decoder =
        symphonia::default::get_codecs().make(&params, &DecoderOptions::default())?;
    let mut channels: Vec<Vec<f32>> = vec![Vec::new(); channel_count];
    if let Some(frames) = params.n_frames {
        for ch in &mut channels {
            ch.reserve(frames as usize);
        }
    }

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            // The reader signals a clean end of stream as an I/O EOF rather than a dedicated
            // variant, so this is the normal way out of the loop, not an error path.
            Err(SymphoniaError::IoError(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(e) => return Err(e.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                // Convert whatever the codec produced into planar f32 in one step; every
                // `AudioBufferRef` variant knows how to do this for itself.
                let spec = *decoded.spec();
                let mut buf = symphonia::core::audio::AudioBuffer::<f32>::new(
                    decoded.capacity() as u64,
                    spec,
                );
                decoded.convert(&mut buf);
                for (ch, out) in channels.iter_mut().enumerate().take(buf.spec().channels.count()) {
                    out.extend_from_slice(buf.chan(ch));
                }
            }
            // A corrupt packet mid-file is recoverable: symphonia's own guidance is to skip
            // it and keep decoding rather than discard everything read so far.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(e.into()),
        }
    }

    // Channels must be equal length — `Document::len_samples` reads channel 0 and the playback
    // source assumes the rest match it (see `Document::insert_range`'s doc comment for the
    // panic that invariant exists to prevent). A truncated final packet can leave them ragged.
    let len = channels.iter().map(|c| c.len()).min().unwrap_or(0);
    for ch in &mut channels {
        ch.truncate(len);
    }

    let head_tail_marks: Vec<usize> = super::headstails::load(path, sample_rate)
        .into_iter()
        .map(|m| m.min(len))
        .collect();

    Ok(Document {
        head_tail_marks,
        channels,
        sample_rate,
        bits_per_sample,
        selection: None,
        cursor: 0,
        dirty: false,
        path: Some(path.to_path_buf()),
        markers: Vec::new(),
        bext: None,
        stream: None,
    })
}

/// Loads a WAV fully into memory.
///
/// Decoding goes through `model::wavread` rather than `hound`, which is what makes `RF64`/`BW64`
/// (and so any file over 4GB) readable at all — see that module for why hound cannot be made to
/// do it. `hound` still writes.
///
/// Whether a file *should* be loaded this way is a separate question, decided by the caller from
/// `wavread::probe`'s `resident_bytes` before it gets here (`App::load_file`). This function
/// makes the attempt fail cleanly rather than fatally if that gate is bypassed: the per-channel
/// capacity is claimed with `try_reserve`, so a file too large to hold returns an error instead
/// of aborting the process the way an ordinary allocation failure would. Reserving up front also
/// avoids `Vec` growth doubling the peak — pushing a 20GB channel set without it would transiently
/// need about 30GB.
pub fn load_wav(path: impl AsRef<Path>) -> color_eyre::Result<Document> {
    let path: PathBuf = path.as_ref().to_path_buf();
    let mut frames = super::wavread::WavFrames::open(&path)?;
    let info = frames.info();

    let frame_count = usize::try_from(info.frame_count)
        .map_err(|_| color_eyre::eyre::eyre!("{} has more frames than fit in memory", path.display()))?;
    let mut channels: Vec<Vec<f32>> = vec![Vec::new(); info.channels];
    for ch in &mut channels {
        ch.try_reserve_exact(frame_count).map_err(|_| {
            color_eyre::eyre::eyre!(
                "{} needs {:.1} GB of memory to open ({} channels x {} frames)",
                path.display(),
                info.resident_bytes() as f64 / (1024.0 * 1024.0 * 1024.0),
                info.channels,
                info.frame_count,
            )
        })?;
    }

    let mut at = 0u64;
    while at < info.frame_count {
        let n = frames.read_into(at, super::wavread::READ_BLOCK_FRAMES, &mut channels)?;
        if n == 0 {
            break;
        }
        at += n as u64;
    }

    let (mut markers, bext) = super::bwf::read_markers_and_bext(&path);
    // Clamp any out-of-range cue positions to the actual sample count.
    let len = channels.first().map(|c| c.len()).unwrap_or(0);
    for m in &mut markers {
        m.position = m.position.min(len);
    }

    // Head/tail marks live in a `.headstails` sidecar next to the audio, not in the WAV's own
    // chunks — see `model::headstails`. Absent or unreadable simply means "no marks".
    let head_tail_marks: Vec<usize> = super::headstails::load(&path, info.sample_rate)
        .into_iter()
        .map(|m| m.min(len))
        .collect();

    Ok(Document {
        head_tail_marks,
        channels,
        sample_rate: info.sample_rate,
        bits_per_sample: info.bits_per_sample,
        selection: None,
        cursor: 0,
        dirty: false,
        path: Some(path),
        markers,
        bext,
        stream: None,
    })
}

/// Quantizes one f32 sample to a `bits`-deep signed integer, optionally with TPDF dither.
///
/// Full-scale maps to 2^(bits-1), matching the normalization `load_wav` uses on the way in, so
/// a load→save round-trip at the same depth is stable. Shared by the WAV writer and the FLAC
/// encoder (`model::export`) so the two can never drift apart on rounding or clipping — FLAC
/// is integer-only, and a second copy of this arithmetic is exactly the kind of duplication
/// `model::dsp` exists to prevent.
pub(crate) fn quantize(sample: f32, bits: u16, dither: Option<&mut DitherRng>) -> i32 {
    let scale = (1i64 << (bits - 1)) as f32;
    let mut v = sample * scale;
    if let Some(rng) = dither {
        v += rng.tpdf();
    }
    v.round().clamp(-scale, scale - 1.0) as i32
}

/// Output sample format chosen at save time. The in-memory representation is always f32;
/// `Int16`/`Int24` re-quantize on the way out (with optional dithering), while `Float32`
/// round-trips losslessly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitDepth {
    Int16,
    Int24,
    Float32,
}

impl BitDepth {
    pub fn label(self) -> &'static str {
        match self {
            BitDepth::Int16 => "16-bit int",
            BitDepth::Int24 => "24-bit int",
            BitDepth::Float32 => "32-bit float",
        }
    }

    pub fn next(self) -> Self {
        match self {
            BitDepth::Int16 => BitDepth::Int24,
            BitDepth::Int24 => BitDepth::Float32,
            BitDepth::Float32 => BitDepth::Int16,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            BitDepth::Int16 => BitDepth::Float32,
            BitDepth::Int24 => BitDepth::Int16,
            BitDepth::Float32 => BitDepth::Int24,
        }
    }

    pub(crate) fn bits(self) -> u16 {
        match self {
            BitDepth::Int16 => 16,
            BitDepth::Int24 => 24,
            BitDepth::Float32 => 32,
        }
    }

    /// Maps the source file's `bits_per_sample` to the nearest `BitDepth` variant.
    pub fn from_bits(bits: u16) -> Self {
        match bits {
            16 => BitDepth::Int16,
            24 => BitDepth::Int24,
            _ => BitDepth::Float32,
        }
    }

    fn sample_format(self) -> SampleFormat {
        match self {
            BitDepth::Float32 => SampleFormat::Float,
            _ => SampleFormat::Int,
        }
    }

    /// Whether dithering is meaningful — only when re-quantizing to integer PCM.
    pub fn supports_dither(self) -> bool {
        !matches!(self, BitDepth::Float32)
    }
}

/// Small, dependency-free xorshift PRNG used purely to generate dither noise. A fixed seed
/// keeps saves reproducible; dither only needs to be decorrelated from the signal, not
/// cryptographically random.
pub(crate) struct DitherRng(u32);

impl DitherRng {
    pub(crate) fn new() -> Self {
        DitherRng(0x9E3779B9)
    }
    /// Uniform f32 in [0, 1).
    fn next_unit(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x >> 8) as f32 / (1u32 << 24) as f32
    }
    /// TPDF (triangular) noise in [-1, 1] LSB, the standard choice for audio dither.
    fn tpdf(&mut self) -> f32 {
        self.next_unit() - self.next_unit()
    }
}

/// Saves at the document's original bit depth. Used by quick Save to round-trip
/// the source format; Save As goes through `save_wav_with` so the user can pick a depth.
pub fn save_wav(doc: &Document, path: impl AsRef<Path>) -> color_eyre::Result<()> {
    save_wav_with(doc, path, BitDepth::from_bits(doc.bits_per_sample), false)
}

/// Saves at the requested bit depth. Integer depths re-quantize from f32; `dither` adds
/// TPDF noise before quantization to decorrelate quantization error (ignored for Float32).
pub fn save_wav_with(
    doc: &Document,
    path: impl AsRef<Path>,
    depth: BitDepth,
    dither: bool,
) -> color_eyre::Result<()> {
    let path = path.as_ref();
    let spec = WavSpec {
        channels: doc.channel_count().max(1) as u16,
        sample_rate: doc.sample_rate,
        bits_per_sample: depth.bits(),
        sample_format: depth.sample_format(),
    };
    let mut writer = WavWriter::create(path, spec)?;
    match depth {
        BitDepth::Float32 => {
            for i in 0..doc.len_samples() {
                for channel in &doc.channels {
                    writer.write_sample(channel[i])?;
                }
            }
        }
        BitDepth::Int16 | BitDepth::Int24 => {
            let bits = depth.bits();
            let mut rng = DitherRng::new();
            for i in 0..doc.len_samples() {
                for channel in &doc.channels {
                    let q = quantize(channel[i], bits, dither.then_some(&mut rng));
                    writer.write_sample(q)?;
                }
            }
        }
    }
    writer.finalize()?;
    // Append cue/adtl marker chunks and any preserved bext after hound's fmt/data.
    super::bwf::append_aux_chunks(path, &doc.markers, &doc.bext)?;
    // Head/tail marks go to their own sidecar rather than into the WAV — see
    // `model::headstails`. Done here, in the one function every save path funnels through
    // (quick Save, Save As, Save All, region export, the Buffers panel's own save), so no
    // call site has to remember to do it; and after `finalize`, so a failed audio write never
    // leaves a sidecar describing a file that wasn't written.
    //
    // On Save As this writes next to the *new* path, so the marks follow the buffer. Any
    // sidecar beside the original file is left alone, exactly as the original audio is.
    super::headstails::save(path, &doc.head_tail_marks, doc.sample_rate);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FLAC is lossless, so a strong assertion is available: the decoded samples must match
    /// the WAV they were made from to within one 16-bit quantization step.
    #[test]
    fn load_audio_decodes_flac_losslessly() {
        let wav = load_audio("tests/fixtures/stereo_sine.wav").unwrap();
        let flac = load_audio("tests/fixtures/stereo_sine.flac").unwrap();
        assert_eq!(flac.channel_count(), wav.channel_count());
        assert_eq!(flac.sample_rate, wav.sample_rate);
        assert_eq!(flac.len_samples(), wav.len_samples());
        assert_eq!(flac.bits_per_sample, 16);
        let step = 1.0 / 32768.0;
        for (ch, (a, b)) in flac.channels.iter().zip(&wav.channels).enumerate() {
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                assert!((x - y).abs() <= step, "flac ch{ch}[{i}]: {x} vs {y}");
            }
        }
    }

    #[test]
    fn load_audio_decodes_aiff() {
        let wav = load_audio("tests/fixtures/stereo_sine.wav").unwrap();
        let aiff = load_audio("tests/fixtures/stereo_sine.aif").unwrap();
        assert_eq!(aiff.channel_count(), 2);
        assert_eq!(aiff.sample_rate, wav.sample_rate);
        assert_eq!(aiff.len_samples(), wav.len_samples());
        let step = 1.0 / 32768.0;
        for (a, b) in aiff.channels.iter().zip(&wav.channels) {
            for (x, y) in a.iter().zip(b) {
                assert!((x - y).abs() <= step);
            }
        }
    }

    /// MP3 is export-only. A fixture exists so this can assert the refusal is real rather than
    /// incidental — the Files panel never offers one, but a command-line path can reach here.
    #[test]
    fn load_audio_refuses_mp3_and_other_unsupported_extensions() {
        let err = match load_audio("tests/fixtures/stereo_sine.mp3") {
            Err(e) => e.to_string(),
            Ok(_) => panic!("mp3 must not be importable"),
        };
        assert!(err.contains("mp3"), "the message must name the extension: {err}");

        let err = match load_audio("tests/fixtures/whatever.xyz") {
            Err(e) => e.to_string(),
            Ok(_) => panic!("an unknown extension must not load"),
        };
        assert!(err.contains("xyz"), "{err}");
    }

    /// `.wav` must route to `load_wav`, not symphonia — it is the only reader that picks up
    /// BWF markers, so their survival is the proof the routing happened.
    #[test]
    fn load_audio_keeps_bwf_markers_by_routing_wav_to_load_wav() {
        let tmp = std::env::temp_dir().join(format!("tui_wave_load_audio_wav_{}.wav", std::process::id()));
        let mut doc = load_wav("tests/fixtures/stereo_sine.wav").unwrap();
        doc.markers = vec![super::super::document::Marker { position: 1234, label: "here".into() }];
        save_wav(&doc, &tmp).unwrap();

        let reloaded = load_audio(&tmp).unwrap();
        assert_eq!(reloaded.markers.len(), 1);
        assert_eq!(reloaded.markers[0].position, 1234);
        assert_eq!(reloaded.markers[0].label, "here");
        std::fs::remove_file(&tmp).ok();
    }

    /// The `.headstails` sidecar sits next to the audio rather than inside it, so it works for
    /// every importable format — including the ones symphonia decodes.
    #[test]
    fn load_audio_reads_the_headstails_sidecar_beside_a_flac() {
        let dir = std::env::temp_dir().join(format!("tui_wave_ht_flac_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let flac = dir.join("take.flac");
        std::fs::copy("tests/fixtures/stereo_sine.flac", &flac).unwrap();
        super::super::headstails::save(&flac, &[4410, 8820], 44100);

        let doc = load_audio(&flac).unwrap();
        assert_eq!(doc.head_tail_marks, vec![4410, 8820]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn loads_mono_wav() {
        let doc = load_wav("tests/fixtures/mono_sine.wav").unwrap();
        assert_eq!(doc.channel_count(), 1);
        assert_eq!(doc.sample_rate, 44100);
        assert_eq!(doc.len_samples(), 44100);
    }

    #[test]
    fn loads_stereo_wav() {
        let doc = load_wav("tests/fixtures/stereo_sine.wav").unwrap();
        assert_eq!(doc.channel_count(), 2);
        assert_eq!(doc.sample_rate, 44100);
        assert_eq!(doc.len_samples(), 44100);
        // Left and right channels carry different frequencies, so they must differ.
        assert_ne!(doc.channels[0], doc.channels[1]);
    }

    #[test]
    fn save_then_reload_round_trips_exactly() {
        let original = load_wav("tests/fixtures/stereo_sine.wav").unwrap();
        let tmp = std::env::temp_dir().join("tui_wave_save_roundtrip_test.wav");

        save_wav(&original, &tmp).unwrap();
        let reloaded = load_wav(&tmp).unwrap();

        assert_eq!(reloaded.sample_rate, original.sample_rate);
        assert_eq!(reloaded.channel_count(), original.channel_count());
        assert_eq!(reloaded.channels, original.channels);

        std::fs::remove_file(&tmp).unwrap();
    }

    fn approx_doc(samples: Vec<f32>) -> Document {
        Document {
            head_tail_marks: Vec::new(),
            channels: vec![samples],
            sample_rate: 44100,
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

    #[test]
    fn save_16bit_reloads_within_quantization_error() {
        let doc = approx_doc(vec![0.0, 0.5, -0.5, 0.999, -0.999, 0.123]);
        let tmp = std::env::temp_dir().join("tui_wave_16bit_test.wav");
        save_wav_with(&doc, &tmp, BitDepth::Int16, false).unwrap();
        let reloaded = load_wav(&tmp).unwrap();
        // One 16-bit LSB ≈ 1/32768; allow a couple LSBs of slack.
        for (a, b) in doc.channels[0].iter().zip(reloaded.channels[0].iter()) {
            assert!((a - b).abs() < 1.0 / 16000.0, "16-bit drift too large: {a} vs {b}");
        }
        std::fs::remove_file(&tmp).unwrap();
    }

    #[test]
    fn save_24bit_is_more_accurate_than_16bit() {
        let doc = approx_doc(vec![0.0, 0.5, -0.5, 0.999, -0.999, 0.123]);
        let tmp = std::env::temp_dir().join("tui_wave_24bit_test.wav");
        save_wav_with(&doc, &tmp, BitDepth::Int24, false).unwrap();
        let reloaded = load_wav(&tmp).unwrap();
        for (a, b) in doc.channels[0].iter().zip(reloaded.channels[0].iter()) {
            assert!((a - b).abs() < 1.0 / 4_000_000.0, "24-bit drift too large: {a} vs {b}");
        }
        std::fs::remove_file(&tmp).unwrap();
    }

    /// Head/tail marks go to a `.headstails` sidecar rather than into the WAV, and every save
    /// path funnels through `save_wav_with`, so this covers Save, Save As, Save All and region
    /// export in one. Also pins that the marks stay *out* of the WAV's own chunks — an
    /// implementation that folded them into the cue list would pass a naive round-trip check
    /// while corrupting the ordinary marker list.
    #[test]
    fn head_tail_marks_round_trip_through_a_sidecar_beside_the_wav() {
        let dir = std::env::temp_dir()
            .join(format!("tui_wave_headstails_io_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("take.wav");

        let mut doc = approx_doc(vec![0.0; 44_100]);
        doc.head_tail_marks = vec![4_410, 8_820, 22_050, 30_870];
        save_wav(&doc, &wav).unwrap();

        assert!(
            crate::model::headstails::sidecar_path(&wav).exists(),
            "the sidecar is written next to the audio"
        );
        let reloaded = load_wav(&wav).unwrap();
        assert_eq!(reloaded.head_tail_marks, doc.head_tail_marks);
        assert!(reloaded.markers.is_empty(), "and not as ordinary cue-chunk markers");

        // Clearing them and saving again removes the sidecar, so the next load finds none.
        doc.head_tail_marks.clear();
        save_wav(&doc, &wav).unwrap();
        assert!(!crate::model::headstails::sidecar_path(&wav).exists());
        assert!(load_wav(&wav).unwrap().head_tail_marks.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Save As writes the sidecar beside the *new* file, so the marks follow the buffer — and
    /// leaves the original's sidecar alone, exactly as it leaves the original audio alone.
    #[test]
    fn save_as_writes_the_sidecar_next_to_the_new_path() {
        let dir = std::env::temp_dir()
            .join(format!("tui_wave_headstails_saveas_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let original = dir.join("original.wav");
        let copy = dir.join("copy.wav");

        let mut doc = approx_doc(vec![0.0; 44_100]);
        doc.head_tail_marks = vec![1_000, 2_000, 3_000, 4_000];
        save_wav(&doc, &original).unwrap();

        doc.head_tail_marks = vec![5_000, 6_000];
        save_wav_with(&doc, &copy, BitDepth::Int16, false).unwrap();

        assert_eq!(load_wav(&copy).unwrap().head_tail_marks, vec![5_000, 6_000]);
        assert_eq!(
            load_wav(&original).unwrap().head_tail_marks,
            vec![1_000, 2_000, 3_000, 4_000],
            "the original's sidecar is untouched"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn markers_and_bext_round_trip_through_save_and_load() {
        use crate::model::document::Marker;
        let mut doc = approx_doc(vec![0.0; 2000]);
        doc.markers = vec![
            Marker { position: 100, label: "Intro".into() },
            Marker { position: 1500, label: "Chorus".into() },
        ];
        doc.bext = Some(vec![1, 2, 3, 4, 5, 6, 7]); // arbitrary preserved bytes
        let tmp = std::env::temp_dir().join("tui_wave_markers_test.wav");
        save_wav_with(&doc, &tmp, BitDepth::Int16, false).unwrap();
        let reloaded = load_wav(&tmp).unwrap();
        assert_eq!(reloaded.markers.len(), 2);
        assert_eq!(reloaded.markers[0].position, 100);
        assert_eq!(reloaded.markers[0].label, "Intro");
        assert_eq!(reloaded.markers[1].position, 1500);
        assert_eq!(reloaded.markers[1].label, "Chorus");
        assert_eq!(reloaded.bext, Some(vec![1, 2, 3, 4, 5, 6, 7]));
        // Samples must still load correctly with the extra chunks present.
        assert_eq!(reloaded.len_samples(), 2000);
        std::fs::remove_file(&tmp).unwrap();
    }

    #[test]
    fn fixture_without_markers_loads_empty() {
        let doc = load_wav("tests/fixtures/mono_sine.wav").unwrap();
        assert!(doc.markers.is_empty());
        assert!(doc.bext.is_none());
    }

    #[test]
    fn dithered_save_stays_in_range_and_close() {
        let doc = approx_doc(vec![0.0, 0.25, -0.25, 0.8, -0.8]);
        let tmp = std::env::temp_dir().join("tui_wave_dither_test.wav");
        save_wav_with(&doc, &tmp, BitDepth::Int16, true).unwrap();
        let reloaded = load_wav(&tmp).unwrap();
        for (a, b) in doc.channels[0].iter().zip(reloaded.channels[0].iter()) {
            assert!(b.abs() <= 1.0, "sample out of range after dither: {b}");
            // Dither adds at most ~1 LSB of noise on top of quantization.
            assert!((a - b).abs() < 1.0 / 8000.0, "dithered drift too large: {a} vs {b}");
        }
        std::fs::remove_file(&tmp).unwrap();
    }
}
