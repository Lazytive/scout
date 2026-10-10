use std::io::{Cursor, Read, Seek, SeekFrom};

use klip_media::NANOS_PER_SEC;
use proptest::prelude::*;

use super::*;
use crate::frag_tests::{Rng, finalize_bytes, markers, standard_file};
use crate::tests::{audio_info, audio_packets, find, video_info, video_packets};
use crate::{Marker, Mp4Options, Mp4Track, write_mp4};

fn clip(markers: &[Marker]) -> Vec<u8> {
    let vi = video_info();
    let ai = audio_info();
    // 2 s of 60 fps video and a bit less AAC (the movie lasts as long as its longest track).
    let v = video_packets(120, 10 * NANOS_PER_SEC);
    let a = audio_packets(93, 10 * NANOS_PER_SEC);
    let mut out = Vec::new();
    write_mp4(
        &mut out,
        &[Mp4Track::new(&vi, &v), Mp4Track::new(&ai, &a)],
        &Mp4Options {
            creation_time_unix: 1_760_000_000,
            markers,
            ..Default::default()
        },
    )
    .unwrap();
    out
}

fn probe_bytes(d: &[u8]) -> Result<Mp4Info, Mp4Error> {
    probe(&mut Cursor::new(d))
}

#[test]
fn faststart_clip() {
    let marks = vec![
        Marker {
            at: 11 * NANOS_PER_SEC,
            title: "Bookmark".into(),
        },
        Marker {
            at: 10 * NANOS_PER_SEC + NANOS_PER_SEC / 2,
            title: "Ace\u{7}!".into(),
        },
    ];
    let info = probe_bytes(&clip(&marks)).unwrap();
    assert_eq!(info.duration_ms, Some(2000));
    assert_eq!(info.creation_time_unix, 1_760_000_000);
    let v = info.video.clone().unwrap();
    assert_eq!(v.codec, "H.264");
    assert_eq!((v.width, v.height), (1280, 720));
    assert!((v.fps.unwrap() - 60.0).abs() < 0.01, "{:?}", v.fps);
    assert_eq!(info.audio_tracks, 1);
    // Sorted by time, with their titles (control characters dropped).
    assert_eq!(info.chapters_ms, vec![500, 1000]);
    assert_eq!(info.chapter_titles, vec!["Ace!", "Bookmark"]);
    assert!(!info.fragmented);
}

#[test]
fn moov_at_the_end() {
    // ftyp, mdat, moov: the layout of most recorders that don't write faststart.
    let d = clip(&[]);
    let kids = crate::tests::children(&d, 0, d.len());
    let span = |k: &[u8; 4]| {
        let (_, s, e) = kids.iter().find(|(kind, _, _)| kind == k).unwrap();
        let hdr = if crate::tests::be32(&d, s - 8) == 1 {
            16
        } else {
            8
        };
        d[s - hdr..*e].to_vec()
    };
    let mut moved = span(b"ftyp");
    moved.extend(span(b"mdat"));
    moved.extend(span(b"moov"));
    assert_eq!(moved.len(), d.len());
    let info = probe_bytes(&moved).unwrap();
    assert_eq!(info.duration_ms, Some(2000));
    assert_eq!(info.video.unwrap().width, 1280);
}

#[test]
fn recordings_before_and_after_finalising() {
    let (d, _, _, _) = standard_file();
    let live = probe_bytes(&d).unwrap();
    assert!(live.fragmented);
    assert_eq!(live.duration_ms, None);
    assert!(live.video.as_ref().unwrap().fps.is_none());
    let (fin, summary) = finalize_bytes(&d, &markers());
    let done = probe_bytes(&fin).unwrap();
    assert!(!done.fragmented);
    let want = u64::try_from(summary.duration / 1_000_000).unwrap();
    let got = done.duration_ms.unwrap();
    assert!(got.abs_diff(want) <= 1, "{got} vs {want}");
    assert_eq!(done.chapters_ms, vec![250, 1500]);
    assert!(done.video.unwrap().fps.is_some());
}

#[test]
fn no_moov_is_an_error() {
    assert!(probe_bytes(b"").is_err());
    assert!(probe_bytes(b"\0\0\0\x08free").is_err());
    let d = clip(&[]);
    let (s, _) = find(&d, "moov");
    // Cut inside the moov: the box no longer fits the file.
    assert!(probe_bytes(&d[..s + 20]).is_err());
}

/// A file that claims to be huge: only the first bytes are real, the rest reads as zeros.
struct Sparse {
    head: Vec<u8>,
    len: u64,
    pos: u64,
}

impl Read for Sparse {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.len.saturating_sub(self.pos);
        let n = (buf.len() as u64).min(left) as usize;
        for (i, b) in buf[..n].iter_mut().enumerate() {
            *b = self
                .head
                .get((self.pos + i as u64) as usize)
                .copied()
                .unwrap_or(0);
        }
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Sparse {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.pos = match to {
            SeekFrom::Start(p) => p,
            SeekFrom::End(d) => (self.len as i64 + d) as u64,
            SeekFrom::Current(d) => (self.pos as i64 + d) as u64,
        };
        Ok(self.pos)
    }
}

#[test]
fn a_huge_moov_is_refused_without_reading_it() {
    let size: u64 = MAX_INFO_MOOV_BYTES + 1024;
    let mut head = Vec::new();
    head.extend_from_slice(&1u32.to_be_bytes());
    head.extend_from_slice(b"moov");
    head.extend_from_slice(&size.to_be_bytes());
    let mut f = Sparse {
        head,
        len: size,
        pos: 0,
    };
    assert!(matches!(probe(&mut f), Err(Mp4Error::TooLarge(_))));
}

#[test]
fn truncation_at_every_byte_never_panics() {
    let d = clip(&[Marker {
        at: 11 * NANOS_PER_SEC,
        title: "x".into(),
    }]);
    for cut in 0..d.len() {
        let _ = probe_bytes(&d[..cut]);
    }
}

#[test]
fn mutated_files_never_panic() {
    let marks = markers();
    let a = clip(&marks);
    let (frag, _, _, _) = standard_file();
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    for round in 0..4000 {
        let mut m = if round % 2 == 0 {
            a.clone()
        } else {
            frag.clone()
        };
        // Mostly in the first few KB, where the boxes are.
        let hot = m.len().min(4096);
        for _ in 0..1 + rng.below(6) {
            let at = rng.below(hot);
            m[at] = rng.next() as u8;
        }
        let _ = probe_bytes(&m);
    }
}

proptest! {
    #[test]
    fn random_bytes_never_panic(d in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let _ = probe_bytes(&d);
    }

    #[test]
    fn random_boxes_never_panic(
        boxes in proptest::collection::vec(
            (any::<u32>(), prop::sample::select(vec![*b"moov", *b"trak", *b"mdia", *b"mvhd", *b"udta", *b"chpl", *b"free"]),
             proptest::collection::vec(any::<u8>(), 0..64)),
            0..12,
        )
    ) {
        let mut d = Vec::new();
        for (size, kind, body) in boxes {
            d.extend_from_slice(&size.to_be_bytes());
            d.extend_from_slice(&kind);
            d.extend_from_slice(&body);
        }
        let _ = probe_bytes(&d);
    }
}
