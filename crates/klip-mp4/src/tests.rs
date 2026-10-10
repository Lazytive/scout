use bytes::Bytes;
use klip_media::{AudioParams, Codec, Colorimetry, NANOS_PER_SEC, TrackParams, VideoParams};

use super::*;
use crate::moov::{ns_to_ts, ts_to_ns};

/// Tiny box parser for tests: returns (type, payload range) children of a byte range.
pub(crate) fn children(data: &[u8], start: usize, end: usize) -> Vec<([u8; 4], usize, usize)> {
    let mut out = Vec::new();
    let mut pos = start;
    while pos + 8 <= end {
        let size = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        let kind: [u8; 4] = data[pos + 4..pos + 8].try_into().unwrap();
        let (hdr, size) = if size == 1 {
            let large = u64::from_be_bytes(data[pos + 8..pos + 16].try_into().unwrap()) as usize;
            (16, large)
        } else {
            (8, size)
        };
        assert!(
            size >= hdr && pos + size <= end,
            "bad box {:?}",
            std::str::from_utf8(&kind)
        );
        out.push((kind, pos + hdr, pos + size));
        pos += size;
    }
    assert_eq!(pos, end, "trailing bytes");
    out
}

pub(crate) fn find(data: &[u8], path: &str) -> (usize, usize) {
    let mut range = (0, data.len());
    for (depth, name) in path.split('/').enumerate() {
        let kids = children(data, range.0, range.1);
        let (_, s, e) = kids
            .into_iter()
            .find(|(k, _, _)| k == name.as_bytes())
            .unwrap_or_else(|| panic!("no {name} at depth {depth} in {path}"));
        // Skip into containers that have a fixed header before their children.
        range = match name {
            "stsd" => (s + 8, e),
            "dref" => (s + 8, e),
            _ => (s, e),
        };
    }
    range
}

pub(crate) fn be32(d: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(d[at..at + 4].try_into().unwrap())
}

pub(crate) fn video_info() -> TrackInfo {
    TrackInfo {
        codec: Codec::H264,
        codec_data: Bytes::from_static(&[1, 0x64, 0, 0x28, 0xFF, 0xE1, 0, 0]),
        params: TrackParams::Video(VideoParams {
            width: 1280,
            height: 720,
            colorimetry: Some(Colorimetry::BT709),
        }),
        bitrate: None,
    }
}

pub(crate) fn audio_info() -> TrackInfo {
    TrackInfo {
        codec: Codec::Aac,
        codec_data: Bytes::from_static(&[0x11, 0x90]),
        params: TrackParams::Audio(AudioParams {
            sample_rate: 48_000,
            channels: 2,
        }),
        bitrate: Some(160_000),
    }
}

pub(crate) const FRAME: i64 = NANOS_PER_SEC / 60;

/// Video packets with unique payloads: 4-byte big-endian sample index + track tag.
pub(crate) fn video_packets(n: usize, start: i64) -> Vec<Packet> {
    (0..n)
        .map(|i| {
            let mut data = (i as u32).to_be_bytes().to_vec();
            data.push(b'V');
            data.extend(std::iter::repeat_n(0xAB, i % 7));
            Packet {
                pts: start + i as i64 * FRAME,
                dts: start + i as i64 * FRAME,
                duration: FRAME,
                keyframe: i % 60 == 0,
                data: Bytes::from(data),
            }
        })
        .collect()
}

pub(crate) fn audio_packets(n: usize, start: i64) -> Vec<Packet> {
    let step = NANOS_PER_SEC * 1024 / 48_000;
    (0..n)
        .map(|i| {
            let mut data = (i as u32).to_be_bytes().to_vec();
            data.push(b'A');
            Packet {
                pts: start + i as i64 * step,
                dts: start + i as i64 * step,
                duration: step,
                keyframe: true,
                data: Bytes::from(data),
            }
        })
        .collect()
}

fn write(tracks: &[Mp4Track<'_>], opts: Mp4Options) -> (Vec<u8>, Mp4Summary) {
    let mut out = Vec::new();
    let s = write_mp4(&mut out, tracks, &opts).unwrap();
    assert_eq!(s.bytes as usize, out.len());
    (out, s)
}

#[test]
fn faststart_layout_and_sample_tables() {
    let vi = video_info();
    let ai = audio_info();
    let v = video_packets(150, 1_000_000_000);
    // Audio starts 100 ms after video.
    let a = audio_packets(100, 1_100_000_000);
    let tracks = [Mp4Track::new(&vi, &v), Mp4Track::new(&ai, &a)];
    let (d, summary) = write(&tracks, Mp4Options::default());
    assert_eq!(summary.samples, 250);

    let top: Vec<_> = children(&d, 0, d.len()).iter().map(|c| c.0).collect();
    assert_eq!(
        top,
        [*b"ftyp", *b"moov", *b"mdat"],
        "moov must come before mdat"
    );

    // Video: 150 samples, keyframes at 1, 61, 121.
    let (s, _) = find(&d, "moov/trak/mdia/minf/stbl/stsz");
    assert_eq!(be32(&d, s + 8), 150);
    let (s, _) = find(&d, "moov/trak/mdia/minf/stbl/stss");
    assert_eq!(be32(&d, s + 4), 3);
    assert_eq!(
        [be32(&d, s + 8), be32(&d, s + 12), be32(&d, s + 16)],
        [1, 61, 121]
    );
    // stts: one run of 150 x 1500 ticks (90 kHz / 60).
    let (s, _) = find(&d, "moov/trak/mdia/minf/stbl/stts");
    assert_eq!(be32(&d, s + 4), 1);
    assert_eq!((be32(&d, s + 8), be32(&d, s + 12)), (150, 1500));
    // No ctts when PTS == DTS.
    let stbl = find(&d, "moov/trak/mdia/minf/stbl");
    assert!(!children(&d, stbl.0, stbl.1).iter().any(|c| &c.0 == b"ctts"));
    // avcC carries the codec data verbatim; colr is nclx BT.709.
    let (s, e) = find(&d, "moov/trak/mdia/minf/stbl/stsd/avc1");
    let kids = children(&d, s + 78, e);
    let avcc = kids.iter().find(|k| &k.0 == b"avcC").unwrap();
    assert_eq!(&d[avcc.1..avcc.2], &vi.codec_data[..]);
    let colr = kids.iter().find(|k| &k.0 == b"colr").unwrap();
    assert_eq!(
        &d[colr.1..colr.2],
        &[b'n', b'c', b'l', b'x', 0, 1, 0, 1, 0, 1, 0]
    );

    // Every chunk offset points at the right payload.
    let traks: Vec<_> = children(&d, find(&d, "moov").0, find(&d, "moov").1)
        .into_iter()
        .filter(|c| &c.0 == b"trak")
        .collect();
    assert_eq!(traks.len(), 2);
    for (ti, (tag, packets)) in [(b'V', &v), (b'A', &a)].into_iter().enumerate() {
        let trak = traks[ti];
        let mdia = children(&d, trak.1, trak.2)
            .into_iter()
            .find(|c| &c.0 == b"mdia")
            .unwrap();
        let minf = children(&d, mdia.1, mdia.2)
            .into_iter()
            .find(|c| &c.0 == b"minf")
            .unwrap();
        let stbl = children(&d, minf.1, minf.2)
            .into_iter()
            .find(|c| &c.0 == b"stbl")
            .unwrap();
        let stco = children(&d, stbl.1, stbl.2)
            .into_iter()
            .find(|c| &c.0 == b"stco")
            .unwrap();
        assert_eq!(be32(&d, stco.1 + 4) as usize, packets.len());
        for (i, p) in packets.iter().enumerate() {
            let off = be32(&d, stco.1 + 8 + 4 * i) as usize;
            assert_eq!(
                &d[off..off + p.data.len()],
                &p.data[..],
                "track {ti} sample {i}"
            );
            assert_eq!(d[off + 4], tag);
        }
    }
    // Audio got an empty edit of 100 ms in movie timescale.
    let trak = traks[1];
    let edts = children(&d, trak.1, trak.2)
        .into_iter()
        .find(|c| &c.0 == b"edts")
        .unwrap();
    let elst = children(&d, edts.1, edts.2)[0];
    assert_eq!(be32(&d, elst.1 + 4), 2);
    assert_eq!(be32(&d, elst.1 + 8), 100);
    assert_eq!(be32(&d, elst.1 + 12) as i32, -1);
    // Video needs no edit list.
    assert!(
        !children(&d, traks[0].1, traks[0].2)
            .iter()
            .any(|c| &c.0 == b"edts")
    );
    // 150 frames at 60 fps = 2.5 s.
    assert_eq!(summary.duration, 2_500_000_000);
}

#[test]
fn samples_are_interleaved_by_decode_time() {
    let vi = video_info();
    let ai = audio_info();
    let v = video_packets(30, 0);
    let a = audio_packets(24, 0);
    let (d, _) = write(
        &[Mp4Track::new(&vi, &v), Mp4Track::new(&ai, &a)],
        Mp4Options::default(),
    );
    let mdat = children(&d, 0, d.len())[2];
    let mut tags = Vec::new();
    let mut pos = mdat.1;
    // Walk payloads using the known layout: video payloads are 5 + i%7 bytes, audio 5.
    let (mut vi_n, mut ai_n) = (0usize, 0usize);
    while pos < mdat.2 {
        let tag = d[pos + 4];
        tags.push(tag);
        if tag == b'V' {
            pos += 5 + vi_n % 7;
            vi_n += 1;
        } else {
            pos += 5;
            ai_n += 1;
        }
    }
    assert_eq!((vi_n, ai_n), (30, 24));
    // Both tracks appear before either finishes.
    let first_a = tags.iter().position(|t| *t == b'A').unwrap();
    let last_v = tags.iter().rposition(|t| *t == b'V').unwrap();
    assert!(first_a < last_v);
}

#[test]
fn composition_offsets_and_edit_list() {
    let vi = video_info();
    // I P B order: pts = dts + 2 frames for the first, typical B-frame shape.
    let mut v = video_packets(6, 0);
    let pts = [2, 4, 3, 6, 5, 7];
    for (p, k) in v.iter_mut().zip(pts) {
        p.pts = k * FRAME;
    }
    let (d, _) = write(&[Mp4Track::new(&vi, &v)], Mp4Options::default());
    let (s, _) = find(&d, "moov/trak/mdia/minf/stbl/ctts");
    assert_eq!(d[s], 1, "ctts version 1");
    let (s, _) = find(&d, "moov/trak/edts/elst");
    assert_eq!(be32(&d, s + 4), 1);
    assert_eq!(
        be32(&d, s + 12) as i32,
        3000,
        "media_time = first composition offset"
    );
}

#[test]
fn forced_co64_uses_64_bit_offsets() {
    let vi = video_info();
    let v = video_packets(10, 0);
    let (d, _) = write(
        &[Mp4Track::new(&vi, &v)],
        Mp4Options {
            force_co64: true,
            ..Default::default()
        },
    );
    let (s, _) = find(&d, "moov/trak/mdia/minf/stbl/co64");
    let first = u64::from_be_bytes(d[s + 8..s + 16].try_into().unwrap()) as usize;
    assert_eq!(&d[first..first + 5], &v[0].data[..]);
}

#[test]
fn rejects_bad_input_without_panicking() {
    let vi = video_info();
    let mut out = Vec::new();
    assert!(matches!(
        write_mp4(&mut out, &[], &Mp4Options::default()),
        Err(Mp4Error::NoTracks)
    ));
    assert!(matches!(
        write_mp4(&mut out, &[Mp4Track::new(&vi, &[])], &Mp4Options::default()),
        Err(Mp4Error::EmptyTrack(0))
    ));
    let mut v = video_packets(3, 0);
    v[2].dts = -5;
    assert!(matches!(
        write_mp4(&mut out, &[Mp4Track::new(&vi, &v)], &Mp4Options::default()),
        Err(Mp4Error::NonMonotonic(0))
    ));
    let mut huge = video_info();
    huge.params = TrackParams::Video(VideoParams {
        width: 70_000,
        height: 10,
        colorimetry: None,
    });
    let v = video_packets(3, 0);
    assert!(matches!(
        write_mp4(
            &mut out,
            &[Mp4Track::new(&huge, &v)],
            &Mp4Options::default()
        ),
        Err(Mp4Error::TooLarge(_))
    ));
    // Extreme timestamps: error, not overflow.
    let mut v = video_packets(2, 0);
    v[0].dts = i64::MIN;
    v[0].pts = i64::MIN;
    v[1].dts = i64::MAX;
    v[1].pts = i64::MAX;
    assert!(write_mp4(&mut out, &[Mp4Track::new(&vi, &v)], &Mp4Options::default()).is_err());
    // Opus without an OpusHead.
    let opus = TrackInfo {
        codec: Codec::Opus,
        codec_data: Bytes::from_static(b"nope"),
        params: TrackParams::Audio(AudioParams {
            sample_rate: 48_000,
            channels: 2,
        }),
        bitrate: None,
    };
    let a = audio_packets(2, 0);
    assert!(matches!(
        write_mp4(
            &mut out,
            &[Mp4Track::new(&opus, &a)],
            &Mp4Options::default()
        ),
        Err(Mp4Error::Unsupported(_))
    ));
}

#[test]
fn opus_head_becomes_dops() {
    let mut head = b"OpusHead".to_vec();
    head.extend_from_slice(&[1, 2]);
    head.extend_from_slice(&312u16.to_le_bytes());
    head.extend_from_slice(&48_000u32.to_le_bytes());
    head.extend_from_slice(&0u16.to_le_bytes());
    head.push(0);
    let opus = TrackInfo {
        codec: Codec::Opus,
        codec_data: Bytes::from(head),
        params: TrackParams::Audio(AudioParams {
            sample_rate: 48_000,
            channels: 2,
        }),
        bitrate: None,
    };
    let vi = video_info();
    let v = video_packets(5, 0);
    let a = audio_packets(5, 0);
    let (d, _) = write(
        &[Mp4Track::new(&vi, &v), Mp4Track::new(&opus, &a)],
        Mp4Options::default(),
    );
    let pos = d.windows(4).position(|w| w == b"dOps").unwrap();
    let body = &d[pos + 4..pos + 4 + 11];
    assert_eq!(body, &[0, 2, 0x01, 0x38, 0, 0, 0xBB, 0x80, 0, 0, 0]);
}

#[test]
fn atomic_save_refuses_overwrite_and_leaves_no_temp() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clip.mp4");
    let vi = video_info();
    let v = video_packets(10, 0);
    let tracks = [Mp4Track::new(&vi, &v)];
    let s = save_mp4_atomic(&path, &tracks, &Mp4Options::default()).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), s.bytes);
    assert!(matches!(
        save_mp4_atomic(&path, &tracks, &Mp4Options::default()),
        Err(Mp4Error::AlreadyExists(_))
    ));
    // A failing write removes its temp file.
    let bad = dir.path().join("bad.mp4");
    assert!(save_mp4_atomic(&bad, &[Mp4Track::new(&vi, &[])], &Mp4Options::default()).is_err());
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, ["clip.mp4"]);
}

#[test]
fn rounding_helpers() {
    assert_eq!(ns_to_ts(FRAME, 90_000), 1500);
    assert_eq!(ns_to_ts(-FRAME, 90_000), -1500);
    assert_eq!(ns_to_ts(i64::MAX, u32::MAX), i64::MAX);
    assert_eq!(ts_to_ns(1500, 90_000), 16_666_666);
    assert_eq!(ts_to_ns(5, 0), 0);
}

#[test]
fn audio_tracks_are_named_grouped_and_only_the_mix_enabled() {
    let vi = video_info();
    let ai = audio_info();
    let v = video_packets(30, 0);
    let a = audio_packets(24, 0);
    let long = "é".repeat(200);
    let (d, _) = write(
        &[
            Mp4Track::new(&vi, &v),
            Mp4Track::new(&ai, &a).named("Mix"),
            Mp4Track::new(&ai, &a).named("Game\0").enabled(false),
            Mp4Track::new(&ai, &a).named(&long).enabled(false),
        ],
        Mp4Options::default(),
    );
    let moov = children(&d, 0, d.len())[1];
    let traks: Vec<_> = children(&d, moov.1, moov.2)
        .into_iter()
        .filter(|c| &c.0 == b"trak")
        .collect();
    assert_eq!(traks.len(), 4);
    let mut seen = Vec::new();
    for t in &traks {
        let kids = children(&d, t.1, t.2);
        let tkhd = kids.iter().find(|c| &c.0 == b"tkhd").unwrap();
        let flags = be32(&d, tkhd.1) & 0x00FF_FFFF;
        // v0 tkhd: version/flags, ctime, mtime, id, reserved, duration, 8 reserved, layer.
        let group = i16::from_be_bytes(d[tkhd.1 + 34..tkhd.1 + 36].try_into().unwrap());
        let mdia = kids.iter().find(|c| &c.0 == b"mdia").unwrap();
        let hdlr = children(&d, mdia.1, mdia.2)
            .into_iter()
            .find(|c| &c.0 == b"hdlr")
            .unwrap();
        // version/flags, pre_defined, handler, 3 reserved words, then the name.
        let name = &d[hdlr.1 + 24..hdlr.2];
        assert_eq!(name.last(), Some(&0), "NUL-terminated");
        let name = std::str::from_utf8(&name[..name.len() - 1])
            .unwrap()
            .to_string();
        seen.push((flags, group, name));
    }
    assert_eq!(seen[0], (3, 0, "VideoHandler".to_string()));
    assert_eq!(seen[1], (3, 1, "Mix".to_string()));
    assert_eq!(seen[2], (2, 1, "Game".to_string()));
    assert_eq!(seen[3].0, 2);
    assert_eq!(
        seen[3].2.len(),
        254,
        "cut at a char boundary under 255 bytes"
    );

    // A single audio track is not in an alternate group.
    let (d, _) = write(
        &[Mp4Track::new(&vi, &v), Mp4Track::new(&ai, &a)],
        Mp4Options::default(),
    );
    let moov = children(&d, 0, d.len())[1];
    let trak = children(&d, moov.1, moov.2)
        .into_iter()
        .filter(|c| &c.0 == b"trak")
        .nth(1)
        .unwrap();
    let tkhd = children(&d, trak.1, trak.2)[0];
    assert_eq!(&tkhd.0, b"tkhd");
    assert_eq!(
        i16::from_be_bytes(d[tkhd.1 + 34..tkhd.1 + 36].try_into().unwrap()),
        0
    );
}

/// Packets spooled to a file in arrival order (audio and video interleaved as they
/// came) write the same bytes as the in-memory writer.
#[test]
fn spooled_writer_matches_the_in_memory_one() {
    let vi = video_info();
    let ai = audio_info();
    let v = video_packets(90, 2_000_000_000);
    let a = audio_packets(60, 2_000_000_000);
    let marks = [Marker {
        at: 2_500_000_000,
        title: "half".into(),
    }];
    let opts = Mp4Options {
        creation_time_unix: 1_700_000_000,
        force_co64: false,
        markers: &marks,
    };
    let (want, _) = write(&[Mp4Track::new(&vi, &v), Mp4Track::new(&ai, &a)], opts);
    // The spool: whatever order the encoders produced, offsets recorded.
    let mut spool: Vec<u8> = Vec::new();
    let mut vs = Vec::new();
    let mut r#as = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < v.len() || j < a.len() {
        let take_video = j >= a.len() || (i < v.len() && i % 3 != 2);
        let (p, list) = if take_video {
            i += 1;
            (&v[i - 1], &mut vs)
        } else {
            j += 1;
            (&a[j - 1], &mut r#as)
        };
        let mut s = Sample::of(p).unwrap();
        s.offset = spool.len() as u64;
        spool.extend_from_slice(&p.data);
        list.push(s);
    }
    let tracks = [
        SpooledTrack {
            info: &vi,
            samples: &vs,
            name: None,
            enabled: true,
        },
        SpooledTrack {
            info: &ai,
            samples: &r#as,
            name: None,
            enabled: true,
        },
    ];
    let mut got = Vec::new();
    let s = write_mp4_spooled(&mut got, &tracks, &opts, &mut std::io::Cursor::new(&spool)).unwrap();
    assert_eq!(got, want);
    assert_eq!(s.bytes as usize, got.len());
    // A short spool is an error, not a truncated file.
    let short = &spool[..spool.len() - 1];
    let mut out = Vec::new();
    assert!(write_mp4_spooled(&mut out, &tracks, &opts, &mut std::io::Cursor::new(short)).is_err());
}
