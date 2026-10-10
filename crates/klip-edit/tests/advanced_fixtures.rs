//! The Advanced-mode fixtures shared with the front end (`fixtures/advanced/*.json`):
//! keyframe evaluation and speed ramps give the same numbers in Rust and TypeScript.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::Path;

use klip_edit::advanced::keyframes::{transform_at, value_at};
use klip_edit::advanced::ramp::{output_length, source_offset};
use klip_edit::advanced::{Keyframe, SpeedPoint, Transform, TransformKey};
use serde_json::Value;

fn fixture(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("advanced")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn keyframes_match_the_fixture() {
    let fx = fixture("keyframes.json");
    let keys: Vec<Keyframe> = serde_json::from_value(fx["keys"].clone()).unwrap();
    for pair in fx["expect"].as_array().unwrap() {
        let t = pair[0].as_u64().unwrap();
        let want = pair[1].as_f64().unwrap() as f32;
        let got = value_at(&keys, t).unwrap();
        assert!((got - want).abs() < 1e-5, "t={t}: {got} vs {want}");
    }
    let tkeys: Vec<TransformKey> = serde_json::from_value(fx["transform_keys"].clone()).unwrap();
    for pair in fx["transform_expect"].as_array().unwrap() {
        let t = pair[0].as_u64().unwrap();
        let w = pair[1].as_array().unwrap();
        let want = Transform {
            zoom: w[0].as_f64().unwrap() as f32,
            x: w[1].as_f64().unwrap() as f32,
            y: w[2].as_f64().unwrap() as f32,
        };
        let got = transform_at(&tkeys, t).unwrap();
        assert!(
            (got.zoom - want.zoom).abs() < 1e-5
                && (got.x - want.x).abs() < 1e-5
                && (got.y - want.y).abs() < 1e-5,
            "t={t}: {got:?} vs {want:?}"
        );
    }
}

#[test]
fn ramps_match_the_fixture() {
    let fx = fixture("ramp.json");
    let points: Vec<SpeedPoint> = serde_json::from_value(fx["points"].clone()).unwrap();
    for pair in fx["source_offset"].as_array().unwrap() {
        let (out, want) = (pair[0].as_u64().unwrap(), pair[1].as_u64().unwrap());
        assert_eq!(source_offset(&points, out), want, "out={out}");
    }
    for pair in fx["output_length"].as_array().unwrap() {
        let (src, want) = (pair[0].as_u64().unwrap(), pair[1].as_u64().unwrap());
        assert_eq!(output_length(&points, src), want, "src={src}");
    }
}

/// Switching modes never loses work: a Normal-mode save (`Project::from_value` →
/// `to_value`, what the engine does) keeps the `advanced` section byte for byte, an
/// empty one is not written, and the badge follows.
#[test]
fn advanced_survives_a_normal_mode_round_trip() {
    let v: Value = serde_json::json!({
        "version": 1, "source_ms": 10_000,
        "pieces": [{"start_ms": 1000, "end_ms": 5000, "speed": 2}],
        "advanced": {
            "tracks": [{"id": 1, "kind": "music", "name": "Music", "clips": [
                {"id": 1, "source": {"kind": "file", "path": "C:/music/beat.mp3"}, "in_ms": 0, "out_ms": 30000, "at_ms": 0,
                 "volume": [{"at_ms": 0, "value": 0.8, "ease": "linear"}]}
            ]}],
            "colour": {"brightness": 0.1, "contrast": 1.0, "saturation": 1.2},
            "markers": [{"at_ms": 2000, "title": "drop", "kind": "user"}]
        }
    });
    let (p, issues) = klip_edit::Project::from_value(v.clone());
    assert!(issues.is_empty(), "{issues:?}");
    assert!(p.has_advanced_edits());
    let out = p.to_value().unwrap();
    // Written back with the defaults filled in and f32 precision; the meaning is the same.
    let (again, _) = klip_edit::Project::from_value(out.clone());
    assert_eq!(again.advanced, p.advanced);
    assert_eq!(
        out["advanced"]["tracks"][0]["clips"][0]["source"]["path"],
        "C:/music/beat.mp3"
    );
    assert_eq!(out["advanced"]["markers"][0]["title"], "drop");
    assert!((out["advanced"]["colour"]["saturation"].as_f64().unwrap() - 1.2).abs() < 1e-6);
    assert_eq!(again.pieces, p.pieces);
    // A Normal-mode edit on top (a cut) leaves the section alone.
    let mut p2 = p.clone();
    klip_edit::timeline::cut(&mut p2, 0, 500).unwrap();
    assert_eq!(p2.advanced, p.advanced);
    // Nothing advanced: nothing written, no badge.
    let (plain, _) = klip_edit::Project::from_value(serde_json::json!({"source_ms": 5}));
    assert!(!plain.has_advanced_edits());
    assert!(plain.to_value().unwrap().get("advanced").is_none());
}
