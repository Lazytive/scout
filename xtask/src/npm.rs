//! Front end (`apps/klip-app`): the npm gates for `ci`, and the npm licence audit.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::Value;

pub const APP: &str = "apps/klip-app";

/// Licences allowed for packages that end up in the app (AGENTS.md rule 5).
const SHIPPED: &[&str] = &[
    "MIT",
    "MIT-0",
    "ISC",
    "0BSD",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "Apache-2.0",
    "Zlib",
    "MPL-2.0",
    "Unlicense",
    "CC0-1.0",
];

/// Extra licences fine for build and test tools only (not shipped): data and tooling.
const DEV_ONLY: &[&str] = &["CC-BY-4.0", "Python-2.0", "BlueOak-1.0.0"];

pub fn app_dir(root: &Path) -> PathBuf {
    root.join(APP)
}

pub fn exists(root: &Path) -> bool {
    app_dir(root).join("package.json").exists()
}

/// Whether an SPDX expression is satisfied by `allowed` (`A OR B`: either; `A AND B`:
/// both; parentheses ignored, which is fine for the simple expressions npm uses).
pub fn spdx_ok(expr: &str, allowed: &[&str]) -> bool {
    let e = expr.replace(['(', ')'], " ");
    e.split(" OR ").any(|alt| {
        alt.split(" AND ")
            .map(str::trim)
            .all(|l| !l.is_empty() && allowed.iter().any(|a| a.eq_ignore_ascii_case(l)))
    })
}

/// Check every package in `package-lock.json`. Returns (shipped, dev) counts.
pub fn audit(root: &Path, errs: &mut Vec<String>) -> Result<(usize, usize)> {
    let lock_path = app_dir(root).join("package-lock.json");
    let lock: Value = serde_json::from_str(
        &std::fs::read_to_string(&lock_path).context("apps/klip-app/package-lock.json")?,
    )?;
    let Some(packages) = lock.get("packages").and_then(Value::as_object) else {
        bail!("package-lock.json has no `packages` (lockfile v2+ needed)");
    };
    let dev_allowed: Vec<&str> = SHIPPED.iter().chain(DEV_ONLY).copied().collect();
    let (mut shipped, mut dev) = (0, 0);
    for (path, p) in packages {
        if path.is_empty() {
            continue;
        }
        let is_dev = p.get("dev").and_then(Value::as_bool).unwrap_or(false)
            || p.get("devOptional")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        let license = p.get("license").and_then(Value::as_str).unwrap_or("");
        let ok = if is_dev {
            dev += 1;
            spdx_ok(license, &dev_allowed)
        } else {
            shipped += 1;
            spdx_ok(license, SHIPPED)
        };
        if !ok {
            errs.push(format!(
                "npm {} {path}: licence {license:?} is not allowed",
                if is_dev { "dev package" } else { "package" }
            ));
        }
    }
    Ok((shipped, dev))
}

fn npm(root: &Path) -> std::process::Command {
    let mut c = crate::command(if cfg!(windows) { "npm.cmd" } else { "npm" });
    c.current_dir(app_dir(root));
    c
}

/// Install the npm packages if the lockfile changed since the last install.
fn install(root: &Path) -> Result<()> {
    let app = app_dir(root);
    let lock = app.join("package-lock.json");
    let installed = app.join("node_modules").join(".package-lock.json");
    let stale = match (std::fs::metadata(&lock), std::fs::metadata(&installed)) {
        (Ok(l), Ok(i)) => l.modified()? > i.modified()?,
        _ => true,
    };
    if stale {
        let mut c = npm(root);
        c.args(["ci", "--no-audit", "--no-fund"]);
        crate::run_step("npm ci", c)?;
    }
    Ok(())
}

/// Build the front end (the app embeds `dist/`); for `e2e`.
pub fn build(root: &Path) -> Result<()> {
    install(root)?;
    let mut c = npm(root);
    c.args(["run", "--silent", "build"]);
    crate::run_step("front-end build", c)
}

/// Install (only when the lockfile changed), lint, type-check, test and build the front
/// end. The build must come before the cargo gates: the Tauri crate embeds `dist/`.
pub fn gates(root: &Path) -> Result<()> {
    install(root)?;
    for (name, script) in [
        ("eslint", "lint"),
        ("svelte-check", "check"),
        ("vitest", "test"),
        ("front-end build", "build"),
    ] {
        let mut c = npm(root);
        c.args(["run", "--silent", script]);
        crate::run_step(name, c)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spdx_expressions() {
        assert!(spdx_ok("MIT", SHIPPED));
        assert!(spdx_ok("(MIT OR Apache-2.0)", SHIPPED));
        assert!(spdx_ok("Apache-2.0 OR GPL-3.0", SHIPPED));
        assert!(!spdx_ok("GPL-3.0", SHIPPED));
        assert!(!spdx_ok("MIT AND GPL-2.0", SHIPPED));
        assert!(!spdx_ok("", SHIPPED));
        assert!(!spdx_ok("CC-BY-4.0", SHIPPED));
        assert!(spdx_ok("CC-BY-4.0", &["CC-BY-4.0"]));
    }

    #[test]
    fn the_real_lockfile_passes() {
        let root = crate::root();
        if !exists(&root) {
            return;
        }
        let mut errs = Vec::new();
        let (shipped, dev) = audit(&root, &mut errs).unwrap();
        assert!(errs.is_empty(), "{errs:?}");
        assert!(shipped >= 2 && dev > 10, "{shipped} {dev}");
    }
}
