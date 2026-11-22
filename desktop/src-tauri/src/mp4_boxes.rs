//! Does a local MP4 hold any media, judged from its top-level boxes only.
//!
//! Beta 1.1.81 on Windows left a 1.2 KB `<id>.mp4` behind when a start
//! failed offline: the encoder had written `ftyp` + `moov` (fragmented MP4,
//! `empty_moov`) and nothing else. Home then offered it for recovery, and
//! once uploaded the server's ffmpeg failed on it. A file like that holds
//! nothing anyone could want back.
//!
//! The walk reads 8 or 16 header bytes per box and seeks past the payload,
//! so a long recording costs a handful of reads. It is deliberately
//! one-sided: only a clean walk to the end of the file that saw no `moof`
//! and no non-empty `mdat` is [`Mp4Media::HeaderOnly`]. Anything it cannot
//! read with certainty is [`Mp4Media::Unknown`] and callers must treat that
//! as "keep it", so a damaged take that may still hold video is never
//! deleted. The server applies the same rule at `/finalize`.

use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mp4Media {
    /// At least one `moof`, or an `mdat` with payload bytes.
    HasMedia,
    /// A clean walk found only header boxes (`ftyp`, `moov`, `free`, ...).
    HeaderOnly,
    /// Not an MP4, or a box the walk could not follow. Keep the file.
    Unknown,
}

/// More top-level boxes than any real recording reaches before its first
/// fragment; past this the answer is `Unknown`, never `HeaderOnly`.
const MAX_TOP_LEVEL_BOXES: u32 = 4096;

pub fn scan<R: Read + Seek>(reader: &mut R, len: u64) -> io::Result<Mp4Media> {
    let mut offset: u64 = 0;
    for index in 0..MAX_TOP_LEVEL_BOXES {
        if offset == len {
            return Ok(if index == 0 {
                Mp4Media::Unknown
            } else {
                Mp4Media::HeaderOnly
            });
        }
        let remaining = len - offset;
        if remaining < 8 {
            return Ok(Mp4Media::Unknown);
        }
        reader.seek(SeekFrom::Start(offset))?;
        let mut head = [0u8; 16];
        let want = remaining.min(16) as usize;
        reader.read_exact(&mut head[..want])?;
        let size32 = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
        let kind = [head[4], head[5], head[6], head[7]];
        if index == 0 && &kind != b"ftyp" {
            return Ok(Mp4Media::Unknown);
        }
        let (box_size, header_len) = match size32 {
            1 => {
                if want < 16 {
                    return Ok(Mp4Media::Unknown);
                }
                let mut large = [0u8; 8];
                large.copy_from_slice(&head[8..16]);
                (u64::from_be_bytes(large), 16u64)
            }
            0 => (remaining, 8u64),
            n => (u64::from(n), 8u64),
        };
        if box_size < header_len {
            return Ok(Mp4Media::Unknown);
        }
        let available = box_size.min(remaining);
        match &kind {
            b"moof" => return Ok(Mp4Media::HasMedia),
            b"mdat" => {
                if available > header_len {
                    return Ok(Mp4Media::HasMedia);
                }
                if box_size > header_len {
                    // Declared payload, none of it on disk yet.
                    return Ok(Mp4Media::Unknown);
                }
            }
            _ => {}
        }
        if box_size > remaining {
            return Ok(Mp4Media::Unknown);
        }
        offset += box_size;
    }
    Ok(Mp4Media::Unknown)
}

/// [`scan`] on a file. Any I/O error reads as `Unknown` (keep the file).
pub fn scan_file(path: &Path) -> Mp4Media {
    let result = std::fs::File::open(path).and_then(|mut file| {
        let len = file.metadata()?.len();
        scan(&mut file, len)
    });
    match result {
        Ok(media) => media,
        Err(err) => {
            tracing::debug!(path = %path.display(), "mp4 box scan failed: {err}");
            Mp4Media::Unknown
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn boxed(kind: &[u8; 4], payload: usize) -> Vec<u8> {
        let mut out = ((8 + payload) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend(std::iter::repeat_n(1u8, payload));
        out
    }

    fn header() -> Vec<u8> {
        let mut out = boxed(b"ftyp", 20);
        out.extend(boxed(b"moov", 1200));
        out
    }

    fn run(bytes: &[u8]) -> Mp4Media {
        scan(&mut Cursor::new(bytes), bytes.len() as u64).unwrap()
    }

    #[test]
    fn header_only_fragmented_mp4_has_no_media() {
        assert_eq!(run(&header()), Mp4Media::HeaderOnly);
    }

    #[test]
    fn one_fragment_is_media() {
        let mut bytes = header();
        bytes.extend(boxed(b"moof", 40));
        bytes.extend(boxed(b"mdat", 500));
        assert_eq!(run(&bytes), Mp4Media::HasMedia);
    }

    #[test]
    fn progressive_mdat_is_media() {
        let mut bytes = boxed(b"ftyp", 20);
        bytes.extend(boxed(b"mdat", 900));
        bytes.extend(boxed(b"moov", 100));
        assert_eq!(run(&bytes), Mp4Media::HasMedia);
    }

    #[test]
    fn empty_mdat_is_not_media() {
        let mut bytes = header();
        bytes.extend(boxed(b"mdat", 0));
        assert_eq!(run(&bytes), Mp4Media::HeaderOnly);
    }

    #[test]
    fn truncated_header_box_is_unknown_not_header_only() {
        let bytes = header();
        assert_eq!(run(&bytes[..600]), Mp4Media::Unknown);
    }

    #[test]
    fn truncated_fragment_is_media() {
        let mut bytes = header();
        bytes.extend(boxed(b"moof", 40));
        bytes.extend(boxed(b"mdat", 500));
        let cut = bytes.len() - 100;
        assert_eq!(run(&bytes[..cut]), Mp4Media::HasMedia);
        // Cut inside the moof header itself: still a fragment was started.
        let moof_at = header().len();
        assert_eq!(run(&bytes[..moof_at + 8]), Mp4Media::HasMedia);
    }

    #[test]
    fn a_few_stray_bytes_after_the_header_are_unknown() {
        let mut bytes = header();
        bytes.extend_from_slice(&[0, 0, 0]);
        assert_eq!(run(&bytes), Mp4Media::Unknown);
    }

    #[test]
    fn mdat_to_end_of_file_and_64_bit_sizes() {
        let mut to_eof = header();
        let mut mdat = boxed(b"mdat", 300);
        mdat[..4].copy_from_slice(&0u32.to_be_bytes());
        to_eof.extend(mdat);
        assert_eq!(run(&to_eof), Mp4Media::HasMedia);

        let mut large = header();
        large.extend_from_slice(&1u32.to_be_bytes());
        large.extend_from_slice(b"mdat");
        large.extend_from_slice(&(16u64 + 200).to_be_bytes());
        large.extend(std::iter::repeat_n(1u8, 200));
        assert_eq!(run(&large), Mp4Media::HasMedia);
    }

    #[test]
    fn not_an_mp4_is_unknown() {
        assert_eq!(run(&[]), Mp4Media::Unknown);
        assert_eq!(
            run(&[0x1a, 0x45, 0xdf, 0xa3, 0, 0, 0, 0, 0]),
            Mp4Media::Unknown
        );
        assert_eq!(run(&boxed(b"moov", 50)), Mp4Media::Unknown);
    }

    #[test]
    fn a_box_smaller_than_its_header_is_unknown() {
        let mut bytes = boxed(b"ftyp", 20);
        bytes.extend_from_slice(&4u32.to_be_bytes());
        bytes.extend_from_slice(b"free");
        assert_eq!(run(&bytes), Mp4Media::Unknown);
    }

    #[test]
    fn scan_file_reads_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("header.mp4");
        std::fs::write(&path, header()).unwrap();
        assert_eq!(scan_file(&path), Mp4Media::HeaderOnly);
        assert_eq!(
            scan_file(&dir.path().join("missing.mp4")),
            Mp4Media::Unknown
        );
    }
}
