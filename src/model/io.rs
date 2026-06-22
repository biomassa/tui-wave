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

    Ok(Document {
        channels,
        sample_rate: spec.sample_rate,
        selection: None,
            cursor: 0,
        dirty: false,
        path: Some(path),
    })
}

/// Always writes 32-bit float PCM — our in-memory representation is already f32, so this
/// is the only format that round-trips without re-quantizing, regardless of what bit depth
/// the file was originally loaded at.
pub fn save_wav(doc: &Document, path: impl AsRef<Path>) -> color_eyre::Result<()> {
    let spec = WavSpec {
        channels: doc.channel_count().max(1) as u16,
        sample_rate: doc.sample_rate,
        bits_per_sample: 32,
        sample_format: SampleFormat::Float,
    };
    let mut writer = WavWriter::create(path, spec)?;
    for i in 0..doc.len_samples() {
        for channel in &doc.channels {
            writer.write_sample(channel[i])?;
        }
    }
    writer.finalize()?;
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
}
