//! Broadcast-WAV extras that `hound` doesn't handle: `cue `/`adtl` timeline markers and
//! the `bext` broadcast-metadata chunk.
//!
//! Reading walks the RIFF chunk list directly. Writing keeps `hound` responsible for the
//! `fmt `/`data` chunks (so float/int encoding stays battle-tested) and *appends* the extra
//! chunks afterward, patching the top-level RIFF size — readers that don't understand these
//! chunks simply skip them, so sample data still round-trips everywhere.

use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use super::document::Marker;

fn read_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Reads timeline markers (`cue ` points joined with `adtl`/`labl` labels) and the raw
/// `bext` chunk bytes (header excluded) from a WAV file. Returns empties on any malformed or
/// missing chunk rather than erroring — markers are optional metadata.
pub fn read_markers_and_bext(path: impl AsRef<Path>) -> (Vec<Marker>, Option<Vec<u8>>) {
    let Ok(bytes) = fs::read(path) else {
        return (Vec::new(), None);
    };
    // Header: 'RIFF' <size:4> 'WAVE' then chunks.
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return (Vec::new(), None);
    }

    let mut cue_positions: Vec<(u32, u32)> = Vec::new(); // (id, sample offset)
    let mut labels: Vec<(u32, String)> = Vec::new(); // (id, text)
    let mut bext: Option<Vec<u8>> = None;

    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = read_u32(&bytes, pos + 4) as usize;
        let body = pos + 8;
        if body + size > bytes.len() {
            break;
        }
        let chunk = &bytes[body..body + size];
        match id {
            b"cue " => {
                if chunk.len() >= 4 {
                    let n = read_u32(chunk, 0) as usize;
                    for i in 0..n {
                        let base = 4 + i * 24;
                        if base + 24 <= chunk.len() {
                            let cue_id = read_u32(chunk, base);
                            // dwSampleOffset is the last u32 of the 24-byte record.
                            let sample_offset = read_u32(chunk, base + 20);
                            cue_positions.push((cue_id, sample_offset));
                        }
                    }
                }
            }
            b"LIST" if chunk.len() >= 4 && &chunk[0..4] == b"adtl" => {
                let mut p = 4;
                while p + 8 <= chunk.len() {
                    let sub_id = &chunk[p..p + 4];
                    let sub_size = read_u32(chunk, p + 4) as usize;
                    let sub_body = p + 8;
                    if sub_body + sub_size > chunk.len() {
                        break;
                    }
                    if sub_id == b"labl" && sub_size >= 4 {
                        let label_id = read_u32(chunk, sub_body);
                        let text_bytes = &chunk[sub_body + 4..sub_body + sub_size];
                        let end = text_bytes.iter().position(|&c| c == 0).unwrap_or(text_bytes.len());
                        let text = String::from_utf8_lossy(&text_bytes[..end]).into_owned();
                        labels.push((label_id, text));
                    }
                    p = sub_body + sub_size + (sub_size & 1); // word-align
                }
            }
            b"bext" => {
                bext = Some(chunk.to_vec());
            }
            _ => {}
        }
        pos = body + size + (size & 1); // chunks are word-aligned
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

    let mut file = fs::OpenOptions::new().read(true).write(true).open(&path)?;
    let orig_len = file.seek(SeekFrom::End(0))?;
    file.write_all(&extra)?;
    // Patch RIFF chunk size = total file length - 8.
    let new_riff_size = (orig_len + extra.len() as u64 - 8) as u32;
    file.seek(SeekFrom::Start(4))?;
    file.write_all(&new_riff_size.to_le_bytes())?;
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
}
