//! What the setup does without a window: the switches (NSIS's, which Tauri's updater
//! uses too), the install folder, the inner installer's arguments and the progress
//! estimate.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The switches the setup understands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Args {
    /// `/S`: install with no window at all.
    pub silent: bool,
    /// `/P`: show the window but need no click (what the updater passes).
    pub passive: bool,
    /// `/UPDATE`: an update over an installed Scout (handed to the installer).
    pub update: bool,
    /// `/R`: start Scout when done.
    pub run_after: bool,
    /// `/D=<folder>`: install there. NSIS takes the rest of the line, unquoted, so the
    /// folder may arrive in pieces.
    pub dir: Option<PathBuf>,
    /// `/ARGS <...>`: Scout's own arguments when it is started afterwards.
    pub app_args: Vec<String>,
}

/// Read the command line (without the program's own name).
pub fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Args {
    let mut out = Args::default();
    let mut dir: Option<String> = None;
    let mut in_app_args = false;
    for a in args {
        if in_app_args {
            out.app_args.push(a);
            continue;
        }
        if let Some(d) = a.strip_prefix("/D=") {
            dir = Some(d.trim_matches('"').to_string());
            continue;
        }
        if let Some(d) = dir.as_mut()
            && !a.starts_with('/')
        {
            // A folder with spaces came as several arguments: join them back.
            d.push(' ');
            d.push_str(a.trim_matches('"'));
            continue;
        }
        match a.as_str() {
            "/S" => out.silent = true,
            "/P" => out.passive = true,
            "/UPDATE" => out.update = true,
            "/R" => out.run_after = true,
            "/ARGS" => in_app_args = true,
            _ => {}
        }
    }
    out.dir = dir
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())
        .map(PathBuf::from);
    out
}

/// Where Scout goes: `/D=`, else `%LOCALAPPDATA%\Scout` (the per-user install).
pub fn install_dir(args: &Args) -> Option<PathBuf> {
    args.dir
        .clone()
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(|l| PathBuf::from(l).join("Scout")))
}

/// The embedded installer's switches: silent, the update flag as given, the folder last
/// (NSIS wants `/D=` last and unquoted).
pub fn inner_args(args: &Args, dir: &Path) -> Vec<OsString> {
    let mut v = vec![OsString::from("/S")];
    if args.update {
        v.push(OsString::from("/UPDATE"));
    }
    let mut d = OsString::from("/D=");
    d.push(dir.as_os_str());
    v.push(d);
    v
}

/// Bytes of the files under `dir` (symbolic links not followed, 16 levels at most).
pub fn dir_bytes(dir: &Path) -> u64 {
    let mut total: u64 = 0;
    let mut stack = vec![(dir.to_path_buf(), 0u8)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(meta) = e.metadata() else {
                continue;
            };
            if meta.is_file() {
                total = total.saturating_add(meta.len());
            } else if meta.is_dir() && depth < 16 {
                stack.push((e.path(), depth.saturating_add(1)));
            }
        }
    }
    total
}

/// Progress, 0–95, while the installer runs (it says nothing itself): the bytes that
/// arrived against what a whole install holds. The last 5 % are its end.
pub fn progress(bytes: u64, expected: u64) -> u8 {
    if expected == 0 {
        return 0;
    }
    bytes
        .saturating_mul(95)
        .checked_div(expected)
        .and_then(|p| u8::try_from(p).ok())
        .unwrap_or(95)
        .min(95)
}

/// Where the embedded installer is written before it runs.
pub fn work_dir() -> PathBuf {
    std::env::temp_dir().join(format!("scout-setup-{}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &[&str]) -> Args {
        parse_args(s.iter().map(|a| a.to_string()))
    }

    #[test]
    fn the_switches_of_nsis_and_the_updater_are_understood() {
        assert_eq!(parse(&[]), Args::default());
        assert!(parse(&["/S"]).silent);
        let u = parse(&["/P", "/UPDATE", "/R", "/ARGS", "--minimized", "x y"]);
        assert!(u.passive && u.update && u.run_after && !u.silent);
        assert_eq!(u.app_args, ["--minimized", "x y"]);
        assert_eq!(u.dir, None);
        assert!(
            parse(&["/s", "/p", "--other"]).app_args.is_empty(),
            "unknown switches are ignored"
        );
    }

    #[test]
    fn the_folder_may_arrive_in_pieces_and_is_the_last_switch() {
        let one = parse(&["/S", "/D=C:\\Users\\me\\AppData\\Local\\Scout"]);
        assert_eq!(
            one.dir.as_deref(),
            Some(Path::new("C:\\Users\\me\\AppData\\Local\\Scout"))
        );
        let split = parse(&["/D=C:\\Program", "Files\\Scout"]);
        assert_eq!(
            split.dir.as_deref(),
            Some(Path::new("C:\\Program Files\\Scout"))
        );
        let quoted = parse(&["/D=\"C:\\Games\\Scout\""]);
        assert_eq!(quoted.dir.as_deref(), Some(Path::new("C:\\Games\\Scout")));
        assert_eq!(parse(&["/D="]).dir, None);
    }

    #[test]
    fn the_inner_installer_runs_silently_into_the_folder() {
        let a = parse(&["/P", "/UPDATE"]);
        let v = inner_args(&a, Path::new("C:\\x y\\Scout"));
        assert_eq!(
            v,
            ["/S", "/UPDATE", "/D=C:\\x y\\Scout"].map(OsString::from)
        );
        let v = inner_args(&Args::default(), Path::new("D:\\Scout"));
        assert_eq!(v, ["/S", "/D=D:\\Scout"].map(OsString::from));
    }

    #[test]
    fn progress_climbs_to_95_and_no_further() {
        assert_eq!(progress(0, 0), 0);
        assert_eq!(progress(50, 0), 0);
        assert_eq!(progress(0, 100), 0);
        assert_eq!(progress(50, 100), 47);
        assert_eq!(progress(100, 100), 95);
        assert_eq!(progress(1000, 100), 95);
        assert_eq!(progress(u64::MAX, 1), 95);
    }

    #[test]
    fn dir_bytes_sums_files_below_and_the_work_dir_names_the_process() {
        let dir = std::env::temp_dir().join(format!("scout-setup-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.bin"), [0u8; 1000]).unwrap();
        std::fs::write(dir.join("sub").join("b.bin"), [0u8; 234]).unwrap();
        assert_eq!(dir_bytes(&dir), 1234);
        assert_eq!(dir_bytes(&dir.join("missing")), 0);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(work_dir().ends_with(format!("scout-setup-{}", std::process::id())));
    }
}
