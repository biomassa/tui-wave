//! WAV decoding: a header probe and framed random-access reads, over `RIFF`, `RF64` and
//! `BW64` alike.
//!
//! This replaces `hound` on the read side. Two reasons it had to, both fatal for the large
//! multichannel takes this exists to open:
//!
//! - **hound cannot represent a file over 4GB.** Its `data_len` and `num_samples` are `u32`
//!   (`read.rs:229,600`), and it rejects any file whose first four bytes are not literally
//!   `RIFF` (`read.rs:272`) — so an `RF64` file fails instantly, before a single sample is
//!   read. A 20GB file is 5.37 billion interleaved samples, past `u32::MAX` regardless.
//! - **Nothing in hound offers framed random access.** `WavReader::seek` takes a `u32` sample
//!   index, so even within 4GB it cannot serve the windowed reads a disk-backed document needs
//!   (`model::stream`). Both modes reading through one decoder is what stops them disagreeing
//!   about what a file contains.
//!
//! `hound` still writes (`model::io::save_wav_with`), and is still used in tests as the
//! reference this reader is checked against — see `matches_hound_on_every_fixture`.
//!
//! Normalization matches what `load_wav` has always done: integer PCM is divided by
//! `1 << (bits - 1)`, so a load→`io::quantize`→save round-trip at the same depth is stable.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use super::riff::Riff;

/// How the `data` chunk's bytes encode each sample. Distinct from the *bit depth* a document
/// reports, because 32 bits can mean either integer or float and they normalize differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFormat {
    /// Signed little-endian integer PCM, 8/16/24/32-bit.
    IntPcm { bits: u16 },
    Float32,
    Float64,
}

impl SourceFormat {
    fn bytes_per_sample(self) -> usize {
        match self {
            SourceFormat::IntPcm { bits } => (bits as usize + 7) / 8,
            SourceFormat::Float32 => 4,
            SourceFormat::Float64 => 8,
        }
    }

    /// Reads one sample from `raw` and normalizes it to f32 in [-1.0, 1.0].
    fn decode(self, raw: &[u8]) -> f32 {
        match self {
            SourceFormat::IntPcm { bits } => {
                // Assembled little-endian, then sign-extended from the top byte. 24-bit is the
                // case that makes a generic path worth it: there is no `i24`, so the sign bit
                // has to be propagated by hand.
                let mut v: i32 = 0;
                for (i, &b) in raw.iter().enumerate() {
                    v |= (b as i32) << (8 * i);
                }
                let used = raw.len() * 8;
                if used < 32 {
                    let shift = 32 - used;
                    v = (v << shift) >> shift;
                }
                // 8-bit WAV PCM is unsigned by definition, with 128 as zero — the one depth
                // that is not two's complement.
                if bits == 8 {
                    return (raw[0] as f32 - 128.0) / 128.0;
                }
                v as f32 / (1i64 << (bits - 1)) as f32
            }
            SourceFormat::Float32 => {
                f32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]])
            }
            SourceFormat::Float64 => f64::from_le_bytes([
                raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
            ]) as f32,
        }
    }
}

/// Everything the header says about a file, with no samples read.
///
/// Cheap enough — a handful of seeks — to run before deciding whether a file can be held in
/// RAM at all, which is exactly what `App::load_file` uses it for.
#[derive(Debug, Clone, Copy)]
pub struct WavInfo {
    pub channels: usize,
    pub sample_rate: u32,
    /// Depth as the header declares it, for `Document::bits_per_sample`.
    pub bits_per_sample: u16,
    pub format: SourceFormat,
    /// Absolute byte offset of the first frame.
    pub data_offset: u64,
    /// Frames actually present in the file — derived from the `data` chunk's real byte length,
    /// not from any declared sample count, so a truncated or still-growing take reports what
    /// it really holds.
    pub frame_count: u64,
    pub bytes_per_frame: usize,
}

impl WavInfo {
    /// Bytes a fully-resident `Vec<Vec<f32>>` of this file would occupy.
    ///
    /// The working format is f32 regardless of source depth, so this is *not* the file size:
    /// 24-bit inflates by 4/3, and the 32-bit float files this was written for are 1:1.
    pub fn resident_bytes(&self) -> u64 {
        self.frame_count
            .saturating_mul(self.channels as u64)
            .saturating_mul(std::mem::size_of::<f32>() as u64)
    }
}

fn invalid(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// `KSDATAFORMAT_SUBTYPE_PCM` — the first two bytes of the GUID are what distinguishes it.
const SUBTYPE_PCM: u16 = 0x0001;
/// `KSDATAFORMAT_SUBTYPE_IEEE_FLOAT`.
const SUBTYPE_IEEE_FLOAT: u16 = 0x0003;

const WAVE_FORMAT_PCM: u16 = 0x0001;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

/// Parses `fmt ` and locates `data`, without reading any audio.
pub fn probe(path: impl AsRef<Path>) -> io::Result<WavInfo> {
    let mut riff = Riff::open(path)?;
    let fmt_header = riff.find(b"fmt ").ok_or_else(|| invalid("no fmt chunk"))?;
    let fmt = riff.read_body(&fmt_header)?;
    if fmt.len() < 16 {
        return Err(invalid("fmt chunk too short"));
    }

    let mut tag = u16_at(&fmt, 0);
    let channels = u16_at(&fmt, 2) as usize;
    let sample_rate = u32_at(&fmt, 4);
    let block_align = u16_at(&fmt, 12) as usize;
    let bits_per_sample = u16_at(&fmt, 14);

    // `WAVE_FORMAT_EXTENSIBLE` is not an exotic case to skip: it is what writers are expected
    // to emit above two channels, so a 58-channel file is *more* likely to use it than not.
    // The real format hides in the SubFormat GUID, whose first two bytes carry the tag the
    // non-extensible header would have held.
    if tag == WAVE_FORMAT_EXTENSIBLE {
        // 16 bytes of WAVEFORMAT + cbSize(2) + validBits(2) + channelMask(4) + GUID(16)
        if fmt.len() < 40 {
            return Err(invalid("WAVE_FORMAT_EXTENSIBLE fmt chunk too short for its GUID"));
        }
        tag = u16_at(&fmt, 24);
        if !matches!(tag, SUBTYPE_PCM | SUBTYPE_IEEE_FLOAT) {
            return Err(invalid("unsupported WAVE_FORMAT_EXTENSIBLE subformat"));
        }
    }

    if channels == 0 {
        return Err(invalid("fmt chunk declares zero channels"));
    }
    if sample_rate == 0 {
        return Err(invalid("fmt chunk declares a zero sample rate"));
    }

    let format = match (tag, bits_per_sample) {
        (WAVE_FORMAT_PCM, 8) | (WAVE_FORMAT_PCM, 16) | (WAVE_FORMAT_PCM, 24)
        | (WAVE_FORMAT_PCM, 32) => SourceFormat::IntPcm { bits: bits_per_sample },
        (WAVE_FORMAT_IEEE_FLOAT, 32) => SourceFormat::Float32,
        (WAVE_FORMAT_IEEE_FLOAT, 64) => SourceFormat::Float64,
        _ => return Err(invalid("unsupported WAV sample format or bit depth")),
    };

    // Trust `blockAlign` when it is consistent, since a writer may pad samples to a wider
    // container than the depth implies; fall back to the arithmetic when it is absent or
    // nonsense (which does happen in the wild).
    let implied = format.bytes_per_sample() * channels;
    let bytes_per_frame = if block_align >= implied && block_align % channels == 0 {
        block_align
    } else {
        implied
    };

    let (data_offset, data_size) = riff.data_chunk().ok_or_else(|| invalid("no data chunk"))?;
    let frame_count = data_size / bytes_per_frame as u64;

    Ok(WavInfo {
        channels,
        sample_rate,
        bits_per_sample,
        format,
        data_offset,
        frame_count,
        bytes_per_frame,
    })
}

/// An open file positioned for framed reads.
///
/// Holds only its own scratch byte buffer, so an instance costs kilobytes regardless of how
/// large the file is. That is the property both callers depend on: `load_wav` reads the whole
/// file through one of these in blocks, and `model::stream` keeps one alive for the lifetime
/// of a document to serve visible windows on demand.
pub struct WavFrames {
    file: File,
    pub info: WavInfo,
    scratch: Vec<u8>,
}

/// Frames per block for whole-file reads. 64Ki frames of a 58-channel float file is ~15MB of
/// scratch — large enough that per-read overhead vanishes, small enough to stay off the heap's
/// radar next to the pyramid being built from it.
pub const READ_BLOCK_FRAMES: usize = 64 * 1024;

impl WavFrames {
    pub fn open(path: impl AsRef<Path>) -> io::Result<WavFrames> {
        let info = probe(&path)?;
        let file = File::open(&path)?;
        Ok(WavFrames { file, info, scratch: Vec::new() })
    }

    pub fn info(&self) -> WavInfo {
        self.info
    }

    /// Decodes `[first_frame, first_frame + frames)` into `out`, one `Vec` per channel,
    /// **appending** to whatever is already there. Returns the number of frames actually read,
    /// which is short at end of file.
    ///
    /// `out` must have at least `info.channels` entries; extra entries are left alone, so a
    /// caller reading a channel subset can pass a full-width buffer and ignore the rest.
    pub fn read_into(
        &mut self,
        first_frame: u64,
        frames: usize,
        out: &mut [Vec<f32>],
    ) -> io::Result<usize> {
        if out.len() < self.info.channels {
            return Err(invalid("output buffer has fewer channels than the file"));
        }
        let available = self.info.frame_count.saturating_sub(first_frame);
        let frames = (frames as u64).min(available) as usize;
        if frames == 0 {
            return Ok(0);
        }

        let bpf = self.info.bytes_per_frame;
        let byte_len = frames * bpf;
        self.scratch.resize(byte_len, 0);
        let offset = self.info.data_offset + first_frame * bpf as u64;
        self.file.seek(SeekFrom::Start(offset))?;
        // `read_exact` rather than `read`: the frame count is already clamped to what the data
        // chunk holds, so a short read here means the file changed under us, not EOF.
        self.file.read_exact(&mut self.scratch)?;

        let format = self.info.format;
        let sample_bytes = format.bytes_per_sample();
        for (ch, out_ch) in out.iter_mut().enumerate().take(self.info.channels) {
            out_ch.reserve(frames);
            let base = ch * sample_bytes;
            for f in 0..frames {
                let at = f * bpf + base;
                out_ch.push(format.decode(&self.scratch[at..at + sample_bytes]));
            }
        }
        Ok(frames)
    }

    /// Decodes `[first_frame, first_frame + frames)` of a **single** channel, appending to
    /// `out`. The windowed-read primitive for `model::stream`, where only the handful of
    /// channels currently on screen are ever wanted and decoding all 58 to show 6 would be
    /// most of the work thrown away.
    pub fn read_channel_into(
        &mut self,
        channel: usize,
        first_frame: u64,
        frames: usize,
        out: &mut Vec<f32>,
    ) -> io::Result<usize> {
        if channel >= self.info.channels {
            return Err(invalid("channel index out of range"));
        }
        let available = self.info.frame_count.saturating_sub(first_frame);
        let frames = (frames as u64).min(available) as usize;
        if frames == 0 {
            return Ok(0);
        }

        let bpf = self.info.bytes_per_frame;
        self.scratch.resize(frames * bpf, 0);
        let offset = self.info.data_offset + first_frame * bpf as u64;
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(&mut self.scratch)?;

        let format = self.info.format;
        let sample_bytes = format.bytes_per_sample();
        let base = channel * sample_bytes;
        out.reserve(frames);
        for f in 0..frames {
            let at = f * bpf + base;
            out.push(format.decode(&self.scratch[at..at + sample_bytes]));
        }
        Ok(frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every fixture must decode identically through the new reader and through hound. This is
    /// the safety net for replacing a battle-tested reader: the fixtures cover mono, stereo,
    /// 16-bit int and 32-bit float, with and without extra chunks.
    #[test]
    fn matches_hound_on_every_fixture() {
        for name in ["mono_sine.wav", "stereo_sine.wav"] {
            let path = format!("tests/fixtures/{name}");

            let mut hound = hound::WavReader::open(&path).unwrap();
            let spec = hound.spec();
            let channel_count = spec.channels as usize;
            let mut expected: Vec<Vec<f32>> = vec![Vec::new(); channel_count];
            match spec.sample_format {
                hound::SampleFormat::Int => {
                    let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
                    for (i, s) in hound.samples::<i32>().enumerate() {
                        expected[i % channel_count].push(s.unwrap() as f32 / max);
                    }
                }
                hound::SampleFormat::Float => {
                    for (i, s) in hound.samples::<f32>().enumerate() {
                        expected[i % channel_count].push(s.unwrap());
                    }
                }
            }

            let mut frames = WavFrames::open(&path).unwrap();
            let info = frames.info();
            assert_eq!(info.channels, channel_count, "{name}: channel count");
            assert_eq!(info.sample_rate, spec.sample_rate, "{name}: sample rate");
            assert_eq!(info.bits_per_sample, spec.bits_per_sample, "{name}: depth");
            assert_eq!(
                info.frame_count as usize,
                expected[0].len(),
                "{name}: frame count"
            );

            let mut got: Vec<Vec<f32>> = vec![Vec::new(); channel_count];
            let mut at = 0u64;
            while at < info.frame_count {
                let n = frames.read_into(at, 1000, &mut got).unwrap();
                assert!(n > 0, "{name}: read must make progress");
                at += n as u64;
            }
            assert_eq!(got, expected, "{name}: samples must be bit-identical to hound's");
        }
    }

    /// Reading a single channel must agree with the same channel of a full read — the property
    /// `model::stream`'s windowed display depends on.
    #[test]
    fn single_channel_reads_match_the_full_read() {
        let mut frames = WavFrames::open("tests/fixtures/stereo_sine.wav").unwrap();
        let info = frames.info();
        let mut all: Vec<Vec<f32>> = vec![Vec::new(); info.channels];
        frames.read_into(0, info.frame_count as usize, &mut all).unwrap();

        for ch in 0..info.channels {
            let mut one = Vec::new();
            frames.read_channel_into(ch, 0, info.frame_count as usize, &mut one).unwrap();
            assert_eq!(one, all[ch], "channel {ch} read alone must match");
        }
    }

    /// A mid-file window must return exactly that window — the operation every zoomed-in
    /// redraw performs. An off-by-one in the seek arithmetic would still look plausible on
    /// screen, so it is asserted against the full read rather than eyeballed.
    #[test]
    fn a_windowed_read_lands_on_the_right_frames() {
        let mut frames = WavFrames::open("tests/fixtures/stereo_sine.wav").unwrap();
        let info = frames.info();
        let mut all: Vec<Vec<f32>> = vec![Vec::new(); info.channels];
        frames.read_into(0, info.frame_count as usize, &mut all).unwrap();

        for &(start, len) in &[(0u64, 10usize), (1, 1), (5000, 256), (info.frame_count - 3, 3)] {
            let mut window = Vec::new();
            let n = frames.read_channel_into(0, start, len, &mut window).unwrap();
            assert_eq!(n, len);
            assert_eq!(
                window,
                all[0][start as usize..start as usize + len],
                "window at {start}+{len}"
            );
        }
    }

    /// Reading past the end returns short rather than erroring, and reading entirely past it
    /// returns nothing. Both happen at the right edge of a view when the file's last frame
    /// falls mid-column.
    #[test]
    fn reads_clamp_at_end_of_file() {
        let mut frames = WavFrames::open("tests/fixtures/mono_sine.wav").unwrap();
        let total = frames.info().frame_count;

        let mut out = Vec::new();
        assert_eq!(frames.read_channel_into(0, total - 5, 100, &mut out).unwrap(), 5);
        assert_eq!(out.len(), 5);

        out.clear();
        assert_eq!(frames.read_channel_into(0, total, 100, &mut out).unwrap(), 0);
        assert!(out.is_empty());
        assert_eq!(frames.read_channel_into(0, total + 1000, 10, &mut out).unwrap(), 0);
    }

    #[test]
    fn resident_bytes_accounts_for_f32_expansion() {
        // 16-bit stereo: 2 bytes on disk per sample, 4 in memory.
        let info = probe("tests/fixtures/stereo_sine.wav").unwrap();
        assert_eq!(info.resident_bytes(), info.frame_count * 2 * 4);
    }

    /// Rewrites a plain RIFF WAV in place into the RF64 form: magic → `RF64`, the `JUNK`
    /// placeholder → `ds64` carrying the real 64-bit sizes, and the `data` size field → the
    /// sentinel. This is exactly the conversion the recorder performs once a take passes 4GB,
    /// so a file built this way is the same shape as the ones that would not open — just small
    /// enough to keep in a test.
    fn to_rf64(bytes: &mut Vec<u8>, junk_at: usize, data_size_at: usize, frames: u64) {
        let data_size = u32::from_le_bytes(
            bytes[data_size_at..data_size_at + 4].try_into().unwrap(),
        ) as u64;
        bytes[0..4].copy_from_slice(b"RF64");
        let riff_size = bytes.len() as u64 - 8;

        bytes[junk_at..junk_at + 4].copy_from_slice(b"ds64");
        // Body: riffSize(8) + dataSize(8) + sampleCount(8) + tableLength(4) = 28.
        bytes[junk_at + 4..junk_at + 8].copy_from_slice(&28u32.to_le_bytes());
        let body = junk_at + 8;
        bytes[body..body + 8].copy_from_slice(&riff_size.to_le_bytes());
        bytes[body + 8..body + 16].copy_from_slice(&data_size.to_le_bytes());
        bytes[body + 16..body + 24].copy_from_slice(&frames.to_le_bytes());
        bytes[body + 24..body + 28].copy_from_slice(&0u32.to_le_bytes());

        bytes[4..8].copy_from_slice(&super::super::riff::SIZE_IN_DS64.to_le_bytes());
        bytes[data_size_at..data_size_at + 4]
            .copy_from_slice(&super::super::riff::SIZE_IN_DS64.to_le_bytes());
    }

    /// Builds a 3-channel 32-bit-float WAVE with a `JUNK` placeholder ahead of `fmt `, which is
    /// the layout the reported files use. Returns the bytes plus the offsets `to_rf64` needs.
    fn float_wav_with_junk(frames: usize, channels: usize) -> (Vec<u8>, usize, usize, Vec<Vec<f32>>) {
        let expected: Vec<Vec<f32>> = (0..channels)
            .map(|c| {
                (0..frames)
                    .map(|f| ((f as f32 * 0.01) + c as f32).sin() * 0.5)
                    .collect()
            })
            .collect();

        let mut body: Vec<u8> = Vec::new();
        body.extend_from_slice(b"WAVE");

        let junk_at = 4 + 8; // after 'RIFF'/size, then 'WAVE'
        body.extend_from_slice(b"JUNK");
        body.extend_from_slice(&52u32.to_le_bytes());
        body.extend_from_slice(&[0u8; 52]);

        body.extend_from_slice(b"fmt ");
        body.extend_from_slice(&16u32.to_le_bytes());
        body.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
        body.extend_from_slice(&(channels as u16).to_le_bytes());
        body.extend_from_slice(&48000u32.to_le_bytes());
        body.extend_from_slice(&((48000 * channels * 4) as u32).to_le_bytes());
        body.extend_from_slice(&((channels * 4) as u16).to_le_bytes());
        body.extend_from_slice(&32u16.to_le_bytes());

        body.extend_from_slice(b"data");
        let data_size_at = 8 + body.len();
        body.extend_from_slice(&((frames * channels * 4) as u32).to_le_bytes());
        for f in 0..frames {
            for c in 0..channels {
                body.extend_from_slice(&expected[c][f].to_le_bytes());
            }
        }

        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        (out, junk_at, data_size_at, expected)
    }

    /// The capability this module exists for: an `RF64` file must decode, and decode to exactly
    /// what the same audio in a plain `RIFF` container decodes to. hound rejects the RF64 form
    /// outright on its first four bytes, which is why files past 4GB silently failed to open.
    #[test]
    fn decodes_an_rf64_file_identically_to_the_same_audio_as_riff() {
        let dir = std::env::temp_dir().join(format!("tuiwave_rf64_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let frames = 500usize;
        let channels = 3usize;
        let (riff_bytes, junk_at, data_size_at, expected) = float_wav_with_junk(frames, channels);

        let plain = dir.join("plain.wav");
        std::fs::write(&plain, &riff_bytes).unwrap();

        let mut rf64_bytes = riff_bytes.clone();
        to_rf64(&mut rf64_bytes, junk_at, data_size_at, frames as u64);
        let big = dir.join("big.wav");
        std::fs::write(&big, &rf64_bytes).unwrap();
        assert_eq!(&rf64_bytes[0..4], b"RF64", "sanity: the fixture really is RF64");
        assert!(
            hound::WavReader::open(&big).is_err(),
            "sanity: hound must be unable to read it — that is the bug being fixed"
        );

        for path in [&plain, &big] {
            let mut f = WavFrames::open(path).unwrap();
            let info = f.info();
            assert_eq!(info.channels, channels, "{path:?}");
            assert_eq!(info.sample_rate, 48000, "{path:?}");
            assert_eq!(info.frame_count, frames as u64, "{path:?}");
            assert_eq!(info.format, SourceFormat::Float32, "{path:?}");
            let mut got: Vec<Vec<f32>> = vec![Vec::new(); channels];
            f.read_into(0, frames, &mut got).unwrap();
            assert_eq!(got, expected, "{path:?}: samples must match");
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    /// And it must work all the way up through `load_wav`, since that is what the app calls.
    #[test]
    fn load_wav_opens_an_rf64_file() {
        let dir = std::env::temp_dir().join(format!("tuiwave_rf64_load_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut bytes, junk_at, data_size_at, expected) = float_wav_with_junk(300, 4);
        to_rf64(&mut bytes, junk_at, data_size_at, 300);
        let path = dir.join("take.wav");
        std::fs::write(&path, &bytes).unwrap();

        let doc = super::super::io::load_wav(&path).unwrap();
        assert_eq!(doc.channel_count(), 4);
        assert_eq!(doc.len_samples(), 300);
        assert_eq!(doc.sample_rate, 48000);
        assert_eq!(doc.channels, expected);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `WAVE_FORMAT_EXTENSIBLE` is what writers are expected to emit above two channels, so a
    /// high-channel-count file is more likely to use it than not. The real format tag hides in
    /// the SubFormat GUID; missing that would reject the file as an unsupported format.
    #[test]
    fn resolves_wave_format_extensible_through_its_subformat_guid() {
        let dir = std::env::temp_dir().join(format!("tuiwave_ext_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let channels = 58usize;
        let frames = 64usize;

        let mut body: Vec<u8> = Vec::new();
        body.extend_from_slice(b"WAVE");
        body.extend_from_slice(b"fmt ");
        body.extend_from_slice(&40u32.to_le_bytes()); // extensible fmt is 40 bytes
        body.extend_from_slice(&0xFFFEu16.to_le_bytes()); // WAVE_FORMAT_EXTENSIBLE
        body.extend_from_slice(&(channels as u16).to_le_bytes());
        body.extend_from_slice(&48000u32.to_le_bytes());
        body.extend_from_slice(&((48000 * channels * 4) as u32).to_le_bytes());
        body.extend_from_slice(&((channels * 4) as u16).to_le_bytes());
        body.extend_from_slice(&32u16.to_le_bytes());
        body.extend_from_slice(&22u16.to_le_bytes()); // cbSize
        body.extend_from_slice(&32u16.to_le_bytes()); // validBitsPerSample
        body.extend_from_slice(&0u32.to_le_bytes()); // channelMask
        // KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: 00000003-0000-0010-8000-00aa00389b71
        body.extend_from_slice(&3u16.to_le_bytes());
        body.extend_from_slice(&[0u8; 14]);

        body.extend_from_slice(b"data");
        body.extend_from_slice(&((frames * channels * 4) as u32).to_le_bytes());
        for f in 0..frames {
            for c in 0..channels {
                body.extend_from_slice(&((f + c) as f32 * 0.001).to_le_bytes());
            }
        }

        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        let path = dir.join("ext.wav");
        std::fs::write(&path, &out).unwrap();

        let info = probe(&path).unwrap();
        assert_eq!(info.channels, 58);
        assert_eq!(info.format, SourceFormat::Float32, "the GUID says IEEE float");
        assert_eq!(info.frame_count, frames as u64);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 24-bit is the depth with no native Rust type: the sign bit has to be propagated by hand
    /// from the top of three bytes, and getting it wrong turns every negative sample positive.
    #[test]
    fn sign_extends_24_bit_samples() {
        let dir = std::env::temp_dir().join(format!("tuiwave_24_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // -1 (0xFFFFFF), +1, the most negative value, and the most positive.
        let raw: [[u8; 3]; 4] = [
            [0xFF, 0xFF, 0xFF],
            [0x01, 0x00, 0x00],
            [0x00, 0x00, 0x80],
            [0xFF, 0xFF, 0x7F],
        ];
        let mut body: Vec<u8> = Vec::new();
        body.extend_from_slice(b"WAVE");
        body.extend_from_slice(b"fmt ");
        body.extend_from_slice(&16u32.to_le_bytes());
        body.extend_from_slice(&1u16.to_le_bytes()); // PCM
        body.extend_from_slice(&1u16.to_le_bytes()); // mono
        body.extend_from_slice(&44100u32.to_le_bytes());
        body.extend_from_slice(&(44100u32 * 3).to_le_bytes());
        body.extend_from_slice(&3u16.to_le_bytes());
        body.extend_from_slice(&24u16.to_le_bytes());
        body.extend_from_slice(b"data");
        body.extend_from_slice(&((raw.len() * 3) as u32).to_le_bytes());
        for r in &raw {
            body.extend_from_slice(r);
        }
        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        let path = dir.join("d24.wav");
        std::fs::write(&path, &out).unwrap();

        let mut f = WavFrames::open(&path).unwrap();
        let mut got: Vec<Vec<f32>> = vec![Vec::new()];
        f.read_into(0, 4, &mut got).unwrap();
        let scale = (1i64 << 23) as f32;
        assert!((got[0][0] - (-1.0 / scale)).abs() < 1e-9, "0xFFFFFF must be -1, got {}", got[0][0]);
        assert!((got[0][1] - (1.0 / scale)).abs() < 1e-9);
        assert!((got[0][2] - (-1.0)).abs() < 1e-6, "0x800000 must be full-scale negative");
        assert!(got[0][3] > 0.999 && got[0][3] < 1.0);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_a_file_with_no_fmt_or_data_chunk() {
        let dir = std::env::temp_dir().join(format!("tuiwave_wavread_bad_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let mut only_header = Vec::new();
        only_header.extend_from_slice(b"RIFF");
        only_header.extend_from_slice(&4u32.to_le_bytes());
        only_header.extend_from_slice(b"WAVE");
        let p = dir.join("empty.wav");
        std::fs::write(&p, &only_header).unwrap();
        assert!(probe(&p).is_err(), "a WAVE with no fmt chunk must not probe");

        std::fs::remove_dir_all(&dir).ok();
    }
}

