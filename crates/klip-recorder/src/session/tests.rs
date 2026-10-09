use std::io::Cursor;

use klip_media::{AudioParams, Codec, TrackParams, VideoParams};
use klip_mp4::scan_fragmented;

use super::*;

const FRAME: Nanos = NANOS_PER_SEC / 60;
const AAC: Nanos = NANOS_PER_SEC * 1024 / 48_000;
/// Capture timeline position where the test streams start.
const T0: Nanos = 100 * NANOS_PER_SEC;

fn video_info(tag: u8) -> TrackInfo {
    TrackInfo {
        codec: Codec::H264,
        codec_data: vec![1, 0x64, 0, 0x28, 0xFF, 0xE1, 0, tag].into(),
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
        codec_data: vec![0x11u8, 0x90].into(),
        params: TrackParams::Audio(AudioParams {
            sample_rate: 48_000,
            channels: 2,
        }),
        bitrate: Some(160_000),
    }
}

/// Packet with payload = 4-byte index + tag, so the file can be checked sample by sample.
fn packet(i: i64, step: Nanos, key: bool, tag: u8) -> Packet {
    let mut data = (i as u32).to_be_bytes().to_vec();
    data.push(tag);
    data.extend(std::iter::repeat_n(0xCD, (i % 13) as usize));
    Packet {
        pts: T0 + i * step,
        dts: T0 + i * step,
        duration: step,
        keyframe: key,
        data: data.into(),
    }
}

fn cfg(dir: &Path) -> SessionConfig {
    SessionConfig {
        dir: dir.to_path_buf(),
        min_free_bytes: 0,
        max_queue_bytes: DEFAULT_MAX_QUEUE_BYTES,
    }
}

fn names() -> Vec<String> {
    vec![String::new(), "Mix".into()]
}

/// Feed `secs` of 60 fps video (keyframe every 60) and AAC audio arriving ~40 ms late, in
/// arrival order.
fn feed(tap: &SessionTap, from_frame: i64, frames: i64) {
    let mut a = (from_frame * FRAME) / AAC;
    for f in from_frame..from_frame + frames {
        assert!(tap.packet(0, packet(f, FRAME, f % 60 == 0, b'V')));
        // Audio up to 40 ms behind this frame.
        while (a + 1) * AAC + 40_000_000 <= f * FRAME {
            assert!(tap.packet(1, packet(a, AAC, true, b'A')));
            a += 1;
        }
    }
}

fn wait_for(mut cond: impl FnMut() -> bool) {
    let t = Instant::now();
    while !cond() {
        assert!(t.elapsed() < Duration::from_secs(10), "timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Indices of the samples of `track` in a scanned file.
fn indices(d: &[u8], track: usize) -> Vec<u32> {
    let s = scan_fragmented(&mut Cursor::new(d)).unwrap();
    let t = &s.tracks[track];
    let mut out = Vec::new();
    let mut sample = 0usize;
    for (off, n) in &t.runs {
        let mut pos = *off as usize;
        for _ in 0..*n {
            out.push(u32::from_be_bytes(d[pos..pos + 4].try_into().unwrap()));
            pos += t.samples[sample].size as usize;
            sample += 1;
        }
    }
    out
}

#[test]
fn records_every_packet_once_and_finalises_with_bookmarks() {
    let dir = tempfile::tempdir().unwrap();
    let (session, tap) = Session::start(&cfg(dir.path()), names()).unwrap();
    assert!(tap.info(0, video_info(0)));
    assert!(tap.info(1, audio_info()));
    // Delta frames before the first keyframe are skipped: start mid-GOP at frame 30.
    for f in 30..60 {
        tap.packet(0, packet(f, FRAME, false, b'V'));
    }
    // A bookmark before the file exists is applied when it starts.
    session.bookmark(T0 + 90 * FRAME).unwrap();
    feed(&tap, 60, 300);
    wait_for(|| session.status()["current"]["fragments"].as_u64() >= Some(3));
    let rel = session.bookmark(T0 + 200 * FRAME).unwrap();
    assert_eq!(rel, Some(140 * FRAME), "relative to the first keyframe");
    let path = session.path();
    let sidecar = sidecar_path(&path);
    assert!(sidecar.exists(), "sidecar while recording");
    drop(tap);
    let summary = session.stop(None);
    assert_eq!(summary.error, None);
    assert_eq!(summary.files.len(), 1);
    let f = &summary.files[0];
    assert_eq!(f.error, None);
    assert_eq!(f.bookmarks, 2);
    assert!(!sidecar.exists(), "sidecar removed once final");

    let d = fs::read(&f.path).unwrap();
    let s = scan_fragmented(&mut Cursor::new(&d)).unwrap();
    assert!(s.finalized);
    let v = indices(&d, 0);
    assert_eq!(
        v,
        (60..360).collect::<Vec<u32>>(),
        "every video frame once, in order"
    );
    let a = indices(&d, 1);
    let first_audio = ((60 * FRAME) + AAC - 1) / AAC;
    assert_eq!(
        a.first().copied(),
        Some(first_audio as u32),
        "audio from the origin on"
    );
    assert!(a.windows(2).all(|w| w[1] == w[0] + 1), "audio continuous");
    // Stopping writes everything queued, including the held-back newest packet.
    assert_eq!(
        *a.last().unwrap() as i64,
        (359 * FRAME - 40_000_000) / AAC - 1
    );
    assert!(
        (4990..=5000).contains(&nanos_to_millis(f.duration)),
        "{}",
        f.duration
    );
    // Chapters: the pending bookmark at 0.5 s, the other at 140 frames.
    let chpl = d.windows(4).position(|w| w == b"chpl").unwrap();
    assert_eq!(d[chpl + 4 + 4 + 4], 2);
    let t0 = u64::from_be_bytes(d[chpl + 13..chpl + 21].try_into().unwrap());
    assert_eq!(t0 as i64, 30 * FRAME / 100);
}

#[test]
fn a_copy_taken_while_recording_is_a_playable_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let (session, tap) = Session::start(&cfg(dir.path()), names()).unwrap();
    tap.info(0, video_info(0));
    tap.info(1, audio_info());
    feed(&tap, 0, 250);
    wait_for(|| session.status()["current"]["fragments"].as_u64() >= Some(4));
    // What a killed recorder leaves: the bytes on disk right now.
    let copy = fs::read(session.path()).unwrap();
    let s = scan_fragmented(&mut Cursor::new(&copy)).unwrap();
    assert!(!s.finalized);
    assert!(s.fragments >= 3);
    let v = indices(&copy, 0);
    assert_eq!(v, (0..v.len() as u32).collect::<Vec<_>>());
    assert_eq!(v.len() % 60, 0, "fragments end at keyframes");
    drop(tap);
    let summary = session.stop(None);
    assert_eq!(summary.files.len(), 1);
}

#[test]
fn a_new_video_description_rolls_over_to_a_new_file() {
    let dir = tempfile::tempdir().unwrap();
    let (session, tap) = Session::start(&cfg(dir.path()), names()).unwrap();
    tap.info(0, video_info(0));
    tap.info(1, audio_info());
    feed(&tap, 0, 180);
    wait_for(|| session.status()["current"]["fragments"].as_u64() >= Some(2));
    tap.info(0, video_info(1));
    feed(&tap, 180, 180);
    drop(tap);
    let summary = session.stop(None);
    assert_eq!(summary.error, None);
    assert_eq!(summary.files.len(), 2, "{summary:?}");
    let first = indices(&fs::read(&summary.files[0].path).unwrap(), 0);
    let second = indices(&fs::read(&summary.files[1].path).unwrap(), 0);
    assert_eq!(first, (0..180).collect::<Vec<u32>>());
    assert_eq!(second, (180..360).collect::<Vec<u32>>());
    assert_ne!(summary.files[0].path, summary.files[1].path);
}

#[test]
fn queue_overflow_ends_the_session_with_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = cfg(dir.path());
    c.max_queue_bytes = 1;
    let (session, tap) = Session::start(&c, names()).unwrap();
    tap.info(0, video_info(0));
    feed(&tap, 0, 600);
    wait_for(|| session.is_finished());
    let summary = session.stop(None);
    assert!(summary.error.unwrap().contains("can't keep up"));
}

fn old(path: &Path) {
    let f = File::options().write(true).open(path).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(3600))
        .unwrap();
}

#[test]
fn leftover_recordings_are_recovered_with_their_bookmarks() {
    let dir = tempfile::tempdir().unwrap();
    // A recording killed after 3 fragments, with one bookmark in its sidecar.
    let (session, tap) = Session::start(&cfg(dir.path()), names()).unwrap();
    tap.info(0, video_info(0));
    tap.info(1, audio_info());
    feed(&tap, 0, 200);
    wait_for(|| session.status()["current"]["fragments"].as_u64() >= Some(3));
    session.bookmark(T0 + 30 * FRAME).unwrap();
    let path = session.path();
    let killed = fs::read(&path).unwrap();
    let sidecar_text = fs::read(sidecar_path(&path)).unwrap();
    drop(tap);
    let _ = session.stop(None);
    let rec = dir.path().join("killed.mp4");
    fs::write(&rec, &killed).unwrap();
    fs::write(sidecar_path(&rec), &sidecar_text).unwrap();
    // One killed before its first fragment: nothing to keep.
    let empty = dir.path().join("empty.mp4");
    let init_end = scan_fragmented(&mut Cursor::new(&killed)).unwrap().moofs[0] as usize;
    fs::write(&empty, &killed[..init_end]).unwrap();
    fs::write(sidecar_path(&empty), b"{\"klip_recording\":1}").unwrap();
    // A sidecar whose file is gone, and one with garbage.
    fs::write(sidecar_path(&dir.path().join("gone.mp4")), b"{}").unwrap();
    let garbage = dir.path().join("garbage.mp4");
    fs::write(&garbage, &killed).unwrap();
    fs::write(sidecar_path(&garbage), b"\xff\xfe not json").unwrap();
    // Too recent: maybe another recorder is still writing it.
    let fresh = dir.path().join("fresh.mp4");
    fs::write(&fresh, &killed).unwrap();
    fs::write(sidecar_path(&fresh), &sidecar_text).unwrap();
    for p in [&rec, &empty, &garbage] {
        old(p);
        old(&sidecar_path(p));
    }
    old(&sidecar_path(&dir.path().join("gone.mp4")));

    let mut out = recover_recordings(dir.path());
    out.sort_by(|a, b| a.path.cmp(&b.path));
    let names: Vec<_> = out
        .iter()
        .map(|r| {
            (
                r.path.file_name().unwrap().to_string_lossy().into_owned(),
                r.result.clone(),
            )
        })
        .collect();
    assert_eq!(
        names,
        [
            ("empty.mp4".into(), Ok(false)),
            ("garbage.mp4".into(), Ok(true)),
            ("gone.mp4".into(), Ok(false)),
            ("killed.mp4".into(), Ok(true)),
        ]
    );
    assert!(!empty.exists());
    for p in [&rec, &garbage] {
        assert!(!sidecar_path(p).exists());
        let d = fs::read(p).unwrap();
        assert!(scan_fragmented(&mut Cursor::new(&d)).unwrap().finalized);
    }
    let d = fs::read(&rec).unwrap();
    assert!(d.windows(4).any(|w| w == b"chpl"), "bookmark kept");
    assert!(
        fresh.exists() && sidecar_path(&fresh).exists(),
        "fresh one left alone"
    );
}
