use std::path::{Path, PathBuf};

use hound::{SampleFormat, WavReader, WavSpec, WavWriter};

use super::document::Document;

pub fn load_wav(path: impl AsRef<Path>) -> color_eyre::Result<Document> {
    let path: PathBuf = path.as_ref().to_path_buf();
    let mut reader = WavReader::open(&path)?;
    let spec = reader.spec();
    let channel_count = spec.channels as usize;

    let mut channels: Vec<Vec<f32>> = vec![Vec::new(); channel_count];

    match spec.sample_format {
        SampleFormat::Int => {
            // Normalize integer PCM to f32 in [-1.0, 1.0] based on bit depth.
            let max_amplitude = (1i64 << (spec.bits_per_sample - 1)) as f32;
            for (i, sample) in reader.samples::<i32>().enumerate() {
                let sample = sample?;
                channels[i % channel_count].push(sample as f32 / max_amplitude);
            }
        }
        SampleFormat::Float => {
            for (i, sample) in reader.samples::<f32>().enumerate() {
                let sample = sample?;
                channels[i % channel_count].push(sample);
            }
        }
    }

    let (mut markers, bext) = super::bwf::read_markers_and_bext(&path);
    // Clamp any out-of-range cue positions to the actual sample count.
    let len = channels.first().map(|c| c.len()).unwrap_or(0);
    for m in &mut markers {
        m.position = m.position.min(len);
    }

    Ok(Document {
        channels,
        sample_rate: spec.sample_rate,
        bits_per_sample: spec.bits_per_sample,
        selection: None,
        cursor: 0,
        dirty: false,
        path: Some(path),
        markers,
        bext,
    })
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

    fn bits(self) -> u16 {
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
struct DitherRng(u32);

impl DitherRng {
    fn new() -> Self {
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
            // Full-scale maps to 2^(bits-1), matching the normalization `load_wav` uses on
            // the way in, so a load→save round-trip at the same depth is stable.
            let scale = (1i64 << (depth.bits() - 1)) as f32;
            let max = scale - 1.0;
            let min = -scale;
            let mut rng = DitherRng::new();
            for i in 0..doc.len_samples() {
                for channel in &doc.channels {
                    let mut v = channel[i] * scale;
                    if dither {
                        v += rng.tpdf();
                    }
                    let q = v.round().clamp(min, max) as i32;
                    writer.write_sample(q)?;
                }
            }
        }
    }
    writer.finalize()?;
    // Append cue/adtl marker chunks and any preserved bext after hound's fmt/data.
    super::bwf::append_aux_chunks(path, &doc.markers, &doc.bext)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
            channels: vec![samples],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bits_per_sample: 32,
            bext: None,
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
