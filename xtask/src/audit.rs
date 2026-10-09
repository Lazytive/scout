//! Licence and asset audit (AGENTS.md rule 5):
//! - denied encoder names never appear in product source outside the policy file;
//! - a bundled GStreamer plugin directory (once installers exist) has no GPL plugins;
//! - every file under `assets/` is listed in ATTRIBUTION.md;
//! - front-end npm licences (`apps/klip-app/package-lock.json`, see `npm.rs`);
//! - app icons (`apps/klip-app/src-tauri/icons`) are listed in ATTRIBUTION.md like assets.

use std::path::Path;

use anyhow::{Context, Result, bail};

const POLICY: &str = "crates/klip-probe/src/policy.rs";
/// Bundled GStreamer plugins land here when installers are built (M13).
const BUNDLE_PLUGINS: &str = "dist/gstreamer/lib/gstreamer-1.0";

/// String literals of `pub const <name>: &[&str] = &[ ... ];` in `src`.
pub fn const_str_list(src: &str, name: &str) -> Result<Vec<String>> {
    let head = format!("pub const {name}: &[&str] = &[");
    let start = src
        .find(&head)
        .with_context(|| format!("{name} not found"))?
        + head.len();
    let body = src.get(start..).unwrap_or("");
    let end = body
        .find("];")
        .with_context(|| format!("{name} not terminated"))?;
    let body = body.get(..end).unwrap_or("");
    Ok(body
        .split('"')
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .map(|(_, s)| s.to_string())
        .collect())
}

/// Does `text` mention a denied element? Entries ending in `*` are prefixes.
pub fn mentions(text: &str, denied: &str) -> bool {
    let needle = denied.trim_end_matches('*');
    let mut from = 0;
    while let Some(pos) = text
        .get(from..)
        .and_then(|t| t.find(needle))
        .map(|p| p + from)
    {
        let before = text.get(..pos).and_then(|s| s.chars().next_back());
        let after = text
            .get(pos + needle.len()..)
            .and_then(|s| s.chars().next());
        let ident = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        if !ident(before) && (denied.ends_with('*') || !ident(after)) {
            return true;
        }
        from = pos + needle.len();
    }
    false
}

fn product_sources(root: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut out = Vec::new();
    for dir in ["crates", "apps"] {
        for f in crate::rust_files(&root.join(dir))? {
            let rel = f
                .strip_prefix(root)
                .unwrap_or(&f)
                .to_string_lossy()
                .replace('\\', "/");
            let test_support =
                rel.starts_with("crates/klip-testkit/") || rel.starts_with("apps/klip-testgame/");
            let tests = rel.contains("/tests/") || rel.ends_with("/tests.rs");
            if !test_support && !tests && rel != POLICY {
                out.push(f);
            }
        }
    }
    Ok(out)
}

/// Folders whose every file must be listed in ATTRIBUTION.md.
const ASSET_DIRS: &[&str] = &["assets", "apps/klip-app/src-tauri/icons"];

/// Whether `rel` (or a folder above it, written as `folder/`) is in ATTRIBUTION.md.
fn listed(attribution: &str, rel: &str) -> bool {
    if attribution.contains(rel) {
        return true;
    }
    let mut p = rel;
    while let Some((parent, _)) = p.rsplit_once('/') {
        if attribution.contains(&format!("`{parent}/`")) {
            return true;
        }
        p = parent;
    }
    false
}

pub fn run(root: &Path) -> Result<()> {
    let mut errs = Vec::new();
    let policy = std::fs::read_to_string(root.join(POLICY)).context(POLICY)?;
    let denied = const_str_list(&policy, "DENIED")?;
    let denied_plugins = const_str_list(&policy, "DENIED_PLUGINS")?;
    if denied.is_empty() || denied_plugins.is_empty() {
        bail!("denylists in {POLICY} are empty");
    }

    for f in product_sources(root)? {
        let text = std::fs::read_to_string(&f)?;
        for d in &denied {
            if mentions(&text, d) {
                errs.push(format!("{}: mentions denied encoder {d}", f.display()));
            }
        }
    }

    let bundle = root.join(BUNDLE_PLUGINS);
    if bundle.is_dir() {
        for e in std::fs::read_dir(&bundle)? {
            let name = e?.file_name().to_string_lossy().to_lowercase();
            let stem = name.split('.').next().unwrap_or("");
            let plugin = stem.trim_start_matches("lib").trim_start_matches("gst");
            if denied_plugins.iter().any(|d| d == plugin) {
                errs.push(format!(
                    "{BUNDLE_PLUGINS}/{name}: GPL plugin must not be bundled"
                ));
            }
        }
        println!("audit: checked bundled plugins in {BUNDLE_PLUGINS}");
    } else {
        println!("audit: no GStreamer bundle yet ({BUNDLE_PLUGINS})");
    }

    let attribution =
        std::fs::read_to_string(root.join("ATTRIBUTION.md")).context("ATTRIBUTION.md")?;
    let mut checked = 0usize;
    for dir in ASSET_DIRS {
        let d = root.join(dir);
        if !d.is_dir() {
            continue;
        }
        for f in crate::all_files(&d)? {
            let rel = f
                .strip_prefix(root)
                .unwrap_or(&f)
                .to_string_lossy()
                .replace('\\', "/");
            checked += 1;
            if !listed(&attribution, &rel) {
                errs.push(format!("{rel}: not listed in ATTRIBUTION.md"));
            }
        }
    }
    println!("audit: {checked} asset files checked against ATTRIBUTION.md");

    if crate::npm::exists(root) {
        let (shipped, dev) = crate::npm::audit(root, &mut errs)?;
        println!("audit: npm licences: {shipped} shipped packages, {dev} dev packages");
    } else {
        println!("audit: no front end yet");
    }

    if errs.is_empty() {
        println!("audit: ok");
        Ok(())
    } else {
        for e in &errs {
            eprintln!("audit: {e}");
        }
        bail!("audit failed ({} problems)", errs.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_const_lists() {
        let src = "pub const DENIED: &[&str] = &[\n    \"a\",\n    \"b*\", // c\n];\n";
        assert_eq!(const_str_list(src, "DENIED").unwrap(), ["a", "b*"]);
        assert!(const_str_list(src, "OTHER").is_err());
    }

    #[test]
    fn detects_mentions() {
        assert!(mentions("make(\"x264enc\")", "x264enc"));
        assert!(!mentions("make(\"x264encoder_like\")", "x264enc"));
        assert!(!mentions("nvx264enc", "x264enc"));
        assert!(mentions("\"avenc_h264_nvenc\"", "avenc_h264*"));
        assert!(!mentions("nothing here", "avenc_h264*"));
    }

    #[test]
    fn real_policy_parses() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let policy = std::fs::read_to_string(root.join(POLICY)).unwrap();
        assert!(
            const_str_list(&policy, "DENIED")
                .unwrap()
                .contains(&"x264enc".to_string())
        );
        assert!(
            const_str_list(&policy, "DENIED_PLUGINS")
                .unwrap()
                .contains(&"x264".to_string())
        );
    }
}
