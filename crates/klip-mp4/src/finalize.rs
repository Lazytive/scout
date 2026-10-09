//! Turning a fragmented recording into a regular MP4 in place.
//!
//! The samples stay where they are. A regular `moov` describing every sample of every
//! complete fragment is appended, then each `moof` and finally the init `moov` become
//! `free` boxes, leaving `ftyp, free, (free, mdat)…, moov`: a plain MP4 that every player
//! and editor reads, with the index at the end.
//!
//! Order matters for crash safety (ADR 0013): until the init `moov` is turned into `free`
//! (the last write), readers that understand fragments still see a fragmented file (they
//! skip a second `moov`), and [`crate::scan_fragmented`] recognises the `free`d fragments,
//! so an interrupted finalise is simply run again.

use std::fs::OpenOptions;
use std::io::{BufReader, Seek, SeekFrom, Write};
use std::path::Path;

use klip_media::{Nanos, TrackKind};

use crate::moov::{MoovKind, TrackPlan, build_moov, group_audio_tracks, ts_to_ns};
use crate::read::{FragmentedScan, scan_fragmented};
use crate::{Marker, Mp4Error, chapters};

/// What [`finalize`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FinalizeSummary {
    /// File size afterwards.
    pub bytes: u64,
    /// Longest track presentation duration.
    pub duration: Nanos,
    pub samples: usize,
    pub fragments: usize,
    /// Bytes cut off the end: an incomplete last fragment (killed recorder) or the `moov`
    /// of an interrupted earlier finalise.
    pub truncated: u64,
    /// The file was already finalised; nothing was changed.
    pub already_final: bool,
}

/// Finalise the fragmented recording at `path` in place. `markers` are relative to the
/// recording's origin (time 0 of the fragmented writer).
pub fn finalize(path: &Path, markers: &[Marker]) -> Result<FinalizeSummary, Mp4Error> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let scan = {
        let mut r = BufReader::with_capacity(64 * 1024, &file);
        scan_fragmented(&mut r)?
    };
    if scan.finalized {
        return Ok(FinalizeSummary {
            bytes: scan.file_len,
            duration: 0,
            samples: 0,
            fragments: scan.fragments,
            truncated: 0,
            already_final: true,
        });
    }
    if scan.fragments == 0 {
        return Err(Mp4Error::NoFragments);
    }
    let built = final_moov(&scan, markers)?;
    let moov_len = u64::try_from(built.moov.len()).map_err(|_| Mp4Error::Internal("moov size"))?;

    file.set_len(scan.data_end)?;
    file.seek(SeekFrom::Start(scan.data_end))?;
    file.write_all(&built.moov)?;
    file.sync_data()?;
    for off in &scan.moofs {
        file.seek(SeekFrom::Start(off.saturating_add(4)))?;
        file.write_all(b"free")?;
    }
    if let Some(off) = scan.init_moov {
        file.seek(SeekFrom::Start(off.saturating_add(4)))?;
        file.write_all(b"free")?;
    }
    file.sync_all()?;
    Ok(FinalizeSummary {
        bytes: scan.data_end.saturating_add(moov_len),
        duration: built.duration,
        samples: built.samples,
        fragments: scan.fragments,
        truncated: scan.file_len.saturating_sub(scan.data_end),
        already_final: false,
    })
}

struct BuiltMoov {
    moov: Vec<u8>,
    duration: Nanos,
    samples: usize,
}

/// The regular `moov` for every complete fragment in `scan`.
fn final_moov(scan: &FragmentedScan, markers: &[Marker]) -> Result<BuiltMoov, Mp4Error> {
    let mut plans: Vec<TrackPlan<'_>> = Vec::new();
    let mut first_pts: Vec<Nanos> = Vec::new();
    for t in scan.tracks.iter().filter(|t| !t.samples.is_empty()) {
        let index = plans.len();
        let mut plan = TrackPlan::empty(index, &t.info, t.name.as_deref(), t.enabled)?;
        if plan.timescale != t.timescale {
            // Audio timescale is the sample rate, which the scan took from mdhd.
            return Err(Mp4Error::Malformed(format!("track {index}: timescale")));
        }
        let n = t.samples.len();
        plan.durations.reserve(n);
        plan.cts_offsets.reserve(n);
        plan.sizes.reserve(n);
        plan.sync.reserve(n);
        let mut iter = t.samples.iter().peekable();
        while let Some(s) = iter.next() {
            // A gap or jitter between fragments goes into the earlier sample's duration.
            let duration = match iter.peek() {
                Some(next) => {
                    let d = next
                        .decode_time
                        .checked_sub(s.decode_time)
                        .ok_or(Mp4Error::NonMonotonic(index))?;
                    u32::try_from(d).map_err(|_| Mp4Error::TooLarge("sample duration"))?
                }
                None => s.duration,
            };
            plan.durations.push(duration);
            plan.cts_offsets.push(s.cts_offset);
            plan.sizes.push(s.size);
            plan.sync.push(s.sync);
        }
        plan.chunks.clone_from(&t.runs);
        plan.finish_timing()?;
        let first = t.samples.first().ok_or(Mp4Error::EmptyTrack(index))?;
        let decode = i64::try_from(first.decode_time).map_err(|_| Mp4Error::TooLarge("time"))?;
        first_pts.push(ts_to_ns(
            decode.saturating_add(i64::from(first.cts_offset)),
            t.timescale,
        ));
        plans.push(plan);
    }
    if plans.is_empty() {
        return Err(Mp4Error::NoFragments);
    }
    // If the enabled audio track recorded nothing, play the first audio track that did.
    let is_audio = |p: &TrackPlan<'_>| p.info.kind() == TrackKind::Audio;
    if !plans.iter().any(|p| is_audio(p) && p.enabled)
        && let Some(p) = plans.iter_mut().find(|p| is_audio(p))
    {
        p.enabled = true;
    }
    group_audio_tracks(&mut plans);
    let origin = first_pts.iter().copied().min().unwrap_or(0);
    for (plan, start) in plans.iter_mut().zip(&first_pts) {
        plan.empty_edit = start.saturating_sub(origin).max(0);
    }
    let duration = plans.iter().map(TrackPlan::total_ns).max().unwrap_or(0);
    let chapters = chapters(markers, origin, duration);
    let co64 = plans
        .iter()
        .flat_map(|p| p.chunks.iter())
        .any(|(o, _)| *o > u64::from(u32::MAX));
    let moov = build_moov(
        &plans,
        &MoovKind::Regular {
            chapters: &chapters,
        },
        co64,
        scan.creation_time_unix,
    )?;
    Ok(BuiltMoov {
        moov,
        duration,
        samples: plans.iter().map(TrackPlan::samples).sum(),
    })
}
