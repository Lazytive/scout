use bytes::Bytes;
use klip_media::{AudioParams, Codec, NANOS_PER_SEC, TrackParams, VideoParams};
use proptest::prelude::*;

use super::*;

const FRAME: Nanos = NANOS_PER_SEC / 60;

fn video_info(tag: u8) -> TrackInfo {
    TrackInfo {
        codec: Codec::H264,
        codec_data: Bytes::from(vec![1, tag]),
        params: TrackParams::Video(VideoParams {
            width: 1280,
            height: 720,
            colorimetry: None,
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
        bitrate: None,
    }
}

fn pkt(i: i64, key: bool, size: usize) -> Packet {
    Packet {
        pts: i * FRAME,
        dts: i * FRAME,
        duration: FRAME,
        keyframe: key,
        data: Bytes::from(vec![0u8; size]),
    }
}

fn video_ring(max_secs: i64, max_bytes: u64) -> Ring {
    let mut r = Ring::new(
        RingConfig {
            max_duration: max_secs * NANOS_PER_SEC,
            max_bytes,
        },
        &[TrackKind::Video],
    )
    .unwrap();
    r.set_track_info(0, video_info(0)).unwrap();
    r
}

/// Push `frames` frames at 60 fps with a keyframe every 60 frames.
fn fill(r: &mut Ring, from: i64, frames: i64) {
    for i in from..from + frames {
        r.push(0, pkt(i, i % 60 == 0, 100)).unwrap();
    }
}

#[test]
fn needs_a_video_track() {
    let cfg = RingConfig {
        max_duration: NANOS_PER_SEC,
        max_bytes: 1000,
    };
    assert!(Ring::new(cfg, &[TrackKind::Audio]).is_none());
    assert!(Ring::new(cfg, &[TrackKind::Audio, TrackKind::Video]).is_some());
}

#[test]
fn eviction_keeps_whole_gops_and_at_least_max_duration() {
    let mut r = video_ring(10, u64::MAX);
    fill(&mut r, 0, 60 * 30);
    let s = r.stats();
    assert!(s.buffered >= 10 * NANOS_PER_SEC, "{s:?}");
    assert!(s.buffered < 11 * NANOS_PER_SEC, "{s:?}");
    let snap = r.snapshot(i64::MAX).unwrap();
    assert!(snap.tracks[0].packets[0].keyframe);
    assert_eq!(snap.tracks[0].packets[0].dts % (60 * FRAME), 0);
}

#[test]
fn byte_cap_is_hard() {
    let mut r = video_ring(1000, 50_000);
    fill(&mut r, 0, 60 * 20);
    let s = r.stats();
    assert!(s.bytes <= 50_000, "{s:?}");
    assert!(s.evicted > 0);
    // Still starts at a keyframe.
    let snap = r.snapshot(i64::MAX).unwrap();
    assert!(snap.tracks[0].packets[0].keyframe);
}

#[test]
fn snapshot_starts_at_keyframe_at_or_before_target() {
    let mut r = video_ring(120, u64::MAX);
    fill(&mut r, 0, 60 * 40 + 30); // 40.5 s
    let snap = r.snapshot(30 * NANOS_PER_SEC).unwrap();
    // End = 40.5 s, target = 10.5 s, keyframe at or before = 10 s.
    assert_eq!(snap.start, 600 * FRAME);
    assert_eq!(snap.end, (60 * 40 + 30) * FRAME);
    assert!(snap.duration() >= 30 * NANOS_PER_SEC);
    assert!(snap.duration() <= 31 * NANOS_PER_SEC);
    let v = &snap.tracks[0].packets;
    assert!(v[0].keyframe);
    assert_eq!(v.len() as i64, 60 * 40 + 30 - 600);
}

#[test]
fn snapshot_tolerates_timestamp_rounding() {
    // 10 s of 60 fps with truncated frame durations: keyframe 480 lands 80 ns after the
    // 2 s target. It must still be chosen instead of keyframe 420 (a whole extra second).
    let mut r = video_ring(120, u64::MAX);
    fill(&mut r, 0, 600);
    let snap = r.snapshot(2 * NANOS_PER_SEC).unwrap();
    assert_eq!(snap.start, 480 * FRAME);
    assert_eq!(snap.tracks[0].packets.len(), 120);
}

#[test]
fn snapshot_longer_than_buffer_returns_everything() {
    let mut r = video_ring(120, u64::MAX);
    fill(&mut r, 0, 90);
    let snap = r.snapshot(30 * NANOS_PER_SEC).unwrap();
    assert_eq!(snap.start, 0);
    assert_eq!(snap.tracks[0].packets.len(), 90);
}

#[test]
fn empty_and_invalid_snapshots() {
    let r = video_ring(10, u64::MAX);
    assert_eq!(r.snapshot(NANOS_PER_SEC).unwrap_err(), SnapshotError::Empty);
    assert_eq!(r.snapshot(0).unwrap_err(), SnapshotError::InvalidLength);
}

#[test]
fn leading_delta_frames_are_refused() {
    let mut r = video_ring(10, u64::MAX);
    assert_eq!(
        r.push(0, pkt(0, false, 10)),
        Err(PushError::WaitingForKeyframe)
    );
    r.push(0, pkt(1, true, 10)).unwrap();
    r.push(0, pkt(2, false, 10)).unwrap();
    assert_eq!(r.stats().rejected, 1);
}

#[test]
fn hostile_packets_are_refused_without_panicking() {
    let mut r = video_ring(10, u64::MAX);
    r.push(0, pkt(5, true, 10)).unwrap();
    assert!(matches!(
        r.push(0, pkt(4, false, 10)),
        Err(PushError::NonMonotonicDts { .. })
    ));
    assert_eq!(r.push(3, pkt(6, false, 10)), Err(PushError::NoSuchTrack(3)));
    let huge = Packet {
        data: Bytes::from(vec![0u8; MAX_PACKET_BYTES + 1]),
        ..pkt(6, false, 0)
    };
    assert!(matches!(r.push(0, huge), Err(PushError::PacketTooLarge(_))));
    // Extreme timestamps saturate instead of overflowing.
    let extreme = Packet {
        pts: i64::MAX,
        dts: i64::MAX,
        duration: i64::MAX,
        keyframe: true,
        data: Bytes::new(),
    };
    r.push(0, extreme).unwrap();
    let _ = r.stats();
    let _ = r.snapshot(i64::MAX).unwrap();
}

#[test]
fn track_info_change_resets_track() {
    let mut r = video_ring(10, u64::MAX);
    fill(&mut r, 0, 120);
    // Same info again: nothing happens.
    r.set_track_info(0, video_info(0)).unwrap();
    assert_eq!(r.stats().packets[0], 120);
    // New SPS/PPS: old packets can't be muxed with it, so they go.
    r.set_track_info(0, video_info(1)).unwrap();
    assert_eq!(r.stats().packets[0], 0);
    assert!(matches!(
        r.set_track_info(0, audio_info()),
        Err(PushError::KindMismatch)
    ));
}

#[test]
fn packets_before_track_info_are_refused() {
    let mut r = Ring::new(
        RingConfig {
            max_duration: NANOS_PER_SEC,
            max_bytes: u64::MAX,
        },
        &[TrackKind::Video],
    )
    .unwrap();
    assert_eq!(r.push(0, pkt(0, true, 1)), Err(PushError::TrackNotReady(0)));
}

#[test]
fn audio_is_trimmed_to_anchor_and_included_in_snapshots() {
    let mut r = Ring::new(
        RingConfig {
            max_duration: 5 * NANOS_PER_SEC,
            max_bytes: u64::MAX,
        },
        &[TrackKind::Video, TrackKind::Audio, TrackKind::Audio],
    )
    .unwrap();
    r.set_track_info(0, video_info(0)).unwrap();
    r.set_track_info(1, audio_info()).unwrap();
    r.set_track_info(2, audio_info()).unwrap();
    // 20 ms audio packets (track 2; track 1 stays empty) with 60 fps video for 12 s.
    let audio_step = NANOS_PER_SEC / 50;
    let mut next_audio = 0;
    for i in 0..60 * 12 {
        r.push(0, pkt(i, i % 60 == 0, 100)).unwrap();
        while next_audio <= i * FRAME {
            let a = Packet {
                pts: next_audio,
                dts: next_audio,
                duration: audio_step,
                keyframe: true,
                data: Bytes::from_static(&[0; 10]),
            };
            r.push(2, a).unwrap();
            next_audio += audio_step;
        }
    }
    let snap = r.snapshot(i64::MAX).unwrap();
    // The empty track is left out; snapshot tracks keep their ring index.
    assert_eq!(snap.tracks.len(), 2);
    assert_eq!((snap.tracks[0].track, snap.tracks[1].track), (0, 2));
    let a = &snap.tracks[1].packets;
    assert!(a[0].pts >= snap.start);
    assert!(a[0].pts < snap.start + audio_step);
    assert!(a.last().unwrap().pts < snap.end);
    let s = r.stats();
    assert!(s.buffered >= 5 * NANOS_PER_SEC && s.buffered < 6 * NANOS_PER_SEC);
}

#[derive(Debug, Clone)]
struct Step {
    track: usize,
    advance: i64,
    key: bool,
    size: usize,
}

fn step() -> impl Strategy<Value = Step> {
    (
        0usize..3,
        0i64..50_000_000,
        proptest::bool::weighted(0.05),
        0usize..5000,
    )
        .prop_map(|(track, advance, key, size)| Step {
            track,
            advance,
            key,
            size,
        })
}

proptest! {
    #[test]
    fn invariants_hold_for_any_input(
        steps in proptest::collection::vec(step(), 1..600),
        max_secs in 1i64..20,
        max_bytes in 1u64..400_000,
    ) {
        let mut r = Ring::new(
            RingConfig { max_duration: max_secs * NANOS_PER_SEC, max_bytes },
            &[TrackKind::Video, TrackKind::Audio, TrackKind::Audio],
        ).unwrap();
        r.set_track_info(0, video_info(0)).unwrap();
        r.set_track_info(1, audio_info()).unwrap();
        r.set_track_info(2, audio_info()).unwrap();
        let mut t = [0i64; 3];
        for s in steps {
            t[s.track] += s.advance;
            let p = Packet {
                pts: t[s.track],
                dts: t[s.track],
                duration: s.advance,
                keyframe: s.track != 0 || s.key,
                data: Bytes::from(vec![0u8; s.size]),
            };
            let _ = r.push(s.track, p);
            let st = r.stats();
            prop_assert!(st.bytes <= max_bytes);
            if let Ok(snap) = r.snapshot(NANOS_PER_SEC) {
                prop_assert!(snap.tracks[0].packets[0].keyframe);
                prop_assert!(snap.start <= snap.end);
            }
        }
    }
}

#[test]
fn snapshot_until_cuts_at_the_request_time() {
    let mut r = Ring::new(
        RingConfig {
            max_duration: 60 * NANOS_PER_SEC,
            max_bytes: u64::MAX,
        },
        &[TrackKind::Video, TrackKind::Audio, TrackKind::Audio],
    )
    .unwrap();
    r.set_track_info(0, video_info(0)).unwrap();
    r.set_track_info(1, audio_info()).unwrap();
    let audio_step = NANOS_PER_SEC / 50;
    for i in 0..600 {
        r.push(0, pkt(i, i % 60 == 0, 100)).unwrap();
    }
    for k in 0..500 {
        let t = k * audio_step;
        r.push(
            1,
            Packet {
                pts: t,
                dts: t,
                duration: audio_step,
                keyframe: true,
                data: Bytes::from_static(&[0; 10]),
            },
        )
        .unwrap();
    }
    assert_eq!(r.anchor_end(), Some(600 * FRAME));
    assert_eq!(r.track_end(1), Some(10 * NANOS_PER_SEC));
    assert_eq!(r.track_end(2), None);
    assert_eq!(r.active_secondary_tracks(), [1]);
    let until = 400 * FRAME;
    let snap = r.snapshot_until(Some(until), 2 * NANOS_PER_SEC).unwrap();
    let v = &snap.tracks[0].packets;
    assert_eq!(v.last().unwrap().dts, 399 * FRAME);
    // End 400 frames, 2 s back is frame 280: the keyframe at or before it is 240.
    assert_eq!(v[0].dts, 240 * FRAME);
    assert_eq!(snap.end, until);
    let a = &snap.tracks[1].packets;
    assert!(a.last().unwrap().pts < until && a.last().unwrap().dts_end() >= until);
    assert_eq!(
        r.snapshot_until(Some(0), NANOS_PER_SEC).unwrap_err(),
        SnapshotError::Empty
    );
    // No limit is the same as a plain snapshot.
    assert_eq!(
        r.snapshot_until(None, NANOS_PER_SEC).unwrap().end,
        r.snapshot(NANOS_PER_SEC).unwrap().end
    );
}

#[test]
fn tail_starts_at_the_newest_keyframe_and_lists_every_described_track() {
    let mut r = Ring::new(
        RingConfig {
            max_duration: 10 * NANOS_PER_SEC,
            max_bytes: u64::MAX,
        },
        &[TrackKind::Video, TrackKind::Audio, TrackKind::Audio],
    )
    .unwrap();
    r.set_track_info(0, video_info(0)).unwrap();
    r.set_track_info(1, audio_info()).unwrap();
    // Track 2 is never described: left out.
    let empty = r.tail_from_last_keyframe();
    assert_eq!(empty.len(), 2);
    assert!(empty.iter().all(|t| t.packets.is_empty()));
    fill(&mut r, 0, 150);
    for i in 0..150 {
        r.push(1, pkt(i, true, 10)).unwrap();
    }
    let tail = r.tail_from_last_keyframe();
    assert_eq!(tail.len(), 2);
    assert_eq!(tail[0].packets.first().unwrap().dts, 120 * FRAME);
    assert!(tail[0].packets.first().unwrap().keyframe);
    assert_eq!(tail[0].packets.len(), 30);
    assert_eq!(tail[1].track, 1);
    assert_eq!(tail[1].packets.len(), 30);
    assert_eq!(tail[1].packets[0].dts, 120 * FRAME);
}
