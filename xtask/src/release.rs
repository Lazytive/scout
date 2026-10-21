//! `cargo xtask bundle --sign` (ADR 0022): after `tauri build` signed the installer
//! (`createUpdaterArtifacts`), the updater manifest `latest.json` for the GitHub
//! release, beside the installer. The app's update check reads it at
//! `<repo>/releases/latest/download/latest.json`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub const REPO: &str = "https://github.com/Lazytive/scout";

/// What `tauri build` names the NSIS installer.
pub fn installer_name(version: &str) -> String {
    format!("Scout_{version}_x64-setup.exe")
}

/// Where the release's installer is downloaded from (the tag is `v<version>`).
pub fn installer_url(version: &str) -> String {
    format!(
        "{REPO}/releases/download/v{version}/{}",
        installer_name(version)
    )
}

/// The manifest text (Tauri's updater format): the version, the notes shown in the
/// update prompt, the date (RFC 3339) and the Windows installer with its signature.
pub fn manifest(version: &str, signature: &str, notes: &str, date: &str) -> String {
    let v = serde_json::json!({
        "version": version,
        "notes": notes,
        "pub_date": date,
        "platforms": {
            "windows-x86_64": {
                "signature": signature.trim(),
                "url": installer_url(version),
            }
        }
    });
    let mut text = serde_json::to_string_pretty(&v).unwrap_or_default();
    text.push('\n');
    text
}

/// Now, as `2026-10-21T02:00:00Z`.
pub fn now_rfc3339() -> String {
    jiff::Timestamp::now()
        .strftime("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

/// Write `latest.json` into `nsis` (where the installer and its `.sig` are). The notes
/// come from `notes_file`, else a line pointing at the release page.
pub fn write_manifest(nsis: &Path, version: &str, notes_file: Option<&Path>) -> Result<PathBuf> {
    let sig_path = nsis.join(format!("{}.sig", installer_name(version)));
    let signature = std::fs::read_to_string(&sig_path).with_context(|| {
        format!(
            "reading {} (did tauri build sign the installer? TAURI_SIGNING_PRIVATE_KEY)",
            sig_path.display()
        )
    })?;
    if signature.trim().is_empty() {
        bail!("{} is empty", sig_path.display());
    }
    let notes = match notes_file {
        Some(p) => std::fs::read_to_string(p)
            .with_context(|| format!("reading the release notes {}", p.display()))?,
        None => format!("Scout {version}. What changed is on the release page."),
    };
    let out = nsis.join("latest.json");
    std::fs::write(
        &out,
        manifest(version, &signature, notes.trim(), &now_rfc3339()),
    )
    .with_context(|| format!("writing {}", out.display()))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_names_the_release_installer_with_its_signature() {
        let text = manifest("0.1.0", "  sig==\n", "Notes.", "2026-10-21T02:00:00Z");
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["version"], "0.1.0");
        assert_eq!(v["notes"], "Notes.");
        assert_eq!(v["pub_date"], "2026-10-21T02:00:00Z");
        let w = &v["platforms"]["windows-x86_64"];
        assert_eq!(w["signature"], "sig==");
        assert_eq!(
            w["url"],
            "https://github.com/Lazytive/scout/releases/download/v0.1.0/Scout_0.1.0_x64-setup.exe"
        );
        assert!(text.ends_with("}\n"));
    }

    #[test]
    fn the_date_is_rfc_3339_to_the_second() {
        let d = now_rfc3339();
        assert_eq!(d.len(), 20, "{d}");
        assert!(
            d.starts_with("20") && d.ends_with('Z') && d.chars().nth(10) == Some('T'),
            "{d}"
        );
    }

    #[test]
    fn write_manifest_needs_the_signature_and_takes_notes_from_a_file() {
        let dir = std::env::temp_dir().join(format!("scout-release-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let missing = write_manifest(&dir, "0.1.0", None).unwrap_err().to_string();
        assert!(
            missing.contains("Scout_0.1.0_x64-setup.exe.sig"),
            "{missing}"
        );

        std::fs::write(dir.join("Scout_0.1.0_x64-setup.exe.sig"), "abc\n").unwrap();
        let out = write_manifest(&dir, "0.1.0", None).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(v["platforms"]["windows-x86_64"]["signature"], "abc");
        assert!(v["notes"].as_str().unwrap().contains("release page"));

        std::fs::write(dir.join("notes.md"), "- One\n- Two\n").unwrap();
        write_manifest(&dir, "0.1.0", Some(&dir.join("notes.md"))).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(v["notes"], "- One\n- Two");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
