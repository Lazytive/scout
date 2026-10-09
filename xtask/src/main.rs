//! `cargo xtask <task>`: Klip's quality gates.
//!
//! - `ci`: front end (eslint, svelte-check, vitest, build), fmt, clippy `-D warnings`,
//!   tests, cargo deny, layers, audit
//! - `layers`: dependency direction + `unsafe` only in klip-platform
//! - `audit`: licence/asset audit
//! - `e2e`: hardware acceptance tests (`#[ignore]`d tests except `diag_*` diagnostics,
//!   release build, one at a time)
//!
//! Dev tool, so `arithmetic_side_effects` is allowed (ADR 0001).

#![allow(clippy::arithmetic_side_effects)]

mod audit;
mod layers;
mod npm;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Instant;

use anyhow::{Context, Result, bail};

fn root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().map(Path::to_path_buf).unwrap_or(manifest)
}

/// Make GStreamer's tools and pkg-config files visible to child processes when the
/// official MSVC installer's environment variable is set but this shell predates it.
fn setup_env() {
    if !cfg!(windows) {
        return;
    }
    let Some(gst) = std::env::var_os("GSTREAMER_1_0_ROOT_MSVC_X86_64") else {
        return;
    };
    let gst = PathBuf::from(gst);
    let bin = gst.join("bin");
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut parts: Vec<PathBuf> = std::env::split_paths(&path).collect();
    if !parts.iter().any(|p| p == &bin) {
        parts.insert(0, bin);
        if let Ok(joined) = std::env::join_paths(parts) {
            set_env("PATH", &joined);
        }
    }
    if std::env::var_os("PKG_CONFIG_PATH").is_none() {
        set_env(
            "PKG_CONFIG_PATH",
            gst.join("lib").join("pkgconfig").as_os_str(),
        );
    }
}

/// Record a variable for child processes (`std::env::set_var` is an unsafe fn in
/// edition 2024, so it's passed to each child explicitly instead).
fn set_env(key: &str, value: &std::ffi::OsStr) {
    ENV.with(|e| e.borrow_mut().push((key.to_string(), value.to_os_string())));
}

thread_local! {
    static ENV: std::cell::RefCell<Vec<(String, std::ffi::OsString)>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn command(program: &str) -> Command {
    let mut c = Command::new(program);
    c.current_dir(root());
    ENV.with(|e| {
        for (k, v) in e.borrow().iter() {
            c.env(k, v);
        }
    });
    c
}

fn cargo() -> Command {
    command(&std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
}

fn run_step(name: &str, mut cmd: Command) -> Result<()> {
    let t = Instant::now();
    println!("==> {name}");
    let status = cmd
        .status()
        .with_context(|| format!("could not run {name}"))?;
    if !status.success() {
        bail!("{name} failed ({status})");
    }
    println!("    {name} ok ({:.1} s)", t.elapsed().as_secs_f64());
    Ok(())
}

/// All `.rs` files below `dir` (skipping `target`).
pub fn rust_files(dir: &Path) -> Result<Vec<PathBuf>> {
    Ok(all_files(dir)?
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "rs"))
        .collect())
}

/// All files below `dir` (skipping `target` and `node_modules`).
pub fn all_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d)? {
            let e = e?;
            let p = e.path();
            let name = e.file_name();
            if e.file_type()?.is_dir() {
                if name != "target" && name != "node_modules" {
                    stack.push(p);
                }
            } else {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

fn layers() -> Result<()> {
    println!("==> layers");
    let out = cargo()
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .output()
        .context("cargo metadata")?;
    if !out.status.success() {
        bail!("cargo metadata failed");
    }
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let (names, edges) = layers::edges_from_metadata(&meta)?;
    let mut errs = layers::check_edges(&names, &edges);
    errs.extend(layers::scan_unsafe(&root())?);
    if errs.is_empty() {
        println!(
            "    layers ok ({} crates, {} edges)",
            names.len(),
            edges.len()
        );
        Ok(())
    } else {
        for e in &errs {
            eprintln!("layers: {e}");
        }
        bail!("layer check failed ({} problems)", errs.len())
    }
}

fn ci() -> Result<()> {
    let t = Instant::now();
    if npm::exists(&root()) {
        npm::gates(&root())?;
    }
    let mut c = cargo();
    c.args(["fmt", "--all", "--", "--check"]);
    run_step("fmt", c)?;
    let mut c = cargo();
    c.args([
        "clippy",
        "--workspace",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ]);
    run_step("clippy", c)?;
    let mut c = cargo();
    c.args(["test", "--workspace"]);
    run_step("test", c)?;
    let mut c = cargo();
    c.args(["deny", "--log-level", "error", "check"]);
    run_step("cargo deny", c)
        .context("is cargo-deny installed? `cargo install cargo-deny --locked`")?;
    layers()?;
    println!("==> audit");
    audit::run(&root())?;
    println!("ci: all gates green ({:.0} s)", t.elapsed().as_secs_f64());
    Ok(())
}

fn e2e() -> Result<()> {
    let mut c = cargo();
    c.args([
        "build",
        "--release",
        "-p",
        "klip-testgame",
        "-p",
        "klip-cli",
    ]);
    run_step("build test game + cli", c)?;
    let mut c = cargo();
    c.args([
        "test",
        "--release",
        "--workspace",
        // Report every failing test binary, not just the first.
        "--no-fail-fast",
        "--",
        "--ignored",
        "--nocapture",
        "--test-threads=1",
        // Diagnostics print measurements and may need manual setup; run them by name.
        "--skip",
        "diag_",
    ]);
    run_step("hardware acceptance tests", c)
}

fn run() -> Result<()> {
    setup_env();
    let task = std::env::args().nth(1).unwrap_or_default();
    match task.as_str() {
        "ci" => ci(),
        "layers" => layers(),
        "audit" => audit::run(&root()),
        "e2e" => e2e(),
        _ => {
            eprintln!("usage: cargo xtask <ci|layers|audit|e2e>");
            bail!("unknown task {task:?}")
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e:#}");
            ExitCode::FAILURE
        }
    }
}
