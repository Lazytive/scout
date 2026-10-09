//! Output time ↔ source time. The same functions exist in the front end
//! (`apps/klip-app/src/lib/timeline.ts`); both are checked against `fixtures/*.json`.
//!
//! A project's pieces (kept source ranges, each at a speed) play back to back. For a
//! piece at speed `num/den` (source ms per output ms), the output length is
//! `ceil(src_len * den / num)` and output offset `o` shows source offset
//! `floor(o * num / den)`: at 0.5x every source ms is shown twice, at 2x every second
//! source ms is skipped. All math is integer and checked.

use crate::project::{MAX_PIECES, Piece, Project, Speed};

/// One piece placed on the output timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Segment {
    pub src_start_ms: u64,
    pub src_end_ms: u64,
    pub speed: Speed,
    pub out_start_ms: u64,
    pub out_end_ms: u64,
}

/// Why an edit can't be made.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    #[error("the clip's length isn't known yet")]
    UnknownDuration,
    #[error("the range is empty or outside the edit")]
    BadRange,
    #[error("that would remove the whole clip")]
    NothingLeft,
    #[error("too many pieces (at most {MAX_PIECES})")]
    TooManyPieces,
}

fn out_len(src_len: u64, speed: Speed) -> u64 {
    let (num, den) = speed.ratio();
    src_len
        .saturating_mul(den)
        .saturating_add(num.saturating_sub(1))
        .checked_div(num)
        .unwrap_or(0)
}

fn src_offset(out_off: u64, speed: Speed) -> u64 {
    let (num, den) = speed.ratio();
    out_off.saturating_mul(num).checked_div(den).unwrap_or(0)
}

fn out_offset(src_off: u64, speed: Speed) -> u64 {
    let (num, den) = speed.ratio();
    src_off.saturating_mul(den).checked_div(num).unwrap_or(0)
}

/// The pieces as played: the project's, or the whole clip when it has none (nothing
/// when the clip's length isn't known).
pub fn pieces(p: &Project) -> Vec<Piece> {
    if !p.pieces.is_empty() {
        p.pieces.clone()
    } else if p.source_ms > 0 {
        vec![Piece {
            start_ms: 0,
            end_ms: p.source_ms,
            speed: Speed::Normal,
        }]
    } else {
        Vec::new()
    }
}

/// The output timeline: one segment per piece, back to back from 0.
pub fn segments(p: &Project) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut cursor: u64 = 0;
    for piece in pieces(p) {
        let len = out_len(piece.end_ms.saturating_sub(piece.start_ms), piece.speed);
        let end = cursor.saturating_add(len);
        out.push(Segment {
            src_start_ms: piece.start_ms,
            src_end_ms: piece.end_ms,
            speed: piece.speed,
            out_start_ms: cursor,
            out_end_ms: end,
        });
        cursor = end;
    }
    out
}

/// Length of the edit, ms.
pub fn output_duration_ms(p: &Project) -> u64 {
    segments(p).last().map_or(0, |s| s.out_end_ms)
}

/// The source time shown at output time `out_ms`; `None` at or after the end.
pub fn source_time(p: &Project, out_ms: u64) -> Option<u64> {
    segments(p)
        .iter()
        .find(|s| out_ms >= s.out_start_ms && out_ms < s.out_end_ms)
        .map(|s| {
            s.src_start_ms
                .saturating_add(src_offset(out_ms.saturating_sub(s.out_start_ms), s.speed))
        })
}

/// When source time `src_ms` is shown; `None` if it is cut or past the end.
pub fn output_time(p: &Project, src_ms: u64) -> Option<u64> {
    segments(p)
        .iter()
        .find(|s| src_ms >= s.src_start_ms && src_ms < s.src_end_ms)
        .map(|s| {
            s.out_start_ms
                .saturating_add(out_offset(src_ms.saturating_sub(s.src_start_ms), s.speed))
        })
}

fn materialize(p: &mut Project) -> Result<(), EditError> {
    if p.pieces.is_empty() {
        if p.source_ms == 0 {
            return Err(EditError::UnknownDuration);
        }
        p.pieces = pieces(p);
    }
    Ok(())
}

/// Put a piece boundary at output time `out_ms` (the picture doesn't change). Returns
/// whether a new boundary was made: `false` when one was already there, at the end, or
/// when the time falls inside one source millisecond.
pub fn split(p: &mut Project, out_ms: u64) -> Result<bool, EditError> {
    materialize(p)?;
    let duration = output_duration_ms(p);
    if out_ms > duration {
        return Err(EditError::BadRange);
    }
    if out_ms == duration {
        return Ok(false);
    }
    let segs = segments(p);
    let (i, seg) = segs
        .iter()
        .enumerate()
        .find(|(_, s)| out_ms >= s.out_start_ms && out_ms < s.out_end_ms)
        .ok_or(EditError::BadRange)?;
    if out_ms == seg.out_start_ms {
        return Ok(false);
    }
    let src = seg.src_start_ms.saturating_add(src_offset(
        out_ms.saturating_sub(seg.out_start_ms),
        seg.speed,
    ));
    if src <= seg.src_start_ms || src >= seg.src_end_ms {
        return Ok(false);
    }
    if p.pieces.len() >= MAX_PIECES {
        return Err(EditError::TooManyPieces);
    }
    let piece = *p.pieces.get(i).ok_or(EditError::BadRange)?;
    let first = Piece {
        start_ms: piece.start_ms,
        end_ms: src,
        speed: piece.speed,
    };
    let second = Piece {
        start_ms: src,
        end_ms: piece.end_ms,
        speed: piece.speed,
    };
    if let Some(slot) = p.pieces.get_mut(i) {
        *slot = first;
    }
    let at = i.saturating_add(1).min(p.pieces.len());
    p.pieces.insert(at, second);
    Ok(true)
}

/// The boundary at or just before output time `out_ms` after `split` (the start of its
/// segment; the end for `out_ms` = duration).
fn boundary(p: &Project, out_ms: u64) -> u64 {
    let segs = segments(p);
    segs.iter()
        .find(|s| out_ms >= s.out_start_ms && out_ms < s.out_end_ms)
        .map_or_else(
            || segs.last().map_or(0, |s| s.out_end_ms),
            |s| s.out_start_ms,
        )
}

/// Split at both ends of an output range and return the segments' index range
/// `[first, last)` covering it.
fn isolate(p: &mut Project, out_start: u64, out_end: u64) -> Result<(usize, usize), EditError> {
    if out_start >= out_end {
        return Err(EditError::BadRange);
    }
    materialize(p)?;
    if out_end > output_duration_ms(p) {
        return Err(EditError::BadRange);
    }
    split(p, out_start)?;
    split(p, out_end)?;
    let a = boundary(p, out_start);
    let b = boundary(p, out_end);
    let segs = segments(p);
    let first = segs
        .iter()
        .position(|s| s.out_start_ms >= a)
        .ok_or(EditError::BadRange)?;
    let last = segs
        .iter()
        .rposition(|s| s.out_end_ms <= b)
        .map_or(first, |i| i.saturating_add(1))
        .max(first);
    Ok((first, last))
}

/// Keep only the output range `[out_start, out_end)`.
pub fn trim(p: &mut Project, out_start: u64, out_end: u64) -> Result<(), EditError> {
    let before = p.pieces.clone();
    let (first, last) =
        isolate(p, out_start, out_end).inspect_err(|_| p.pieces = before.clone())?;
    if first >= last {
        p.pieces = before;
        return Err(EditError::NothingLeft);
    }
    p.pieces = p
        .pieces
        .iter()
        .enumerate()
        .filter(|(i, _)| *i >= first && *i < last)
        .map(|(_, piece)| *piece)
        .collect();
    Ok(())
}

/// Remove the output range `[out_start, out_end)`.
pub fn cut(p: &mut Project, out_start: u64, out_end: u64) -> Result<(), EditError> {
    let before = p.pieces.clone();
    let (first, last) =
        isolate(p, out_start, out_end).inspect_err(|_| p.pieces = before.clone())?;
    let kept: Vec<Piece> = p
        .pieces
        .iter()
        .enumerate()
        .filter(|(i, _)| *i < first || *i >= last)
        .map(|(_, piece)| *piece)
        .collect();
    if kept.is_empty() {
        p.pieces = before;
        return Err(EditError::NothingLeft);
    }
    p.pieces = kept;
    Ok(())
}

/// Play the output range `[out_start, out_end)` at `speed`.
pub fn set_speed(
    p: &mut Project,
    out_start: u64,
    out_end: u64,
    speed: Speed,
) -> Result<(), EditError> {
    let before = p.pieces.clone();
    let (first, last) =
        isolate(p, out_start, out_end).inspect_err(|_| p.pieces = before.clone())?;
    for (i, piece) in p.pieces.iter_mut().enumerate() {
        if i >= first && i < last {
            piece.speed = speed;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(source_ms: u64, pieces: &[(u64, u64, Speed)]) -> Project {
        let mut p = Project::new(source_ms);
        p.pieces = pieces
            .iter()
            .map(|&(start_ms, end_ms, speed)| Piece {
                start_ms,
                end_ms,
                speed,
            })
            .collect();
        p
    }

    #[test]
    fn whole_clip_when_no_pieces() {
        let p = Project::new(10_000);
        assert_eq!(output_duration_ms(&p), 10_000);
        assert_eq!(source_time(&p, 0), Some(0));
        assert_eq!(source_time(&p, 9_999), Some(9_999));
        assert_eq!(source_time(&p, 10_000), None);
        assert_eq!(output_time(&p, 5_000), Some(5_000));
        assert_eq!(output_duration_ms(&Project::new(0)), 0);
        assert_eq!(source_time(&Project::new(0), 0), None);
    }

    #[test]
    fn speed_lengths_and_offsets() {
        assert_eq!(out_len(2000, Speed::Double), 1000);
        assert_eq!(out_len(2001, Speed::Double), 1001);
        assert_eq!(out_len(1, Speed::Double), 1);
        assert_eq!(out_len(1001, Speed::Half), 2002);
        assert_eq!(out_len(7, Speed::Normal), 7);
        // The last output ms of a piece always shows a source ms inside it.
        for len in 1..50u64 {
            for s in Speed::ALL {
                let last = out_len(len, s) - 1;
                assert!(src_offset(last, s) < len, "len {len} speed {s:?}");
            }
        }
    }

    #[test]
    fn split_makes_two_pieces_with_the_same_picture() {
        let mut p = project(10_000, &[(1000, 5000, Speed::Normal)]);
        let before: Vec<Option<u64>> = (0..4000).map(|t| source_time(&p, t)).collect();
        assert!(split(&mut p, 1500).unwrap());
        assert_eq!(p.pieces.len(), 2);
        assert_eq!(p.pieces[0].end_ms, 2500);
        assert_eq!(p.pieces[1].start_ms, 2500);
        let after: Vec<Option<u64>> = (0..4000).map(|t| source_time(&p, t)).collect();
        assert_eq!(before, after);
        assert!(!split(&mut p, 1500).unwrap(), "already a boundary");
        assert!(!split(&mut p, 0).unwrap());
        assert!(!split(&mut p, 4000).unwrap(), "the end");
        assert_eq!(split(&mut p, 4001), Err(EditError::BadRange));
    }

    #[test]
    fn split_inside_a_half_speed_source_ms_is_a_no_op() {
        let mut p = project(100, &[(0, 10, Speed::Half)]);
        assert!(
            !split(&mut p, 1).unwrap(),
            "output ms 0 and 1 show source ms 0"
        );
        assert!(split(&mut p, 2).unwrap());
        assert_eq!(p.pieces[0].end_ms, 1);
    }

    #[test]
    fn trim_keeps_the_range() {
        let mut p = Project::new(10_000);
        trim(&mut p, 2000, 7000).unwrap();
        assert_eq!(
            p.pieces,
            vec![Piece {
                start_ms: 2000,
                end_ms: 7000,
                speed: Speed::Normal
            }]
        );
        assert_eq!(output_duration_ms(&p), 5000);
        assert_eq!(source_time(&p, 0), Some(2000));
        assert_eq!(trim(&mut p, 3000, 3000), Err(EditError::BadRange));
        assert_eq!(trim(&mut p, 0, 5001), Err(EditError::BadRange));
        assert_eq!(
            trim(&mut Project::new(0), 0, 10),
            Err(EditError::UnknownDuration)
        );
    }

    #[test]
    fn cut_removes_the_middle_and_shifts_output() {
        let mut p = Project::new(10_000);
        cut(&mut p, 3000, 5000).unwrap();
        assert_eq!(
            p.pieces,
            vec![
                Piece {
                    start_ms: 0,
                    end_ms: 3000,
                    speed: Speed::Normal
                },
                Piece {
                    start_ms: 5000,
                    end_ms: 10_000,
                    speed: Speed::Normal
                },
            ]
        );
        assert_eq!(output_duration_ms(&p), 8000);
        assert_eq!(source_time(&p, 3000), Some(5000));
        assert_eq!(output_time(&p, 4000), None);
        assert_eq!(output_time(&p, 5000), Some(3000));
        // Cutting everything is refused and leaves the project as it was.
        let before = p.clone();
        assert_eq!(cut(&mut p, 0, 8000), Err(EditError::NothingLeft));
        assert_eq!(p, before);
    }

    #[test]
    fn set_speed_splits_and_changes_only_the_range() {
        let mut p = Project::new(10_000);
        set_speed(&mut p, 2000, 4000, Speed::Double).unwrap();
        assert_eq!(
            p.pieces,
            vec![
                Piece {
                    start_ms: 0,
                    end_ms: 2000,
                    speed: Speed::Normal
                },
                Piece {
                    start_ms: 2000,
                    end_ms: 4000,
                    speed: Speed::Double
                },
                Piece {
                    start_ms: 4000,
                    end_ms: 10_000,
                    speed: Speed::Normal
                },
            ]
        );
        assert_eq!(output_duration_ms(&p), 9000);
        // Now slow the (already shortened) middle piece in output time 2000–3000.
        set_speed(&mut p, 2000, 3000, Speed::Half).unwrap();
        assert_eq!(p.pieces.len(), 3);
        assert_eq!(p.pieces[1].speed, Speed::Half);
        assert_eq!(output_duration_ms(&p), 2000 + 4000 + 6000);
    }

    #[test]
    fn too_many_pieces_is_an_error() {
        let mut p = Project::new(100_000);
        for i in 1..MAX_PIECES as u64 {
            assert!(split(&mut p, i * 10).unwrap());
        }
        assert_eq!(p.pieces.len(), MAX_PIECES);
        assert_eq!(split(&mut p, 5), Err(EditError::TooManyPieces));
    }
}
