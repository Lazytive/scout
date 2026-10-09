use bytes::Bytes;
use klip_media::{AudioParams, Colorimetry, TrackParams, VideoParams};

use super::*;

/// Tiny box parser for tests: returns (type, payload range) children of a byte range.
fn children(data: &[u8], start: usize, end: usize) -> Vec<([u8; 4], usize, usize)> {
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

fn find(data: &[u8], path: &str) -> (usize, usize) {
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

fn be32(d: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(d[at..at + 4].try_into().unwrap())
}

fn video_info() -> TrackInfo {
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

fn audio_info() -> TrackInfo {
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

const FRAME: i64 = NANOS_PER_SEC / 60;

/// Video packets with unique payloads: 4-byte big-endian sample index + track tag.
fn video_packets(n: usize, start: i64) -> Vec<Packet> {
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

fn audio_packets(n: usize, start: i64) -> Vec<Packet> {
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
    let tracks = [
        Mp4Track {
            info: &vi,
            packets: &v,
        },
        Mp4Track {
            info: &ai,
            packets: &a,
        },
    ];
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
        &[
            Mp4Track {
                info: &vi,
                packets: &v,
            },
            Mp4Track {
                info: &ai,
                packets: &a,
            },
        ],
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
    let (d, _) = write(
        &[Mp4Track {
            info: &vi,
            packets: &v,
        }],
        Mp4Options::default(),
    );
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
        &[Mp4Track {
            info: &vi,
            packets: &v,
        }],
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
        write_mp4(
            &mut out,
            &[Mp4Track {
                info: &vi,
                packets: &[]
            }],
            &Mp4Options::default()
        ),
        Err(Mp4Error::EmptyTrack(0))
    ));
    let mut v = video_packets(3, 0);
    v[2].dts = -5;
    assert!(matches!(
        write_mp4(
            &mut out,
            &[Mp4Track {
                info: &vi,
                packets: &v
            }],
            &Mp4Options::default()
        ),
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
            &[Mp4Track {
                info: &huge,
                packets: &v
            }],
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
    assert!(
        write_mp4(
            &mut out,
            &[Mp4Track {
                info: &vi,
                packets: &v
            }],
            &Mp4Options::default()
        )
        .is_err()
    );
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
            &[Mp4Track {
                info: &opus,
                packets: &a
            }],
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
        &[
            Mp4Track {
                info: &vi,
                packets: &v,
            },
            Mp4Track {
                info: &opus,
                packets: &a,
            },
        ],
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
    let tracks = [Mp4Track {
        info: &vi,
        packets: &v,
    }];
    let s = save_mp4_atomic(&path, &tracks, &Mp4Options::default()).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), s.bytes);
    assert!(matches!(
        save_mp4_atomic(&path, &tracks, &Mp4Options::default()),
        Err(Mp4Error::AlreadyExists(_))
    ));
    // A failing write removes its temp file.
    let bad = dir.path().join("bad.mp4");
    assert!(
        save_mp4_atomic(
            &bad,
            &[Mp4Track {
                info: &vi,
                packets: &[]
            }],
            &Mp4Options::default()
        )
        .is_err()
    );
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
