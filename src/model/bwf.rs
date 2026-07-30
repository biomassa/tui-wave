//! Broadcast-WAV extras that `hound` doesn't handle: `cue `/`adtl` timeline markers and
//! the `bext` broadcast-metadata chunk.
//!
//! Reading walks the RIFF chunk list directly. Writing keeps `hound` responsible for the
//! `fmt `/`data` chunks (so float/int encoding stays battle-tested) and *appends* the extra
//! chunks afterward, patching the top-level RIFF size — readers that don't understand these
//! chunks simply skip them, so sample data still round-trips everywhere.

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use super::document::Marker;

fn read_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Reads timeline markers (`cue ` points joined with `adtl`/`labl` labels) and the raw
/// `bext` chunk bytes (header excluded) from a WAV file. Returns empties on any malformed or
/// missing chunk rather than erroring — markers are optional metadata.
///
/// Only the metadata chunks are ever read into memory, via `model::riff`'s seek-based walk.
/// This used to `fs::read` the whole file to find them, which on a large multichannel take
/// meant a second full-size allocation *on top of* the samples that had just been decoded —
/// 20GB, to locate a 12-byte `cue ` chunk. `riff::read_body`'s size cap is what now makes
/// that class of mistake impossible rather than merely avoided here.
pub fn read_markers_and_bext(path: impl AsRef<Path>) -> (Vec<Marker>, Option<Vec<u8>>) {
    let Ok(mut riff) = super::riff::Riff::open(path) else {
        return (Vec::new(), None);
    };

    let mut cue_positions: Vec<(u32, u32)> = Vec::new(); // (id, sample offset)
    let mut labels: Vec<(u32, String)> = Vec::new(); // (id, text)
    let mut bext: Option<Vec<u8>> = None;

    if let Some(header) = riff.find(b"cue ") {
        if let Ok(chunk) = riff.read_body(&header) {
            if chunk.len() >= 4 {
                let n = read_u32(&chunk, 0) as usize;
                for i in 0..n {
                    let base = 4 + i * 24;
                    if base + 24 <= chunk.len() {
                        let cue_id = read_u32(&chunk, base);
                        // dwSampleOffset is the last u32 of the 24-byte record.
                        let sample_offset = read_u32(&chunk, base + 20);
                        cue_positions.push((cue_id, sample_offset));
                    }
                }
            }
        }
    }

    // Every `LIST`, not just the first: a file can carry an `INFO` list alongside the `adtl`
    // one, in either order, and stopping at the first would drop every label.
    for header in riff.find_all(b"LIST") {
        let Ok(chunk) = riff.read_body(&header) else { continue };
        if chunk.len() < 4 || &chunk[0..4] != b"adtl" {
            continue;
        }
        let mut p = 4;
        while p + 8 <= chunk.len() {
            let sub_id = &chunk[p..p + 4];
            let sub_size = read_u32(&chunk, p + 4) as usize;
            let sub_body = p + 8;
            if sub_body + sub_size > chunk.len() {
                break;
            }
            if sub_id == b"labl" && sub_size >= 4 {
                let label_id = read_u32(&chunk, sub_body);
                let text_bytes = &chunk[sub_body + 4..sub_body + sub_size];
                let end = text_bytes.iter().position(|&c| c == 0).unwrap_or(text_bytes.len());
                let text = String::from_utf8_lossy(&text_bytes[..end]).into_owned();
                labels.push((label_id, text));
            }
            p = sub_body + sub_size + (sub_size & 1); // word-align
        }
    }

    if let Some(header) = riff.find(b"bext") {
        if let Ok(body) = riff.read_body(&header) {
            bext = Some(body);
        }
    }

    let mut markers: Vec<Marker> = cue_positions
        .into_iter()
        .map(|(id, offset)| {
            let label = labels
                .iter()
                .find(|(lid, _)| *lid == id)
                .map(|(_, t)| t.clone())
                .unwrap_or_else(|| format!("Marker {id}"));
            Marker { position: offset as usize, label }
        })
        .collect();
    markers.sort_by_key(|m| m.position);
    (markers, bext)
}

fn pad_to_even(out: &mut Vec<u8>) {
    if out.len() % 2 == 1 {
        out.push(0);
    }
}

/// Appends `cue `, `LIST`/`adtl` and `bext` chunks to a WAV that `hound` already wrote, then
/// patches the top-level RIFF size. No-op when there's nothing to add.
pub fn append_aux_chunks(
    path: impl AsRef<Path>,
    markers: &[Marker],
    bext: &Option<Vec<u8>>,
) -> std::io::Result<()> {
    if markers.is_empty() && bext.is_none() {
        return Ok(());
    }

    let mut extra: Vec<u8> = Vec::new();

    if !markers.is_empty() {
        // cue chunk
        let mut cue: Vec<u8> = Vec::new();
        cue.extend_from_slice(&(markers.len() as u32).to_le_bytes());
        for (i, m) in markers.iter().enumerate() {
            let id = (i + 1) as u32;
            let off = m.position as u32;
            cue.extend_from_slice(&id.to_le_bytes());
            cue.extend_from_slice(&off.to_le_bytes()); // dwPosition
            cue.extend_from_slice(b"data"); // fccChunk
            cue.extend_from_slice(&0u32.to_le_bytes()); // dwChunkStart
            cue.extend_from_slice(&0u32.to_le_bytes()); // dwBlockStart
            cue.extend_from_slice(&off.to_le_bytes()); // dwSampleOffset
        }
        extra.extend_from_slice(b"cue ");
        extra.extend_from_slice(&(cue.len() as u32).to_le_bytes());
        extra.extend_from_slice(&cue);
        pad_to_even(&mut extra);

        // LIST/adtl with one labl per marker
        let mut adtl: Vec<u8> = Vec::new();
        adtl.extend_from_slice(b"adtl");
        for (i, m) in markers.iter().enumerate() {
            let id = (i + 1) as u32;
            let mut text = m.label.clone().into_bytes();
            text.push(0); // null-terminated
            let labl_size = 4 + text.len();
            adtl.extend_from_slice(b"labl");
            adtl.extend_from_slice(&(labl_size as u32).to_le_bytes());
            adtl.extend_from_slice(&id.to_le_bytes());
            adtl.extend_from_slice(&text);
            if labl_size % 2 == 1 {
                adtl.push(0);
            }
        }
        extra.extend_from_slice(b"LIST");
        extra.extend_from_slice(&(adtl.len() as u32).to_le_bytes());
        extra.extend_from_slice(&adtl);
        pad_to_even(&mut extra);
    }

    if let Some(bext_bytes) = bext {
        extra.extend_from_slice(b"bext");
        extra.extend_from_slice(&(bext_bytes.len() as u32).to_le_bytes());
        extra.extend_from_slice(bext_bytes);
        pad_to_even(&mut extra);
    }

    // Where the top-level size lives depends on the form, so find that out before appending.
    // On an `RF64`/`BW64` file the 32-bit field holds the `0xFFFFFFFF` sentinel and the real
    // size is `ds64`'s `riffSize`; overwriting the sentinel with a 32-bit truncation of the
    // true size (which is what this used to do unconditionally) corrupts the file outright.
    let ds64_riff_size_offset = {
        let mut probe = fs::File::open(&path)?;
        let mut magic = [0u8; 4];
        probe.read_exact(&mut magic)?;
        if super::riff::Form::from_magic_bytes(&magic).is_some_and(|f| f.uses_ds64()) {
            // `ds64` is required to be the first chunk, so its body starts at 20: 12 bytes of
            // `RF64`/size/`WAVE` header, plus its own 8-byte chunk header.
            let mut id = [0u8; 4];
            probe.seek(SeekFrom::Start(12))?;
            probe.read_exact(&mut id)?;
            (&id == b"ds64").then_some(20u64)
        } else {
            None
        }
    };

    let mut file = fs::OpenOptions::new().read(true).write(true).open(&path)?;
    let orig_len = file.seek(SeekFrom::End(0))?;
    file.write_all(&extra)?;
    // Top-level size = total file length - 8 (the magic and the size field itself).
    let new_riff_size = orig_len + extra.len() as u64 - 8;
    match ds64_riff_size_offset {
        Some(at) => {
            file.seek(SeekFrom::Start(at))?;
            file.write_all(&new_riff_size.to_le_bytes())?;
        }
        None => {
            let Ok(size32) = u32::try_from(new_riff_size) else {
                // Silently writing a wrapped size would produce a file that looks fine and
                // decodes to garbage. Nothing reaches here today — every plain-RIFF writer in
                // the app stays under 4GB — but saying so beats a truncating cast.
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "WAV exceeds 4GB but is not RF64; cannot store its size",
                ));
            };
            file.seek(SeekFrom::Start(4))?;
            file.write_all(&size32.to_le_bytes())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_empty_for_nonexistent() {
        let (m, b) = read_markers_and_bext("/nonexistent/path.wav");
        assert!(m.is_empty());
        assert!(b.is_none());
    }

    /// Labels must be found in *any* `LIST` chunk, not just the first. Writers routinely emit
    /// an `INFO` list (artist, software, date) alongside the `adtl` one, and which comes first
    /// is up to them — stopping at the first `LIST` loses every label whenever `INFO` wins,
    /// leaving markers named "Marker 1", "Marker 2" with no sign anything went wrong.
    #[test]
    fn finds_labels_in_an_adtl_list_that_is_not_the_first_list() {
        let dir = std::env::temp_dir().join(format!("tuiwave_bwf_lists_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("two-lists.wav");

        // A minimal WAVE carrying `fmt `/`data`, then an `INFO` list ahead of the `adtl` one.
        let mut body: Vec<u8> = Vec::new();
        body.extend_from_slice(b"WAVE");
        body.extend_from_slice(b"fmt ");
        body.extend_from_slice(&16u32.to_le_bytes());
        body.extend_from_slice(&[0u8; 16]);
        body.extend_from_slice(b"data");
        body.extend_from_slice(&4u32.to_le_bytes());
        body.extend_from_slice(&[0u8; 4]);

        let mut info: Vec<u8> = Vec::new();
        info.extend_from_slice(b"INFO");
        info.extend_from_slice(b"ISFT");
        info.extend_from_slice(&8u32.to_le_bytes());
        info.extend_from_slice(b"Max 9\0\0\0");
        body.extend_from_slice(b"LIST");
        body.extend_from_slice(&(info.len() as u32).to_le_bytes());
        body.extend_from_slice(&info);

        body.extend_from_slice(b"cue ");
        let mut cue: Vec<u8> = Vec::new();
        cue.extend_from_slice(&1u32.to_le_bytes());
        cue.extend_from_slice(&1u32.to_le_bytes()); // id
        cue.extend_from_slice(&0u32.to_le_bytes());
        cue.extend_from_slice(b"data");
        cue.extend_from_slice(&0u32.to_le_bytes());
        cue.extend_from_slice(&0u32.to_le_bytes());
        cue.extend_from_slice(&77u32.to_le_bytes()); // dwSampleOffset
        body.extend_from_slice(&(cue.len() as u32).to_le_bytes());
        body.extend_from_slice(&cue);

        let mut adtl: Vec<u8> = Vec::new();
        adtl.extend_from_slice(b"adtl");
        adtl.extend_from_slice(b"labl");
        adtl.extend_from_slice(&10u32.to_le_bytes());
        adtl.extend_from_slice(&1u32.to_le_bytes()); // matches cue id 1
        adtl.extend_from_slice(b"Verse\0");
        body.extend_from_slice(b"LIST");
        body.extend_from_slice(&(adtl.len() as u32).to_le_bytes());
        body.extend_from_slice(&adtl);

        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        fs::write(&path, &out).unwrap();

        let (markers, _) = read_markers_and_bext(&path);
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0].position, 77);
        assert_eq!(markers[0].label, "Verse", "the label lives in the second LIST");

        fs::remove_dir_all(&dir).ok();
    }
}
