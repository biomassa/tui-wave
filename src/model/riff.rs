//! Seek-based RIFF chunk walking, shared by `model::bwf` (markers and `bext`) and
//! `model::wavread` (the audio itself).
//!
//! Two things this exists to get right, both of which the previous whole-file
//! `fs::read` approach in `bwf` could not:
//!
//! 1. **It never reads the audio.** Walking a chunk list needs 8 bytes per chunk and a seek;
//!    reading a 20GB file into a `Vec<u8>` to find a 12-byte `cue ` chunk near the end of it
//!    is a 20GB allocation for metadata. [`Riff::read_body`] additionally refuses outright to
//!    materialize anything over [`MAX_BODY_BYTES`], so no future caller can reintroduce that
//!    by asking for the `data` chunk's body.
//!
//! 2. **Sizes are 64-bit.** A plain RIFF header stores chunk sizes as `u32`, capping a WAV at
//!    4GB. `RF64`/`BW64` (EBU Tech 3306) work around that by keeping the RIFF layout but
//!    setting the affected size fields to `0xFFFFFFFF` and carrying the true 64-bit values in
//!    a `ds64` chunk. Everything here works in `u64`, so the same walk serves both.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

/// The size-field value that means "this size lives in `ds64` instead" (EBU Tech 3306).
pub const SIZE_IN_DS64: u32 = 0xFFFF_FFFF;

/// Cap on what [`Riff::read_body`] will allocate. Every chunk this app reads whole is
/// metadata — `fmt `, `ds64`, `cue `, `LIST`/`adtl`, `bext` — and those run to hundreds of
/// bytes, not megabytes. The cap is what makes "read this chunk's body" a safe operation to
/// expose at all: without it, one `read_body` on a `data` chunk is a 20GB allocation.
pub const MAX_BODY_BYTES: u64 = 64 * 1024 * 1024;

/// Which of the three interchangeable top-level forms a file uses. All three are followed by
/// a size field and the `WAVE` form type, and carry the same chunks; they differ only in
/// whether 64-bit sizes are in play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    /// Classic `RIFF` — every size field is a real `u32`.
    Riff,
    /// `RF64`, EBU Tech 3306. Sizes of `0xFFFFFFFF` defer to `ds64`.
    Rf64,
    /// `BW64`, the ITU-R BS.2088 spelling of the same layout.
    Bw64,
}

impl Form {
    /// The form a file's first four bytes name, or `None` if they name no RIFF form at all.
    pub fn from_magic_bytes(magic: &[u8; 4]) -> Option<Form> {
        Form::from_magic(magic)
    }

    fn from_magic(magic: &[u8; 4]) -> Option<Form> {
        match magic {
            b"RIFF" => Some(Form::Riff),
            b"RF64" => Some(Form::Rf64),
            b"BW64" => Some(Form::Bw64),
            _ => None,
        }
    }

    /// Whether a `0xFFFFFFFF` size field should be read as a `ds64` reference rather than as
    /// a literal size.
    pub fn uses_ds64(self) -> bool {
        matches!(self, Form::Rf64 | Form::Bw64)
    }
}

/// The size an `RF64`/`BW64` file's `ds64` chunk gives for the audio.
///
/// Only `data_size` is kept. The chunk also carries `riffSize` and `sampleCount`, and neither is
/// read: the total file length is known from the filesystem, and the frame count is derived from
/// the data chunk's byte size, which is what actually bounds the reads. A file still being written
/// — or one whose writer died mid-take — routinely has a stale `sampleCount`, so preferring it
/// would mean trusting a number the file itself contradicts.
#[derive(Debug, Clone, Copy, Default)]
pub struct Ds64 {
    pub data_size: u64,
}

/// One chunk's identity and where its body sits in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkHeader {
    pub id: [u8; 4],
    /// Body length in bytes, already resolved through `ds64` where that applies.
    pub size: u64,
    /// Absolute byte offset of the first body byte (i.e. just past the 8-byte header).
    pub body_offset: u64,
}

/// An open RIFF/RF64/BW64 file with its chunk list walked.
///
/// The chunk list is collected up front rather than exposed as a lazy iterator: a WAV has a
/// handful of chunks, so the whole list costs a few hundred bytes, and having it in hand
/// avoids every caller needing a mutable borrow of the file just to look for a chunk. The
/// file handle stays open and seekable for whoever needs the audio (`model::wavread`).
pub struct Riff {
    file: File,
    pub form: Form,
    /// Actual length of the file on disk — the ground truth a declared size is checked against.
    pub file_len: u64,
    pub ds64: Option<Ds64>,
    pub chunks: Vec<ChunkHeader>,
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes([
        b[at], b[at + 1], b[at + 2], b[at + 3], b[at + 4], b[at + 5], b[at + 6], b[at + 7],
    ])
}

fn invalid(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

impl Riff {
    /// Opens `path` and walks its chunk list. Fails if the file is not one of the three forms
    /// or is not a `WAVE`.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Riff> {
        let mut file = File::open(path)?;
        let file_len = file.seek(SeekFrom::End(0))?;
        file.seek(SeekFrom::Start(0))?;

        let mut header = [0u8; 12];
        file.read_exact(&mut header)?;
        let magic: [u8; 4] = [header[0], header[1], header[2], header[3]];
        let form = Form::from_magic(&magic).ok_or_else(|| {
            // Naming what was found matters more than usual here: an `RF64` file rejected as
            // "not a RIFF file" is exactly the message that sent the original bug report
            // looking in the wrong place.
            invalid("not a RIFF/RF64/BW64 file")
        })?;
        if &header[8..12] != b"WAVE" {
            return Err(invalid("not a WAVE file"));
        }

        let mut riff = Riff { file, form, file_len, ds64: None, chunks: Vec::new() };
        riff.walk()?;
        Ok(riff)
    }

    /// Reads chunk headers from offset 12 to end of file, resolving `ds64` as it goes.
    fn walk(&mut self) -> io::Result<()> {
        let mut pos: u64 = 12;
        // A `ds64` must be the first chunk in an RF64 file, so it is always parsed before any
        // chunk whose size might refer to it.
        while pos + 8 <= self.file_len {
            self.file.seek(SeekFrom::Start(pos))?;
            let mut head = [0u8; 8];
            if self.file.read_exact(&mut head).is_err() {
                break;
            }
            let id: [u8; 4] = [head[0], head[1], head[2], head[3]];
            let raw_size = u32_at(&head, 4);
            let body_offset = pos + 8;

            if &id == b"ds64" && self.form.uses_ds64() {
                self.ds64 = self.read_ds64(body_offset, raw_size as u64)?;
            }

            let size = self.resolve_size(&id, raw_size, body_offset);
            self.chunks.push(ChunkHeader { id, size, body_offset });

            // Chunks are word-aligned: an odd body is followed by one pad byte.
            let advance = size.saturating_add(size & 1);
            match body_offset.checked_add(advance) {
                Some(next) if next > pos => pos = next,
                // A zero-size chunk would otherwise spin here forever, and an overflowing
                // size means the chunk list is corrupt past this point either way.
                _ => break,
            }
        }
        Ok(())
    }

    /// Turns a raw 32-bit size field into a real byte count.
    ///
    /// Two cases beyond the obvious one, both of which a file still being recorded can
    /// present: an RF64 `data` chunk defers its size to `ds64`, and a declared size can
    /// overrun the file that actually exists. The clamp to end-of-file applies to every
    /// chunk, not just `data` — trusting the file over the header is what makes a truncated
    /// or still-growing take yield the frames that are really there instead of reading off
    /// the end, and it is exactly what a writer that died mid-take leaves behind.
    fn resolve_size(&self, id: &[u8; 4], raw_size: u32, body_offset: u64) -> u64 {
        let to_eof = self.file_len.saturating_sub(body_offset);
        let declared = if raw_size == SIZE_IN_DS64 && self.form.uses_ds64() {
            match (id, self.ds64) {
                (b"data", Some(ds)) => ds.data_size,
                // Only `data` and the RIFF size itself get dedicated `ds64` fields; anything
                // else carrying the sentinel has nothing to resolve to, so treat it as
                // running to the end of the file.
                _ => to_eof,
            }
        } else {
            raw_size as u64
        };
        declared.min(to_eof)
    }

    fn read_ds64(&mut self, body_offset: u64, size: u64) -> io::Result<Option<Ds64>> {
        // riffSize(8) + dataSize(8) + sampleCount(8) + tableLength(4) = 28. The table itself
        // is optional and unused here.
        if size < 28 {
            return Ok(None);
        }
        // Skip `riffSize`; `dataSize` is the second u64.
        self.file.seek(SeekFrom::Start(body_offset + 8))?;
        let mut b = [0u8; 8];
        self.file.read_exact(&mut b)?;
        Ok(Some(Ds64 { data_size: u64_at(&b, 0) }))
    }

    /// The first chunk with this id, if present.
    pub fn find(&self, id: &[u8; 4]) -> Option<ChunkHeader> {
        self.chunks.iter().copied().find(|c| &c.id == id)
    }

    /// Every chunk with this id. `LIST` can legitimately appear more than once (an `adtl`
    /// alongside an `INFO`, say), so finding only the first would silently drop labels.
    pub fn find_all(&self, id: &[u8; 4]) -> Vec<ChunkHeader> {
        self.chunks.iter().copied().filter(|c| &c.id == id).collect()
    }

    /// Reads a chunk's body into memory. Refuses anything over [`MAX_BODY_BYTES`] — see that
    /// constant for why this guard is the point of the function.
    pub fn read_body(&mut self, chunk: &ChunkHeader) -> io::Result<Vec<u8>> {
        if chunk.size > MAX_BODY_BYTES {
            return Err(invalid("chunk too large to read into memory"));
        }
        self.file.seek(SeekFrom::Start(chunk.body_offset))?;
        let mut body = vec![0u8; chunk.size as usize];
        self.file.read_exact(&mut body)?;
        Ok(body)
    }

    /// The audio chunk's absolute offset and resolved byte length.
    pub fn data_chunk(&self) -> Option<(u64, u64)> {
        self.find(b"data").map(|c| (c.body_offset, c.size))
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tuiwave_riff_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Builds a minimal WAVE with the given magic, chunks, and RIFF size field.
    fn build(magic: &[u8; 4], chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(b"WAVE");
        for (id, data) in chunks {
            body.extend_from_slice(*id);
            body.extend_from_slice(&(data.len() as u32).to_le_bytes());
            body.extend_from_slice(data);
            if data.len() % 2 == 1 {
                body.push(0);
            }
        }
        let mut out = Vec::new();
        out.extend_from_slice(magic);
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = dir.join(name);
        File::create(&p).unwrap().write_all(bytes).unwrap();
        p
    }

    #[test]
    fn walks_a_plain_riff_chunk_list() {
        let dir = tmp_dir("plain");
        let bytes = build(
            b"RIFF",
            &[(b"fmt ", vec![1u8; 16]), (b"data", vec![0u8; 40])],
        );
        let riff = Riff::open(write(&dir, "a.wav", &bytes)).unwrap();
        assert_eq!(riff.form, Form::Riff);
        assert_eq!(riff.chunks.len(), 2);
        assert_eq!(riff.find(b"fmt ").unwrap().size, 16);
        assert_eq!(riff.data_chunk().unwrap().1, 40);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The real layout the reported files use: a `JUNK` placeholder ahead of `fmt `, reserved
    /// so the writer can convert it to `ds64` if the take passes 4GB. Walking must step over
    /// it and still find `fmt `.
    #[test]
    fn skips_a_junk_placeholder_before_fmt() {
        let dir = tmp_dir("junk");
        let bytes = build(
            b"RIFF",
            &[
                (b"JUNK", vec![0u8; 52]),
                (b"fmt ", vec![3u8; 16]),
                (b"data", vec![0u8; 8]),
            ],
        );
        let riff = Riff::open(write(&dir, "j.wav", &bytes)).unwrap();
        assert_eq!(riff.chunks.len(), 3);
        assert_eq!(riff.chunks[0].id, *b"JUNK");
        assert_eq!(riff.find(b"fmt ").unwrap().size, 16);
        assert_eq!(riff.data_chunk().unwrap().1, 8);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An odd-sized chunk is followed by a pad byte that is not part of its body. Getting this
    /// wrong shifts every subsequent chunk by one and the walk finds nothing.
    #[test]
    fn honours_word_alignment_after_an_odd_chunk() {
        let dir = tmp_dir("odd");
        let bytes = build(
            b"RIFF",
            &[(b"bext", vec![7u8; 5]), (b"data", vec![0u8; 4])],
        );
        let mut riff = Riff::open(write(&dir, "o.wav", &bytes)).unwrap();
        let bext = riff.find(b"bext").unwrap();
        assert_eq!(bext.size, 5, "the pad byte is not part of the body");
        assert_eq!(riff.read_body(&bext).unwrap(), vec![7u8; 5]);
        assert!(riff.data_chunk().is_some(), "the next chunk must still be found");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// RF64: the `data` size field is the `0xFFFFFFFF` sentinel and the real length comes from
    /// `ds64`. This is the case hound cannot represent at all.
    #[test]
    fn resolves_an_rf64_data_size_through_ds64() {
        let dir = tmp_dir("rf64");
        let data_len = 200usize;
        let mut ds64 = Vec::new();
        ds64.extend_from_slice(&0u64.to_le_bytes()); // riffSize (unused here)
        ds64.extend_from_slice(&(data_len as u64).to_le_bytes());
        ds64.extend_from_slice(&50u64.to_le_bytes()); // sampleCount
        ds64.extend_from_slice(&0u32.to_le_bytes()); // tableLength
        let mut bytes = build(
            b"RF64",
            &[
                (b"ds64", ds64),
                (b"fmt ", vec![1u8; 16]),
                (b"data", vec![0u8; data_len]),
            ],
        );
        // Stamp the sentinel over the data chunk's own size field, as a real RF64 writer does.
        let at = bytes.len() - data_len - 4;
        bytes[at..at + 4].copy_from_slice(&SIZE_IN_DS64.to_le_bytes());

        let riff = Riff::open(write(&dir, "big.wav", &bytes)).unwrap();
        assert_eq!(riff.form, Form::Rf64);
        assert_eq!(riff.ds64.unwrap().data_size, data_len as u64);
        assert_eq!(riff.data_chunk().unwrap().1, data_len as u64);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn accepts_the_bw64_spelling_of_the_same_layout() {
        let dir = tmp_dir("bw64");
        let bytes = build(b"BW64", &[(b"fmt ", vec![1u8; 16]), (b"data", vec![0u8; 4])]);
        let riff = Riff::open(write(&dir, "b.wav", &bytes)).unwrap();
        assert_eq!(riff.form, Form::Bw64);
        assert!(riff.form.uses_ds64());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A file cut short mid-recording declares more `data` than it holds. The walk must report
    /// what is actually there rather than a length that reads past end of file.
    #[test]
    fn clamps_a_data_size_that_overruns_the_file() {
        let dir = tmp_dir("trunc");
        let mut bytes = build(b"RIFF", &[(b"fmt ", vec![1u8; 16]), (b"data", vec![0u8; 100])]);
        bytes.truncate(bytes.len() - 60); // 40 of the declared 100 data bytes survive
        let riff = Riff::open(write(&dir, "t.wav", &bytes)).unwrap();
        assert_eq!(riff.data_chunk().unwrap().1, 40);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The guard that makes `read_body` safe to expose: a `data` chunk must never be
    /// materialized, however it is asked for.
    #[test]
    fn refuses_to_read_a_body_over_the_cap() {
        let dir = tmp_dir("cap");
        let bytes = build(b"RIFF", &[(b"data", vec![0u8; 16])]);
        let mut riff = Riff::open(write(&dir, "c.wav", &bytes)).unwrap();
        let mut huge = riff.find(b"data").unwrap();
        huge.size = MAX_BODY_BYTES + 1;
        assert!(riff.read_body(&huge).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_non_riff_and_non_wave() {
        let dir = tmp_dir("bad");
        assert!(Riff::open(write(&dir, "x.wav", b"this is not a RIFF file at all")).is_err());
        let mut not_wave = build(b"RIFF", &[]);
        not_wave[8..12].copy_from_slice(b"AVI ");
        assert!(Riff::open(write(&dir, "y.wav", &not_wave)).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A zero-size chunk must not stall the walk — the advance has to make progress or stop.
    #[test]
    fn a_zero_size_chunk_does_not_loop_forever() {
        let dir = tmp_dir("zero");
        let bytes = build(b"RIFF", &[(b"nul ", Vec::new()), (b"data", vec![0u8; 4])]);
        let riff = Riff::open(write(&dir, "z.wav", &bytes)).unwrap();
        assert!(!riff.chunks.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}

