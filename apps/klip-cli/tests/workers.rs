//! M6.12 (ADR 0016): the recorder process (`--klip-recorder`) treats what it is given as
//! hostile: bad input ends it with an error and nothing on stdout, never a hang or a
//! panic. No GPU needed.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::io::Write;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Run a worker with `input` on stdin; fails the test if it takes over 60 s.
fn run_worker(args: &[&str], input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_klip-cli"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let data = input.to_vec();
    let writer = std::thread::spawn(move || {
        // The worker may stop reading early (that's the point); ignore broken pipes.
        let _ = stdin.write_all(&data);
    });
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(60) {
            let _ = child.kill();
            panic!("worker {args:?} hung");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = writer.join();
    child.wait_with_output().unwrap()
}

#[test]
fn recorder_worker_rejects_bad_start_requests() {
    let long = vec![b'x'; 3 * 1024 * 1024];
    for input in [
        &b""[..],
        b"not json\n",
        b"{\"settings\":{},\"game\":null}\n",
        b"{\"settings\":{},\"game\":null,\"encoders\":[],\"extra\":1}\n",
        b"{\"settings\":{},\"game\":{\"pid\":\"x\"},\"encoders\":[]}\n",
        &long,
    ] {
        let out = run_worker(&["--klip-recorder"], input);
        assert_eq!(
            out.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.stdout.is_empty(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

#[test]
fn recorder_worker_uses_only_allowed_installed_encoders() {
    // A GPL encoder and made-up names are dropped; with nothing left it says so and
    // exits without recording anything.
    let req = json!({
        "settings": {"clips": {"folder": std::env::temp_dir().join("klip-never-created")}},
        "game": null,
        "encoders": ["x264enc", "../../evil", "nvh264enc\n"],
    });
    let out = run_worker(&["--klip-recorder"], format!("{req}\n").as_bytes());
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let line: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(line["start"]["ok"], false, "{line}");
    assert_eq!(line["start"]["error"], "no encoder", "{line}");
}
