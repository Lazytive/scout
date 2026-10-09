//! The timeline fixtures shared with the front end (`fixtures/*.json`): every file is
//! a project plus what the timeline math must say about it. The TypeScript port is
//! checked against the same files by vitest.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::Path;

use klip_edit::{Project, Segment, timeline};
use serde_json::Value;

fn fixtures() -> Vec<(String, Value)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut out: Vec<(String, Value)> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .map(|p| {
            let text = std::fs::read_to_string(&p).unwrap();
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                serde_json::from_str(&text).unwrap(),
            )
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(out.len() >= 3, "fixtures missing");
    out
}

fn ms(v: &Value) -> Option<u64> {
    v.as_u64()
}

#[test]
fn every_fixture_matches_the_timeline_math() {
    for (file, fx) in fixtures() {
        let (project, issues) = Project::from_value(fx["project"].clone());
        assert!(
            issues.is_empty(),
            "{file}: fixture project must be clean: {issues:?}"
        );
        let expect = &fx["expect"];
        assert_eq!(
            timeline::output_duration_ms(&project),
            expect["output_duration_ms"].as_u64().unwrap(),
            "{file}: duration"
        );
        let expected: Vec<Segment> = serde_json::from_value(expect["segments"].clone()).unwrap();
        assert_eq!(timeline::segments(&project), expected, "{file}: segments");
        for pair in expect["source_time"].as_array().unwrap() {
            let out = pair[0].as_u64().unwrap();
            assert_eq!(
                timeline::source_time(&project, out),
                ms(&pair[1]),
                "{file}: source_time({out})"
            );
        }
        for pair in expect["output_time"].as_array().unwrap() {
            let src = pair[0].as_u64().unwrap();
            assert_eq!(
                timeline::output_time(&project, src),
                ms(&pair[1]),
                "{file}: output_time({src})"
            );
        }
    }
}

#[test]
fn fixture_projects_round_trip_through_json() {
    for (file, fx) in fixtures() {
        let (project, _) = Project::from_value(fx["project"].clone());
        let (again, issues) = Project::from_value(project.to_value().unwrap());
        assert!(issues.is_empty(), "{file}: {issues:?}");
        assert_eq!(again, project, "{file}");
    }
}
