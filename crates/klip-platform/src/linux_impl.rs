//! Linux: GPUs from sysfs, processes from `/proc`, the monotonic clock.
//!
//! Everything read here comes from the kernel's pseudo files, which are treated like any
//! other input: sizes capped, parse failures skipped rather than trusted.

use std::fs;
use std::path::Path;

use crate::{GpuAdapter, GpuVendor, PlatformError, ProcessInfo};

/// Pseudo files read here are tiny; anything bigger is not what we expect.
const MAX_SMALL_FILE: u64 = 64 * 1024;
/// `pci.ids` is about 1.5 MB; refuse anything absurd.
const MAX_PCI_IDS: u64 = 16 * 1024 * 1024;

fn read_small(path: &Path, cap: u64) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    // Pseudo files report size 0; regular files must fit the cap.
    if meta.len() > cap {
        return None;
    }
    let text = fs::read_to_string(path).ok()?;
    (u64::try_from(text.len()).ok()? <= cap).then_some(text)
}

fn read_hex(path: &Path) -> Option<u32> {
    let s = read_small(path, MAX_SMALL_FILE)?;
    u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok()
}

/// Bytes available to unprivileged users on the file system holding `path`.
pub fn free_disk_space(path: &Path) -> Result<u64, PlatformError> {
    let st = rustix::fs::statvfs(path).map_err(|e| PlatformError::Os(format!("statvfs: {e}")))?;
    Ok(st.f_bavail.saturating_mul(st.f_frsize))
}

/// CLOCK_MONOTONIC in ns: the clock of GStreamer's system clock (its default type) and of
/// PipeWire's stream times, so frames, audio and pipelines share one timeline.
pub fn monotonic_ns() -> i64 {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    t.tv_sec
        .saturating_mul(1_000_000_000)
        .saturating_add(t.tv_nsec)
}

/// `0000:01:00.0` → a stable id (domain, bus, device, function packed), standing in for
/// the DXGI LUID.
fn pci_slot_id(slot: &str) -> Option<i64> {
    let (domain, rest) = slot.split_once(':')?;
    let (bus, rest) = rest.split_once(':')?;
    let (dev, func) = rest.split_once('.')?;
    let domain = i64::from_str_radix(domain, 16).ok()?;
    let bus = i64::from_str_radix(bus, 16).ok()?;
    let dev = i64::from_str_radix(dev, 16).ok()?;
    let func = i64::from_str_radix(func, 16).ok()?;
    if !(0..=0xFFFF).contains(&domain)
        || !(0..=0xFF).contains(&bus)
        || !(0..=0x1F).contains(&dev)
        || !(0..=7).contains(&func)
    {
        return None;
    }
    Some((domain << 16) | (bus << 8) | (dev << 3) | func)
}

/// `615.71.09` → `615 << 32 | 71 << 16 | 9`, ordered like the version.
fn pack_version(v: &str) -> Option<u64> {
    let mut parts = v.trim().split('.').map(|p| p.parse::<u64>().ok());
    let major = parts.next()??;
    let minor = parts.next().flatten().unwrap_or(0);
    let patch = parts.next().flatten().unwrap_or(0);
    if major > 0xFFFF_FFFF || minor > 0xFFFF || patch > 0xFFFF {
        return None;
    }
    Some((major << 32) | (minor << 16) | patch)
}

/// Device name from `pci.ids` (hwdata), e.g. "AD103 [GeForce RTX 4080 SUPER]".
fn pci_ids_name(ids: &str, vendor: u32, device: u32) -> Option<String> {
    let v = format!("{vendor:04x}");
    let d = format!("{device:04x}");
    let mut in_vendor = false;
    for line in ids.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if !line.starts_with('\t') {
            if in_vendor {
                return None;
            }
            in_vendor = line.get(..4).is_some_and(|x| x.eq_ignore_ascii_case(&v));
            continue;
        }
        if in_vendor
            && !line.starts_with("\t\t")
            && let Some(rest) = line.strip_prefix('\t')
            && rest.get(..4).is_some_and(|x| x.eq_ignore_ascii_case(&d))
        {
            return rest.get(4..).map(|n| n.trim().to_string());
        }
    }
    None
}

/// NVIDIA's model name from `/proc/driver/nvidia/gpus/<slot>/information`.
fn nvidia_model(slot: &str) -> Option<String> {
    let info = read_small(
        &Path::new("/proc/driver/nvidia/gpus")
            .join(slot)
            .join("information"),
        MAX_SMALL_FILE,
    )?;
    info.lines()
        .find_map(|l| l.strip_prefix("Model:"))
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
}

/// Kernel drivers that only scan out a firmware framebuffer or emulate a GPU.
fn software_driver(driver: &str) -> bool {
    matches!(driver, "simpledrm" | "simplefb" | "efifb" | "vkms" | "vgem")
}

/// GPUs: one entry per DRM card with a PCI device (`/sys/class/drm/cardN`).
pub fn list_gpus() -> Result<Vec<GpuAdapter>, PlatformError> {
    let dir = fs::read_dir("/sys/class/drm").map_err(|e| PlatformError::Os(e.to_string()))?;
    let mut cards: Vec<(u32, std::path::PathBuf)> = dir
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let n = name.strip_prefix("card")?.parse::<u32>().ok()?;
            Some((n, e.path()))
        })
        .collect();
    cards.sort();
    let mut pci_ids: Option<Option<String>> = None;
    let mut out = Vec::new();
    for (index, card) in cards {
        let dev = card.join("device");
        let (Some(vendor_id), Some(device_id)) =
            (read_hex(&dev.join("vendor")), read_hex(&dev.join("device")))
        else {
            continue;
        };
        let driver = fs::read_link(dev.join("driver"))
            .ok()
            .and_then(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string))
            .unwrap_or_default();
        let slot = read_small(&dev.join("uevent"), MAX_SMALL_FILE)
            .and_then(|u| {
                u.lines()
                    .find_map(|l| l.strip_prefix("PCI_SLOT_NAME="))
                    .map(str::to_string)
            })
            .unwrap_or_default();
        let vendor = GpuVendor::from_pci_id(vendor_id);
        let name = (vendor == GpuVendor::Nvidia)
            .then(|| nvidia_model(&slot))
            .flatten()
            .or_else(|| {
                let ids = pci_ids.get_or_insert_with(|| {
                    read_small(Path::new("/usr/share/hwdata/pci.ids"), MAX_PCI_IDS)
                });
                ids.as_deref()
                    .and_then(|ids| pci_ids_name(ids, vendor_id, device_id))
            })
            .unwrap_or_else(|| format!("{} GPU {vendor_id:04x}:{device_id:04x}", vendor.name()));
        let driver_version = if driver == "nvidia" {
            read_small(Path::new("/sys/module/nvidia/version"), MAX_SMALL_FILE)
                .and_then(|v| pack_version(&v))
        } else {
            None
        };
        let dedicated_video_memory = read_small(&dev.join("mem_info_vram_total"), MAX_SMALL_FILE)
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0);
        out.push(GpuAdapter {
            index,
            name,
            vendor,
            vendor_id,
            device_id,
            luid: pci_slot_id(&slot).unwrap_or_else(|| i64::from(index)),
            dedicated_video_memory,
            software: software_driver(&driver),
            driver_version,
        });
    }
    Ok(out)
}

/// `/proc/<pid>/stat` → parent pid. The command name is in parentheses and may itself
/// contain spaces and parentheses, so parse from the last `)`.
fn parent_pid(stat: &str) -> Option<u32> {
    let after = stat.get(stat.rfind(')')?.checked_add(1)?..)?;
    let mut fields = after.split_whitespace();
    let _state = fields.next()?;
    fields.next()?.parse().ok()
}

/// Running processes: pid, parent pid and executable file name (the `exe` link's file
/// name when readable, else the kernel's 15-character `comm`).
pub fn list_processes() -> Result<Vec<ProcessInfo>, PlatformError> {
    let dir = fs::read_dir("/proc").map_err(|e| PlatformError::Os(e.to_string()))?;
    let mut out = Vec::new();
    for entry in dir.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let path = entry.path();
        // Processes exit while we look; skip those.
        let Some(stat) = read_small(&path.join("stat"), MAX_SMALL_FILE) else {
            continue;
        };
        let Some(parent_pid) = parent_pid(&stat) else {
            continue;
        };
        let exe = fs::read_link(path.join("exe"))
            .ok()
            .and_then(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string))
            .or_else(|| {
                read_small(&path.join("comm"), MAX_SMALL_FILE).map(|c| c.trim().to_string())
            })
            .unwrap_or_default();
        out.push(ProcessInfo {
            pid,
            parent_pid,
            exe,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pci_slots_and_versions() {
        assert_eq!(pci_slot_id("0000:01:00.0"), Some(0x0100));
        assert_eq!(
            pci_slot_id("0001:c1:1f.7"),
            Some((1 << 16) | (0xc1 << 8) | (0x1f << 3) | 7)
        );
        assert_eq!(pci_slot_id("0000:01:20.0"), None);
        assert_eq!(pci_slot_id("garbage"), None);
        assert_eq!(
            pack_version("615.71.09\n"),
            Some((615 << 32) | (71 << 16) | 9)
        );
        assert_eq!(pack_version("580"), Some(580 << 32));
        assert_eq!(pack_version("x.1"), None);
        assert_eq!(pack_version("1.99999999"), None);
    }

    #[test]
    fn parses_stat_with_odd_names() {
        assert_eq!(parent_pid("42 (a b) c) S 7 42 42 0 -1"), Some(7));
        assert_eq!(parent_pid("42 (x) R"), None);
        assert_eq!(parent_pid(""), None);
    }

    #[test]
    fn finds_names_in_pci_ids() {
        let ids = "# comment\n10de  NVIDIA Corporation\n\t2702  AD103 [GeForce RTX 4080 SUPER]\n\t\t1043 8888  sub\n1002  AMD\n\t2702  Not this\n";
        assert_eq!(
            pci_ids_name(ids, 0x10de, 0x2702).as_deref(),
            Some("AD103 [GeForce RTX 4080 SUPER]")
        );
        assert_eq!(pci_ids_name(ids, 0x10de, 0x1111), None);
        assert_eq!(
            pci_ids_name(ids, 0x1002, 0x2702).as_deref(),
            Some("Not this")
        );
    }

    #[test]
    fn lists_this_machine() {
        let procs = list_processes().unwrap();
        let me = std::process::id();
        assert!(procs.iter().any(|p| p.pid == me && !p.exe.is_empty()));
        // A container or CI box may have no GPU; just check it doesn't fail.
        let gpus = list_gpus().unwrap();
        for g in &gpus {
            assert!(!g.name.is_empty());
        }
        let a = monotonic_ns();
        let b = monotonic_ns();
        assert!(a > 0 && b >= a);
    }
}
