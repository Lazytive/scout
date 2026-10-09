//! Hostile and random input: nothing panics, repair is idempotent, the timeline math
//! is consistent for any repaired project.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use klip_edit::{Piece, Project, Speed, timeline};
use proptest::prelude::*;
use serde_json::{Value, json};

fn json_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| json!(n)),
        any::<f64>()
            .prop_map(|f| serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)),
        "[ -~]{0,20}".prop_map(Value::String),
    ];
    leaf.prop_recursive(4, 64, 8, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..8).prop_map(Value::Array),
            prop::collection::btree_map("[a-z_]{1,10}", inner, 0..8)
                .prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}

/// A project-shaped object with the right keys but random contents.
fn project_shaped() -> impl Strategy<Value = Value> {
    let piece = (
        any::<u64>(),
        any::<u64>(),
        prop_oneof![Just(0.5), Just(1.0), Just(2.0), Just(3.0)],
    )
        .prop_map(|(a, b, s)| json!({"start_ms": a % 20_000, "end_ms": b % 20_000, "speed": s}));
    let layer = (any::<u32>(), "[a-z]{0,8}", "[ -~\\n\\t]{0,300}", any::<u64>(), any::<u64>()).prop_map(
        |(id, preset, text, a, b)| json!({"id": id % 8, "preset": preset, "text": text, "start_ms": a % 30_000, "end_ms": b % 30_000}),
    );
    (
        any::<u32>(),
        any::<u64>(),
        prop::collection::vec(piece, 0..12),
        prop::collection::vec(layer, 0..6),
        any::<f32>(),
        any::<f32>(),
        any::<f32>(),
        json_value(),
    )
        .prop_map(|(version, source, pieces, text, g, m, crop, extra)| {
            json!({
                "version": version,
                "source_ms": source % 20_000,
                "pieces": pieces,
                "text": text,
                "volumes": {"game": g, "mic": m},
                "format": {"kind": "tall", "crop_x": crop},
                "something_advanced": extra
            })
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    #[test]
    fn any_json_loads_and_repair_is_idempotent(v in json_value()) {
        let (mut p, _) = Project::from_value(v);
        prop_assert!(p.repair().is_empty());
        let (again, issues) = Project::from_value(p.to_value().unwrap());
        prop_assert!(issues.is_empty(), "{issues:?}");
        prop_assert_eq!(again, p);
    }

    #[test]
    fn project_shaped_json_gives_a_consistent_timeline(v in project_shaped()) {
        let (mut p, _) = Project::from_value(v);
        prop_assert!(p.repair().is_empty());
        let (again, issues) = Project::from_value(p.to_value().unwrap());
        prop_assert!(issues.is_empty(), "{issues:?}");
        prop_assert_eq!(&again, &p);
        check_timeline(&p)?;
    }

    #[test]
    fn edits_keep_the_timeline_consistent(
        source in 1u64..5000,
        ops in prop::collection::vec((0u8..4, 0u64..6000, 0u64..6000, 0usize..3), 0..12),
    ) {
        let mut p = Project::new(source);
        for (op, a, b, s) in ops {
            let speed = Speed::ALL[s];
            let before = p.clone();
            let res = match op {
                0 => timeline::split(&mut p, a).map(|_| ()),
                1 => timeline::trim(&mut p, a.min(b), a.max(b)),
                2 => timeline::cut(&mut p, a.min(b), a.max(b)),
                _ => timeline::set_speed(&mut p, a.min(b), a.max(b), speed),
            };
            if res.is_err() {
                // A refused edit changes nothing the viewer can see.
                prop_assert_eq!(timeline::output_duration_ms(&p), timeline::output_duration_ms(&before));
            }
            prop_assert!(p.clone().repair().is_empty(), "edits never need repair");
            prop_assert!(timeline::output_duration_ms(&p) > 0);
            check_timeline(&p)?;
        }
    }
}

fn check_timeline(p: &Project) -> Result<(), TestCaseError> {
    let segs = timeline::segments(p);
    let duration = timeline::output_duration_ms(p);
    let mut cursor = 0;
    for s in &segs {
        prop_assert_eq!(s.out_start_ms, cursor);
        prop_assert!(s.out_end_ms > s.out_start_ms, "no empty segment: {:?}", s);
        prop_assert!(s.src_end_ms > s.src_start_ms);
        cursor = s.out_end_ms;
    }
    prop_assert_eq!(cursor, duration);
    // Pieces are in source order and don't overlap.
    let pieces: Vec<Piece> = timeline::pieces(p);
    for w in pieces.windows(2) {
        prop_assert!(w[0].end_ms <= w[1].start_ms);
    }
    // source_time is defined exactly on [0, duration), non-decreasing, inside the pieces,
    // and output_time maps back within one ms (speed rounding).
    let mut last = 0;
    let step = (duration / 64).max(1);
    let mut t = 0;
    while t < duration {
        let src = timeline::source_time(p, t).expect("inside the edit");
        prop_assert!(src >= last);
        last = src;
        prop_assert!(
            pieces
                .iter()
                .any(|pc| src >= pc.start_ms && src < pc.end_ms)
        );
        let back = timeline::output_time(p, src).expect("shown source time is on the timeline");
        prop_assert!(back <= t && t - back <= 1, "t {t} src {src} back {back}");
        t += step;
    }
    prop_assert_eq!(timeline::source_time(p, duration), None);
    prop_assert_eq!(timeline::source_time(p, u64::MAX), None);
    prop_assert_eq!(timeline::output_time(p, u64::MAX), None);
    Ok(())
}
