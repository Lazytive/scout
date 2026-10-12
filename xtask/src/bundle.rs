//! `cargo xtask bundle` (Windows; the start of M13): what Scout needs of GStreamer,
//! copied into `dist/gstreamer/` for the installer, which puts it beside `Scout.exe`.
//!
//! - `gstreamer-1.0/`: the plugins in [`PLUGINS`] only (no GPL plugins, no `libav` =
//!   no FFmpeg, no `openh264` = no software H.264 encoder: AGENTS rules 5 and 6);
//! - the DLLs those plugins, `gst-plugin-scanner.exe` and the app import, followed
//!   through their import tables (`dumpbin /dependents`), from GStreamer's `bin`, plus
//!   the MSVC runtime from the Build Tools' redistributable folder;
//! - `licenses/`: GStreamer's licence texts and `versions.txt` (LGPL: the DLLs are
//!   dynamically linked and replaceable).
//!
//! The app points GStreamer at this copy when it finds `gstreamer-1.0/` beside its exe
//! (`klip_platform::use_bundled_gstreamer`).

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Plugins product code names elements of but that are left out on purpose: GPL ones
/// and FFmpeg (named on the denylists), Cisco's openh264 and OpenGL (Linux paths only).
const NOT_BUNDLED: &[&str] = &[
    "x264", "x265", "a52dec", "dtsdec", "libav", "openh264", "opengl",
];

/// Where the bundle is staged (relative to the repo).
pub const OUT: &str = "dist/gstreamer";
/// The plugin folder inside it (the audit checks it).
pub const PLUGIN_DIR: &str = "gstreamer-1.0";

/// Plugins Scout uses, by feature. GIF/BMP pictures (gdk-pixbuf with its own loader
/// files) are left out.
pub const PLUGINS: &[&str] = &[
    // Core: queues, app sources/sinks, type finding, decodebin, conversion, mixing.
    "coreelements",
    "app",
    "typefindfunctions",
    "playback",
    "videoconvertscale",
    "videorate",
    "videocrop",
    "audioconvert",
    "audioresample",
    "audiorate",
    "audiomixer",
    "audiofx",
    "volume",
    "audiotestsrc",
    // Containers and parsers: clips (MP4), music files.
    "isomp4",
    "matroska",
    "ogg",
    "wavparse",
    "audioparsers",
    "videoparsersbad",
    "opusparse",
    // Music decoders (BSD/LGPL libraries).
    "opus",
    "vorbis",
    "flac",
    "mpg123",
    // Pictures: thumbnails out, stickers in.
    "jpeg",
    "png",
    // Windows capture, GPU and OS encoders/decoders, audio.
    "d3d11",
    "d3d12",
    "nvcodec",
    "mediafoundation",
    "amfcodec",
    "qsv",
    "wasapi2",
    // AV1 software decoder (BSD).
    "dav1d",
];

fn gst_root() -> PathBuf {
    std::env::var_os("GSTREAMER_1_0_ROOT_MSVC_X86_64")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files\gstreamer\1.0\msvc_x86_64"))
}

/// The newest folder matching `base/*/rest`.
fn newest(base: &Path, rest: &str) -> Result<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(base)
        .with_context(|| format!("reading {}", base.display()))?
        .flatten()
        .map(|e| e.path().join(rest))
        .filter(|p| p.exists())
        .collect();
    found.sort();
    found
        .pop()
        .with_context(|| format!("no {rest} under {}", base.display()))
}

fn vs_root() -> PathBuf {
    PathBuf::from(r"C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC")
}

fn dumpbin() -> Result<PathBuf> {
    newest(
        &vs_root().join("Tools").join("MSVC"),
        r"bin\Hostx64\x64\dumpbin.exe",
    )
}

fn vc_crt() -> Result<PathBuf> {
    newest(
        &vs_root().join("Redist").join("MSVC"),
        r"x64\Microsoft.VC143.CRT",
    )
}

/// The DLLs `file` imports, normal and delay-loaded.
fn dependents(dumpbin: &Path, file: &Path) -> Result<Vec<String>> {
    let out = Command::new(dumpbin)
        .args(["/nologo", "/dependents"])
        .arg(file)
        .output()
        .with_context(|| format!("running dumpbin on {}", file.display()))?;
    if !out.status.success() {
        bail!("dumpbin failed on {}", file.display());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.contains(' ') && l.to_ascii_lowercase().ends_with(".dll"))
        .map(str::to_string)
        .collect())
}

fn copy_into(src: &Path, dir: &Path) -> Result<u64> {
    let name = src.file_name().context("no file name")?;
    std::fs::copy(src, dir.join(name)).with_context(|| format!("copying {}", src.display()))
}

fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let to = dst.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_tree(&e.path(), &to)?;
        } else {
            std::fs::copy(e.path(), to)?;
        }
    }
    Ok(())
}

/// Product source (test modules cut off) of the crates and the app.
fn product_source(root: &Path) -> Result<String> {
    let mut text = String::new();
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root.join("crates"))?
        .flatten()
        .map(|e| e.path().join("src"))
        .collect();
    dirs.push(
        root.join("apps")
            .join("klip-app")
            .join("src-tauri")
            .join("src"),
    );
    while let Some(d) = dirs.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                dirs.push(p);
            } else if p.extension().is_some_and(|x| x == "rs")
                && !p.to_string_lossy().ends_with("tests.rs")
            {
                let src = std::fs::read_to_string(&p)?;
                let body = src.split("#[cfg(test)]").next().unwrap_or("");
                text.push_str(body);
            }
        }
    }
    Ok(text)
}

/// Every element the product code names (as a string literal) must come from a bundled
/// plugin or one of [`NOT_BUNDLED`]; `gst-inspect-1.0` of the full install maps names to
/// plugins. (The first bundle missed `videocrop` and `scaletempo`: exports failed.)
fn check_elements(root: &Path, gst: &Path) -> Result<()> {
    let out = Command::new(gst.join("bin").join("gst-inspect-1.0.exe"))
        .output()
        .context("running gst-inspect-1.0")?;
    let source = product_source(root)?;
    let mut missing: BTreeSet<String> = BTreeSet::new();
    let mut checked: usize = 0;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut parts = line.splitn(3, ':').map(str::trim);
        let (Some(plugin), Some(element)) = (parts.next(), parts.next()) else {
            continue;
        };
        if plugin.is_empty() || element.is_empty() || element.contains(' ') {
            continue;
        }
        if !source.contains(&format!("\"{element}\"")) {
            continue;
        }
        checked = checked.saturating_add(1);
        if !PLUGINS.contains(&plugin) && !NOT_BUNDLED.contains(&plugin) {
            missing.insert(format!("{element} ({plugin})"));
        }
    }
    if !missing.is_empty() {
        bail!(
            "the product uses elements from plugins the bundle leaves out: {}",
            missing.into_iter().collect::<Vec<_>>().join(", ")
        );
    }
    println!("bundle: {checked} elements named in the product code all come with the bundle");
    Ok(())
}

/// Stage the bundle for `app_exe` (the release `klip-app.exe`).
pub fn run(root: &Path, app_exe: &Path) -> Result<()> {
    let gst = gst_root();
    let bin = gst.join("bin");
    let plugins = gst.join("lib").join("gstreamer-1.0");
    let (dumpbin, crt) = (dumpbin()?, vc_crt()?);
    check_elements(root, &gst)?;
    let out = root.join(OUT);
    if out.exists() {
        std::fs::remove_dir_all(&out).with_context(|| format!("clearing {}", out.display()))?;
    }
    let plugin_out = out.join(PLUGIN_DIR);
    std::fs::create_dir_all(&plugin_out)?;

    let mut queue: VecDeque<PathBuf> = VecDeque::new();
    let mut bytes: u64 = 0;
    for p in PLUGINS {
        let f = plugins.join(format!("gst{p}.dll"));
        if !f.is_file() {
            bail!("plugin {p} is not in {}", plugins.display());
        }
        bytes = bytes.saturating_add(copy_into(&f, &plugin_out)?);
        queue.push_back(f);
    }
    let scanner = gst
        .join("libexec")
        .join("gstreamer-1.0")
        .join("gst-plugin-scanner.exe");
    bytes = bytes.saturating_add(copy_into(&scanner, &out)?);
    queue.push_back(scanner);
    queue.push_back(app_exe.to_path_buf());

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut system: BTreeSet<String> = BTreeSet::new();
    let mut dlls: usize = 0;
    while let Some(file) = queue.pop_front() {
        for dep in dependents(&dumpbin, &file)? {
            if !seen.insert(dep.to_ascii_lowercase()) {
                continue;
            }
            let from_gst = bin.join(&dep);
            let from_crt = crt.join(&dep);
            if from_gst.is_file() {
                bytes = bytes.saturating_add(copy_into(&from_gst, &out)?);
                dlls = dlls.saturating_add(1);
                queue.push_back(from_gst);
            } else if from_crt.is_file() {
                bytes = bytes.saturating_add(copy_into(&from_crt, &out)?);
                dlls = dlls.saturating_add(1);
                queue.push_back(from_crt);
            } else {
                system.insert(dep);
            }
        }
    }

    let licenses = out.join("licenses");
    copy_tree(
        &gst.join("share").join("licenses"),
        &licenses.join("gstreamer"),
    )?;
    std::fs::copy(
        gst.join("share").join("versions.txt"),
        licenses.join("gstreamer").join("versions.txt"),
    )?;
    std::fs::write(
        licenses.join("README.txt"),
        "Scout uses GStreamer (https://gstreamer.freedesktop.org), LGPL-2.1-or-later, \
         dynamically linked: the GStreamer DLLs beside Scout.exe and in gstreamer-1.0\\ \
         can be replaced with your own builds. gstreamer\\ holds the licences of GStreamer \
         and the libraries it ships, and versions.txt (source: the GStreamer project's \
         releases of those versions). The Microsoft Visual C++ runtime DLLs are \
         redistributed under the Visual Studio licence.\r\n",
    )?;

    println!(
        "bundle: {} plugins + {dlls} DLLs + the scanner in {OUT} ({} MB)",
        PLUGINS.len(),
        bytes.checked_div(1_000_000).unwrap_or(0)
    );
    println!(
        "bundle: left to Windows: {}",
        system.into_iter().collect::<Vec<_>>().join(" ")
    );
    Ok(())
}
