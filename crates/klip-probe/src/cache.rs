//! Remembered probe results, so the recorder doesn't run a dummy encode on every start.
//!
//! The cache is valid for one [`fingerprint`]: GStreamer version, the hardware adapters
//! (PCI ids and driver version) and the allowlisted encoder elements that are installed.
//! A driver update, a new GPU or a GStreamer update changes it and everything is probed
//! again. The file is untrusted input: it is size-capped, parsed strictly, and ignored
//! when anything is off.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ProbeResult;

/// Format version of the file. 2 (2026-10-12): drops every cache written while the
/// installer lacked the probe's test source (all encoders "failed" on the AMD tester).
const VERSION: u32 = 2;

/// Larger files are not read (a real one is a few kB).
const MAX_FILE_BYTES: u64 = 256 * 1024;

/// Entries kept at most (one per installed encoder element).
const MAX_ENTRIES: usize = 256;

/// An encoder that failed to start this many times in a row (after passing its probe)
/// is left out, like one that fails the probe.
pub const RUNTIME_FAILURE_LIMIT: u32 = 2;

/// What is known about one encoder element.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedResult {
    pub element: String,
    /// Passed the dummy encode.
    pub ok: bool,
    /// Probe error (if it failed), shortened.
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub probe_ms: u64,
    /// Failed starts of a real recording since it last worked.
    #[serde(default)]
    pub runtime_failures: u32,
}

impl CachedResult {
    /// Worth trying.
    pub fn usable(&self) -> bool {
        self.ok && self.runtime_failures < RUNTIME_FAILURE_LIMIT
    }
}

/// Probe results for one machine configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeCache {
    pub version: u32,
    pub fingerprint: String,
    pub results: Vec<CachedResult>,
}

fn short(s: &str) -> String {
    s.chars().take(300).collect()
}

impl ProbeCache {
    /// An empty cache for `fingerprint`.
    pub fn new(fingerprint: &str) -> ProbeCache {
        ProbeCache {
            version: VERSION,
            fingerprint: fingerprint.to_string(),
            results: Vec::new(),
        }
    }

    /// Load the cache at `path` if it exists, is valid and matches `fingerprint`;
    /// otherwise an empty cache for `fingerprint`. Never fails.
    pub fn load(path: &Path, fingerprint: &str) -> ProbeCache {
        match Self::read(path) {
            // Probes that all failed say more about the setup at the time (a broken
            // install) than about the GPUs: probe again rather than never record.
            Ok(c) if !c.results.is_empty() && c.results.iter().all(|r| !r.ok) => {
                tracing::info!("every cached encoder failed; probing again");
                Self::new(fingerprint)
            }
            Ok(c) if c.version == VERSION && c.fingerprint == fingerprint => c,
            Ok(_) => {
                tracing::info!("encoder cache is for another setup; probing again");
                Self::new(fingerprint)
            }
            Err(e) => {
                if path.exists() {
                    tracing::warn!(error = %e, "encoder cache unreadable; probing again");
                }
                Self::new(fingerprint)
            }
        }
    }

    fn read(path: &Path) -> Result<ProbeCache, String> {
        let file = File::open(path).map_err(|e| e.to_string())?;
        let mut text = String::new();
        file.take(MAX_FILE_BYTES.saturating_add(1))
            .read_to_string(&mut text)
            .map_err(|e| e.to_string())?;
        if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_FILE_BYTES {
            return Err("file too large".into());
        }
        let mut c: ProbeCache = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        c.results.truncate(MAX_ENTRIES);
        for r in &mut c.results {
            r.element = short(&r.element);
            r.error = r.error.as_deref().map(short);
        }
        Ok(c)
    }

    /// Write atomically (temp file in the same folder, fsync, rename over the old one).
    /// Creates the folder if needed.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        let mut tmp_name = path.as_os_str().to_owned();
        tmp_name.push(".tmp");
        let tmp = PathBuf::from(tmp_name);
        let result = (|| {
            let mut f = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
            drop(f);
            fs::rename(&tmp, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }

    pub fn get(&self, element: &str) -> Option<&CachedResult> {
        self.results.iter().find(|r| r.element == element)
    }

    fn entry(&mut self, element: &str) -> Option<&mut CachedResult> {
        let pos = match self.results.iter().position(|r| r.element == element) {
            Some(p) => p,
            None => {
                if self.results.len() >= MAX_ENTRIES {
                    return None;
                }
                self.results.push(CachedResult {
                    element: short(element),
                    ok: false,
                    error: None,
                    probe_ms: 0,
                    runtime_failures: 0,
                });
                self.results.len().checked_sub(1)?
            }
        };
        self.results.get_mut(pos)
    }

    /// Remember a probe outcome.
    pub fn record(&mut self, r: &ProbeResult) {
        if let Some(e) = self.entry(&r.choice.element) {
            e.ok = r.ok;
            e.error = r.error.as_deref().map(short);
            e.probe_ms = u64::try_from(r.elapsed.as_millis()).unwrap_or(u64::MAX);
            e.runtime_failures = 0;
        }
    }

    /// A recording started with `element` and produced video.
    pub fn record_started(&mut self, element: &str) {
        if let Some(e) = self.entry(element) {
            e.ok = true;
            e.error = None;
            e.runtime_failures = 0;
        }
    }

    /// `element` failed to start a recording (after passing its probe, or untested).
    pub fn record_start_failure(&mut self, element: &str, error: &str) {
        if let Some(e) = self.entry(element) {
            if e.probe_ms == 0 && !e.ok {
                // Never probed: a failed start counts like a failed probe.
                e.runtime_failures = RUNTIME_FAILURE_LIMIT;
            } else {
                e.runtime_failures = e.runtime_failures.saturating_add(1);
            }
            e.error = Some(short(error));
        }
    }
}

/// Identifies the setup the probe results are valid for. `adapters`: `(PCI vendor id,
/// PCI device id, driver version)` of each hardware adapter; `elements`: installed,
/// allowlisted encoder element names.
pub fn fingerprint(adapters: &[(u32, u32, Option<u64>)], elements: &[String]) -> String {
    let (major, minor, micro, nano) = gstreamer::version();
    let mut gpus: Vec<String> = adapters
        .iter()
        .map(|(v, d, drv)| match drv {
            Some(drv) => format!("{v:04x}:{d:04x}@{drv:x}"),
            None => format!("{v:04x}:{d:04x}"),
        })
        .collect();
    gpus.sort();
    let mut els: Vec<&str> = elements.iter().map(String::as_str).collect();
    els.sort_unstable();
    els.dedup();
    format!(
        "gst {major}.{minor}.{micro}.{nano}; gpus {}; encoders {}",
        gpus.join(","),
        els.join(",")
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use klip_media::Codec;

    use super::*;
    use crate::{EncoderChoice, EncoderFamily, EncoderVendor};

    fn result(element: &str, ok: bool) -> ProbeResult {
        ProbeResult {
            choice: EncoderChoice {
                element: element.into(),
                codec: Codec::H264,
                family: EncoderFamily::NvencD3d11,
                vendor: EncoderVendor::Nvidia,
                description: String::new(),
                rank: 0,
                gpu: None,
                device_index: 0,
            },
            ok,
            error: (!ok).then(|| "no device".into()),
            elapsed: Duration::from_millis(120),
        }
    }

    #[test]
    fn round_trip_and_fingerprint_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("encoders.json");
        let mut c = ProbeCache::new("fp1");
        c.record(&result("nvd3d11h264enc", true));
        c.record(&result("amfh264enc", false));
        c.save(&path).unwrap();
        assert!(!dir.path().join("sub").join("encoders.json.tmp").exists());
        let back = ProbeCache::load(&path, "fp1");
        assert_eq!(back, c);
        assert!(back.get("nvd3d11h264enc").unwrap().usable());
        assert!(!back.get("amfh264enc").unwrap().usable());
        // Another setup: start over.
        assert!(ProbeCache::load(&path, "fp2").results.is_empty());
        // Saving again replaces the file.
        c.record(&result("amfh264enc", true));
        c.save(&path).unwrap();
        assert!(ProbeCache::load(&path, "fp1").get("amfh264enc").unwrap().ok);
    }

    #[test]
    fn hostile_files_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("encoders.json");
        for bad in [
            "".to_string(),
            "{".to_string(),
            "[1,2,3]".to_string(),
            r#"{"version":1,"fingerprint":"fp","results":[{"element":5}]}"#.to_string(),
            format!(
                r#"{{"version":2,"fingerprint":"fp","results":[],"pad":"{}"}}"#,
                "x".repeat(300_000)
            ),
        ] {
            fs::write(&path, bad).unwrap();
            let c = ProbeCache::load(&path, "fp");
            assert!(c.results.is_empty());
            assert_eq!(c.fingerprint, "fp");
        }
        // Missing file.
        assert!(
            ProbeCache::load(&dir.path().join("none.json"), "fp")
                .results
                .is_empty()
        );
        // Long strings are cut, many entries capped.
        let many: Vec<String> = (0..300)
            .map(|i| format!(r#"{{"element":"e{i}{}","ok":true}}"#, "y".repeat(400)))
            .collect();
        fs::write(
            &path,
            format!(
                r#"{{"version":2,"fingerprint":"fp","results":[{}]}}"#,
                many.join(",")
            ),
        )
        .unwrap();
        let c = ProbeCache::load(&path, "fp");
        assert_eq!(c.results.len(), MAX_ENTRIES);
        assert!(c.results.iter().all(|r| r.element.chars().count() <= 300));
    }

    #[test]
    fn start_failures_retire_an_encoder() {
        let mut c = ProbeCache::new("fp");
        c.record(&result("a", true));
        c.record_start_failure("a", "could not start");
        assert!(c.get("a").unwrap().usable(), "one failure is forgiven");
        c.record_start_failure("a", "could not start");
        assert!(!c.get("a").unwrap().usable());
        c.record_started("a");
        assert!(c.get("a").unwrap().usable());
        // An untested encoder that fails to start is out at once.
        c.record_start_failure("b", "no");
        assert!(!c.get("b").unwrap().usable());
    }

    #[test]
    fn fingerprint_is_order_independent() {
        let a = fingerprint(
            &[(0x10de, 0x2702, Some(5)), (0x8086, 0x1, None)],
            &["b".into(), "a".into(), "a".into()],
        );
        let b = fingerprint(
            &[(0x8086, 0x1, None), (0x10de, 0x2702, Some(5))],
            &["a".into(), "b".into()],
        );
        assert_eq!(a, b);
        assert_ne!(
            a,
            fingerprint(&[(0x10de, 0x2702, Some(6))], &["a".into(), "b".into()])
        );
    }

    #[test]
    fn a_cache_where_everything_failed_is_probed_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("encoders.json");
        let mut c = ProbeCache::new("fp");
        c.record(&result("amfh264enc", false));
        c.record(&result("mfh264enc", false));
        c.save(&path).unwrap();
        assert!(ProbeCache::load(&path, "fp").results.is_empty());
        c.record(&result("mfh264enc", true));
        c.save(&path).unwrap();
        assert_eq!(ProbeCache::load(&path, "fp").results.len(), 2);
    }
}
