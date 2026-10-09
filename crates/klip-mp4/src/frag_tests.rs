//! Fragmented writer, scanner and finalise.

use std::io::Cursor;

use bytes::Bytes;
use klip_media::{AudioParams, Codec, NANOS_PER_SEC, Packet, TrackInfo, TrackParams};

use crate::moov::ns_to_ts;
use crate::tests::{
    FRAME, audio_info, audio_packets, be32, children, find, video_info, video_packets,
};
use crate::*;

const ORIGIN: i64 = 5 * NANOS_PER_SEC;
/// Audio starts 30 ms after video.
const AUDIO_LAG: i64 = 30_000_000;

fn opus_info() -> TrackInfo {
    let mut head = b"OpusHead".to_vec();
    head.extend_from_slice(&[1, 2]);
    head.extend_from_slice(&312u16.to_le_bytes());
    head.extend_from_slice(&48_000u32.to_le_bytes());
    head.extend_from_slice(&0u16.to_le_bytes());
    head.push(0);
    TrackInfo {
        codec: Codec::Opus,
        codec_data: Bytes::from(head),
        params: TrackParams::Audio(AudioParams {
            sample_rate: 48_000,
            channels: 2,
        }),
        bitrate: None,
    }
}

/// What a recorder does: a fragment per video GOP, audio up to the next keyframe with the
/// newest audio packet held back until its successor is known.
fn write_fragments(
    infos: &[&TrackInfo],
    v: &[Packet],
    audio: &[&[Packet]],
) -> (Vec<u8>, Vec<FragmentInfo>) {
    let tracks: Vec<Mp4Track<'_>> = infos
        .iter()
        .enumerate()
        .map(|(i, info)| {
            let t = Mp4Track::new(info, &[]);
            if i == 1 {
                t.named("Mix")
            } else {
                t.enabled(i < 2)
            }
        })
        .collect();
    let mut w = FragmentedWriter::new(Vec::new(), &tracks, ORIGIN, &Mp4Options::default()).unwrap();
    let keys: Vec<usize> = (0..v.len()).filter(|i| v[*i].keyframe).collect();
    let mut audio_pos = vec![0usize; audio.len()];
    let mut infos_out = Vec::new();
    for (f, &k) in keys.iter().enumerate() {
        let end = keys.get(f + 1).copied().unwrap_or(v.len());
        let next_dts = v.get(end).map(|p| p.dts);
        let limit = next_dts.unwrap_or(i64::MAX);
        let mut parts = vec![FragmentPart {
            track: 0,
            packets: &v[k..end],
            next_dts,
        }];
        for (ai, a) in audio.iter().enumerate() {
            let start = audio_pos[ai];
            let mut stop = start;
            while stop < a.len() && a[stop].dts < limit {
                stop += 1;
            }
            // Hold the newest one back unless this is the last fragment.
            let stop = if next_dts.is_some() && stop == a.len() {
                stop - 1
            } else {
                stop
            };
            parts.push(FragmentPart {
                track: ai + 1,
                packets: &a[start..stop],
                next_dts: a.get(stop).map(|p| p.dts),
            });
            audio_pos[ai] = stop;
        }
        infos_out.push(w.write_fragment(&parts).unwrap());
    }
    assert_eq!(w.fragments(), keys.len() as u64);
    let bytes = w.bytes_written();
    let out = w.into_inner();
    assert_eq!(bytes as usize, out.len());
    (out, infos_out)
}

fn standard_file() -> (Vec<u8>, Vec<Packet>, Vec<Packet>, Vec<FragmentInfo>) {
    let (vi, ai) = (video_info(), audio_info());
    let v = video_packets(180, ORIGIN);
    // 3 s of audio starting 30 ms later.
    let a = audio_packets(140, ORIGIN + AUDIO_LAG);
    let (d, frags) = write_fragments(&[&vi, &ai], &v, &[&a]);
    (d, v, a, frags)
}

fn top(d: &[u8]) -> Vec<String> {
    children(d, 0, d.len())
        .iter()
        .map(|c| String::from_utf8_lossy(&c.0).into_owned())
        .collect()
}

fn scan(d: &[u8]) -> FragmentedScan {
    scan_fragmented(&mut Cursor::new(d)).unwrap()
}

#[test]
fn fragmented_layout_init_and_data_offsets() {
    let (d, v, a, frags) = standard_file();
    assert_eq!(
        top(&d),
        [
            "ftyp", "moov", "moof", "mdat", "moof", "mdat", "moof", "mdat"
        ]
    );
    let (fs, fe) = find(&d, "ftyp");
    assert!(d[fs..fe].windows(4).any(|w| w == b"iso5"));
    // Init: empty sample tables, one trex per track.
    let (ss, _) = find(&d, "moov/trak/mdia/minf/stbl/stsz");
    assert_eq!(be32(&d, ss + 8), 0, "no samples in the init moov");
    let (xs, xe) = find(&d, "moov/mvex");
    assert_eq!(children(&d, xs, xe).len(), 2);
    assert_eq!(frags[0].offset as usize, children(&d, 0, d.len())[2].1 - 8);

    // Every trun's data offset points at the right payload, and tfdt is the rounded
    // absolute time.
    let mut video_seen = 0u32;
    let mut audio_seen = 0u32;
    for f in &frags {
        let moof = f.offset as usize;
        let kids = children(&d, moof + 8, moof + be32(&d, moof) as usize);
        assert_eq!(&kids[0].0, b"mfhd");
        for (kind, s, e) in kids.iter().skip(1) {
            assert_eq!(kind, b"traf");
            let traf = children(&d, *s, *e);
            let track_id = be32(&d, traf[0].1 + 4);
            assert_eq!(
                be32(&d, traf[0].1) & 0xFF_FFFF,
                0x02_0000,
                "default-base-is-moof"
            );
            let tfdt = u64::from_be_bytes(d[traf[1].1 + 4..traf[1].1 + 12].try_into().unwrap());
            let trun = traf[2].1;
            let count = be32(&d, trun + 4);
            let data = moof + be32(&d, trun + 8) as usize;
            let (seen, tag, packets) = if track_id == 1 {
                (&mut video_seen, b'V', &v)
            } else {
                (&mut audio_seen, b'A', &a)
            };
            let first = &packets[*seen as usize];
            let ts = if track_id == 1 { 90_000 } else { 48_000 };
            assert_eq!(tfdt as i64, ns_to_ts(first.dts - ORIGIN, ts));
            assert_eq!(be32(&d, data), *seen, "first sample payload");
            assert_eq!(d[data + 4], tag);
            *seen += count;
        }
    }
    assert_eq!(video_seen, 180);
    assert_eq!(audio_seen, 140, "every audio packet written once");
}

#[test]
fn scan_round_trips_tracks_and_samples() {
    let (d, v, a, frags) = standard_file();
    let s = scan(&d);
    assert_eq!(s.fragments, 3);
    assert_eq!(s.data_end as usize, d.len());
    assert!(s.stopped.is_none() && !s.finalized);
    assert_eq!(s.moofs, frags.iter().map(|f| f.offset).collect::<Vec<_>>());
    assert_eq!(s.tracks.len(), 2);
    assert_eq!(s.tracks[0].info, video_info());
    assert_eq!(s.tracks[1].info, audio_info());
    assert_eq!(s.tracks[1].name.as_deref(), Some("Mix"));
    assert!(s.tracks[0].enabled && s.tracks[1].enabled);
    for (t, pk, ts) in [(&s.tracks[0], &v, 90_000u32), (&s.tracks[1], &a, 48_000)] {
        assert_eq!(t.samples.len(), pk.len());
        for (i, (smp, p)) in t.samples.iter().zip(pk.iter()).enumerate() {
            assert_eq!(
                smp.decode_time as i64,
                ns_to_ts(p.dts - ORIGIN, ts),
                "sample {i}"
            );
            assert_eq!(smp.size as usize, p.data.len());
            assert_eq!(smp.sync, p.keyframe);
        }
        // Durations are exact differences of rounded absolute times (no drift).
        for w in t.samples.windows(2) {
            assert_eq!(
                w[0].decode_time + u64::from(w[0].duration),
                w[1].decode_time
            );
        }
        // Runs cover all samples and point at their payloads.
        let mut idx = 0usize;
        for (off, n) in &t.runs {
            assert_eq!(be32(&d, *off as usize) as usize, idx);
            idx += *n as usize;
        }
        assert_eq!(idx, pk.len());
    }
    // 180 frames at 60 fps = 3 s exactly at 90 kHz.
    let last = s.tracks[0].samples.last().unwrap();
    assert_eq!(last.decode_time + u64::from(last.duration), 270_000);
}

#[test]
fn opus_and_disabled_tracks_round_trip_through_the_init() {
    let (vi, ai, oi) = (video_info(), audio_info(), opus_info());
    let v = video_packets(60, ORIGIN);
    let a = audio_packets(50, ORIGIN);
    let (d, _) = write_fragments(&[&vi, &ai, &oi], &v, &[&a, &a]);
    let s = scan(&d);
    assert_eq!(s.tracks[2].info, oi);
    assert!(!s.tracks[2].enabled);
    // Several audio tracks share alternate group 1 (as in clips).
    let traks: Vec<_> = {
        let (ms, me) = find(&d, "moov");
        children(&d, ms, me)
            .into_iter()
            .filter(|c| &c.0 == b"trak")
            .collect()
    };
    for (i, t) in traks.iter().enumerate() {
        let tkhd = children(&d, t.1, t.2)[0].1;
        let group = u16::from_be_bytes(
            d[tkhd + 4 + 8 + 4 + 4 + 4 + 8 + 2..][..2]
                .try_into()
                .unwrap(),
        );
        assert_eq!(group, if i == 0 { 0 } else { 1 });
    }
}

#[test]
fn truncation_at_every_byte_keeps_the_complete_fragments() {
    let (d, _, _, frags) = standard_file();
    let init_end = frags[0].offset as usize;
    let ends: Vec<usize> = frags
        .iter()
        .map(|f| (f.offset + f.bytes) as usize)
        .collect();
    // Every byte of the last two fragments, and a stride through the rest.
    let from = frags[1].offset as usize - 16;
    let cuts = (init_end..from).step_by(97).chain(from..=d.len());
    for cut in cuts {
        let s = scan(&d[..cut]);
        let complete = ends.iter().filter(|e| **e <= cut).count();
        assert_eq!(s.fragments, complete, "cut at {cut}");
        let expect_end = if complete == 0 {
            init_end
        } else {
            ends[complete - 1]
        };
        assert_eq!(s.data_end as usize, expect_end, "cut at {cut}");
        let samples: usize = s.tracks.iter().map(|t| t.samples.len()).sum();
        let runs: usize = s
            .tracks
            .iter()
            .flat_map(|t| &t.runs)
            .map(|r| r.1 as usize)
            .sum();
        assert_eq!(samples, runs);
    }
    // Cut inside the init: not a recording (yet), but no panic.
    for cut in 0..init_end {
        let _ = scan_fragmented(&mut Cursor::new(&d[..cut]));
    }
}

fn finalize_bytes(d: &[u8], markers: &[Marker]) -> (Vec<u8>, FinalizeSummary) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("rec.mp4");
    std::fs::write(&p, d).unwrap();
    let s = finalize(&p, markers).unwrap();
    (std::fs::read(&p).unwrap(), s)
}

fn markers() -> Vec<Marker> {
    vec![
        Marker {
            at: 1_500_000_000,
            title: "Bookmark 1".into(),
        },
        Marker {
            at: 250_000_000,
            title: "Bookmark 0".into(),
        },
        // Past the end: left out.
        Marker {
            at: 9 * NANOS_PER_SEC,
            title: "late".into(),
        },
    ]
}

#[test]
fn finalize_writes_a_regular_mp4_with_chapters() {
    let (d, _, a, frags) = standard_file();
    let (f, sum) = finalize_bytes(&d, &markers());
    assert!(!sum.already_final);
    assert_eq!(sum.fragments, 3);
    assert_eq!(sum.samples, 320);
    assert_eq!(sum.truncated, 0);
    assert_eq!(sum.bytes as usize, f.len());
    assert_eq!(
        top(&f),
        [
            "ftyp", "free", "free", "mdat", "free", "mdat", "free", "mdat", "moov"
        ]
    );
    // Only box types changed before the new moov: the payload is untouched.
    let mut flipped: Vec<usize> = frags.iter().map(|x| x.offset as usize + 4).collect();
    flipped.push(children(&d, 0, d.len())[1].1 - 4);
    for i in (0..d.len()).filter(|i| f[*i] != d[*i]) {
        assert!(
            flipped.iter().any(|o| (*o..*o + 4).contains(&i)),
            "byte {i} changed"
        );
    }
    let (ms, me) = find(&f, "moov");
    assert!(children(&f, ms, me).iter().all(|c| &c.0 != b"mvex"));

    // Video: 180 samples in 3 chunks (one per fragment), 1500 ticks each.
    let (ss, _) = find(&f, "moov/trak/mdia/minf/stbl/stsz");
    assert_eq!(be32(&f, ss + 8), 180);
    let (cs, _) = find(&f, "moov/trak/mdia/minf/stbl/stco");
    assert_eq!(be32(&f, cs + 4), 3);
    for c in 0..3 {
        let off = be32(&f, cs + 8 + c * 4) as usize;
        assert_eq!(
            be32(&f, off),
            (c * 60) as u32,
            "chunk {c} starts at its first sample"
        );
    }
    let (sc, _) = find(&f, "moov/trak/mdia/minf/stbl/stsc");
    assert_eq!(be32(&f, sc + 4), 1);
    assert_eq!(be32(&f, sc + 12), 60);
    let (ts, _) = find(&f, "moov/trak/mdia/minf/stbl/stts");
    assert_eq!(
        (be32(&f, ts + 4), be32(&f, ts + 8), be32(&f, ts + 12)),
        (1, 180, 1500)
    );

    // Audio: starts 30 ms in, as an empty edit; every sample listed.
    let traks: Vec<_> = children(&f, ms, me)
        .into_iter()
        .filter(|c| &c.0 == b"trak")
        .collect();
    let audio = traks[1];
    let kids = children(&f, audio.1, audio.2);
    let edts = kids
        .iter()
        .find(|k| &k.0 == b"edts")
        .expect("audio edit list");
    let elst = children(&f, edts.1, edts.2)[0].1;
    assert_eq!(be32(&f, elst + 4), 2);
    assert_eq!(be32(&f, elst + 8), 30, "30 ms empty edit");
    assert_eq!(be32(&f, elst + 12) as i32, -1);
    let stbl = {
        let mdia = kids.iter().find(|k| &k.0 == b"mdia").unwrap();
        let minf = children(&f, mdia.1, mdia.2)
            .into_iter()
            .find(|k| &k.0 == b"minf")
            .unwrap();
        children(&f, minf.1, minf.2)
            .into_iter()
            .find(|k| &k.0 == b"stbl")
            .unwrap()
    };
    let stsz = children(&f, stbl.1, stbl.2)
        .into_iter()
        .find(|k| &k.0 == b"stsz")
        .unwrap();
    assert_eq!(be32(&f, stsz.1 + 8) as usize, a.len());

    // Chapters: in time order, relative to the movie start, the late one left out.
    let (cs, ce) = find(&f, "moov/udta/chpl");
    assert_eq!(f[cs], 1, "version 1");
    assert_eq!(f[cs + 8], 2, "two chapters");
    let t0 = u64::from_be_bytes(f[cs + 9..cs + 17].try_into().unwrap());
    assert_eq!(t0, 2_500_000);
    assert_eq!(&f[cs + 18..cs + 28], b"Bookmark 0");
    let t1 = u64::from_be_bytes(f[cs + 28..cs + 36].try_into().unwrap());
    assert_eq!(t1, 15_000_000);
    assert_eq!(ce, cs + 9 + 2 * (8 + 1 + 10));

    // Finalising again changes nothing.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("again.mp4");
    std::fs::write(&p, &f).unwrap();
    let again = finalize(&p, &[]).unwrap();
    assert!(again.already_final);
    assert_eq!(std::fs::read(&p).unwrap(), f);
}

#[test]
fn finalize_after_a_kill_drops_the_partial_fragment() {
    let (d, _, _, frags) = standard_file();
    let last = frags[2];
    // Killed halfway through writing the last mdat.
    let cut = (last.offset + last.bytes / 2) as usize;
    let (f, sum) = finalize_bytes(&d[..cut], &[]);
    assert_eq!(sum.fragments, 2);
    assert_eq!(sum.truncated as usize, cut - last.offset as usize);
    assert_eq!(
        top(&f),
        ["ftyp", "free", "free", "mdat", "free", "mdat", "moov"]
    );
    let (ss, _) = find(&f, "moov/trak/mdia/minf/stbl/stsz");
    assert_eq!(be32(&f, ss + 8), 120);
    // A recording killed before its first fragment: nothing to finalise.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("empty.mp4");
    std::fs::write(&p, &d[..frags[0].offset as usize + 5]).unwrap();
    assert!(matches!(finalize(&p, &[]), Err(Mp4Error::NoFragments)));
}

#[test]
fn an_interrupted_finalize_can_be_repeated() {
    let (d, _, _, frags) = standard_file();
    let m = markers();
    let (clean, _) = finalize_bytes(&d, &m);
    let moov = &clean[d.len()..];
    // State after step 1 (moov appended), after flipping some moofs, after a torn moov.
    let mut states: Vec<Vec<u8>> = Vec::new();
    let mut s1 = d.clone();
    s1.extend_from_slice(moov);
    states.push(s1.clone());
    for n in 1..=frags.len() {
        let mut s = s1.clone();
        for f in &frags[..n] {
            let o = f.offset as usize + 4;
            s[o..o + 4].copy_from_slice(b"free");
        }
        states.push(s);
    }
    let mut torn = d.clone();
    torn.extend_from_slice(&moov[..moov.len() / 2]);
    states.push(torn);
    for (i, s) in states.iter().enumerate() {
        // Players that read fragments still see every fragment until the last step.
        let (out, _) = finalize_bytes(s, &m);
        assert_eq!(out, clean, "state {i}");
    }
}

#[test]
fn writer_rejects_bad_input_without_writing() {
    let vi = video_info();
    let mut w = FragmentedWriter::new(
        Vec::new(),
        &[Mp4Track::new(&vi, &[])],
        ORIGIN,
        &Mp4Options::default(),
    )
    .unwrap();
    let start = w.bytes_written();
    let early = video_packets(2, ORIGIN - FRAME);
    let r = w.write_fragment(&[FragmentPart {
        track: 0,
        packets: &early,
        next_dts: None,
    }]);
    assert!(matches!(r, Err(Mp4Error::InvalidTrack(0, _))));
    let v = video_packets(4, ORIGIN);
    let r = w.write_fragment(&[
        FragmentPart {
            track: 0,
            packets: &v[..2],
            next_dts: None,
        },
        FragmentPart {
            track: 0,
            packets: &v[2..],
            next_dts: None,
        },
    ]);
    assert!(r.is_err(), "same track twice");
    let r = w.write_fragment(&[FragmentPart {
        track: 3,
        packets: &v,
        next_dts: None,
    }]);
    assert!(r.is_err(), "no such track");
    assert_eq!(w.bytes_written(), start);
    w.write_fragment(&[FragmentPart {
        track: 0,
        packets: &v[2..],
        next_dts: None,
    }])
    .unwrap();
    // Going back in time after that: rejected.
    let r = w.write_fragment(&[FragmentPart {
        track: 0,
        packets: &v[..2],
        next_dts: None,
    }]);
    assert!(matches!(r, Err(Mp4Error::NonMonotonic(0))));
    // Empty fragments write nothing.
    let before = w.bytes_written();
    assert_eq!(w.write_fragment(&[]).unwrap().bytes, 0);
    assert_eq!(w.bytes_written(), before);
}

/// xorshift64*: deterministic pseudo-random numbers for mutation tests.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
fn mutated_files_never_panic() {
    let (d, _, _, frags) = standard_file();
    let (fin, _) = finalize_bytes(&d, &markers());
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let dir = tempfile::tempdir().unwrap();
    for round in 0..3000 {
        let src = if round % 2 == 0 { &d } else { &fin };
        let mut m = src.clone();
        // Flip bytes, mostly in the boxes (where sizes and counts live).
        let hot = frags[0].offset as usize + 200;
        for _ in 0..1 + rng.below(4) {
            let at = if rng.below(4) == 0 {
                rng.below(m.len())
            } else {
                rng.below(hot)
            };
            m[at] = rng.next() as u8;
        }
        if rng.below(3) == 0 {
            m.truncate(rng.below(m.len()));
        }
        let _ = scan_fragmented(&mut Cursor::new(&m));
        if round % 25 == 0 {
            let p = dir.path().join(format!("m{round}.mp4"));
            std::fs::write(&p, &m).unwrap();
            let _ = finalize(&p, &[]);
        }
    }
}

#[test]
fn clips_get_markers_in_range_as_chapters() {
    let vi = video_info();
    let v = video_packets(120, 10 * NANOS_PER_SEC);
    let marks = vec![
        Marker {
            at: 11 * NANOS_PER_SEC,
            title: "Bookmark".into(),
        },
        Marker {
            at: 9 * NANOS_PER_SEC,
            title: "before".into(),
        },
    ];
    let mut out = Vec::new();
    write_mp4(
        &mut out,
        &[Mp4Track::new(&vi, &v)],
        &Mp4Options {
            markers: &marks,
            ..Default::default()
        },
    )
    .unwrap();
    let (cs, _) = find(&out, "moov/udta/chpl");
    assert_eq!(out[cs + 8], 1);
    let t = u64::from_be_bytes(out[cs + 9..cs + 17].try_into().unwrap());
    assert_eq!(t, 10_000_000, "1 s after the clip start, in 100 ns units");
    // No markers: no udta.
    let mut plain = Vec::new();
    write_mp4(
        &mut plain,
        &[Mp4Track::new(&vi, &v)],
        &Mp4Options::default(),
    )
    .unwrap();
    assert!(!plain.windows(4).any(|w| w == b"chpl"));
}
