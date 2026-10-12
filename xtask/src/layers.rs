//! Layering rules (ADR 0001): dependency direction between crates, and `unsafe` only in
//! klip-platform.

use std::path::Path;

use anyhow::{Result, bail};
use serde_json::Value;

/// Pure crates: no GStreamer, no OS bindings, no engine/app crates.
pub const PURE: &[&str] = &[
    "klip-media",
    "klip-buffer",
    "klip-mp4",
    "klip-control",
    "klip-testkit",
    "klip-settings",
    "klip-library",
    "klip-edit",
    "klip-share",
];
/// Media engine (and the platform layer): no UI, no apps.
pub const ENGINE: &[&str] = &[
    "klip-platform",
    "klip-probe",
    "klip-capture",
    "klip-recorder",
    "klip-export",
    "klip-engine",
];
/// Applications and tools: may depend on anything below them.
pub const APPS: &[&str] = &[
    "klip-cli",
    "klip-testgame",
    "klip-app",
    "klip-share-server",
    "xtask",
];

const UI_PREFIXES: &[&str] = &["tauri", "wry", "tao"];
const NATIVE_PREFIXES: &[&str] = &["gstreamer", "windows", "glib", "ashpd", "pipewire"];

/// One dependency edge: (package, dependency name, kind) where kind is `normal`, `dev` or
/// `build`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub kind: String,
}

/// Extract package names and direct dependency edges from `cargo metadata --no-deps`.
pub fn edges_from_metadata(meta: &Value) -> Result<(Vec<String>, Vec<Edge>)> {
    let Some(packages) = meta.get("packages").and_then(Value::as_array) else {
        bail!("cargo metadata has no packages");
    };
    let mut names = Vec::new();
    let mut edges = Vec::new();
    for p in packages {
        let name = p
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        for d in p
            .get("dependencies")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            edges.push(Edge {
                from: name.clone(),
                to: d
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                kind: d
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("normal")
                    .to_string(),
            });
        }
        names.push(name);
    }
    Ok((names, edges))
}

/// Check the rules; returns every violation.
pub fn check_edges(names: &[String], edges: &[Edge]) -> Vec<String> {
    let mut errs = Vec::new();
    for n in names {
        if !PURE.contains(&n.as_str())
            && !ENGINE.contains(&n.as_str())
            && !APPS.contains(&n.as_str())
        {
            errs.push(format!(
                "{n} is not classified in xtask/src/layers.rs (update ADR 0001 too)"
            ));
        }
    }
    for e in edges {
        let from = e.from.as_str();
        let to = e.to.as_str();
        let starts = |prefixes: &[&str]| prefixes.iter().any(|p| to.starts_with(p));
        // Dev-dependencies of pure crates may use test helpers, but never native bindings.
        if PURE.contains(&from) {
            if starts(NATIVE_PREFIXES) || starts(UI_PREFIXES) {
                errs.push(format!("pure crate {from} depends on {to}"));
            }
            if e.kind != "dev" && (ENGINE.contains(&to) || APPS.contains(&to)) {
                errs.push(format!("pure crate {from} depends on higher layer {to}"));
            }
        }
        if ENGINE.contains(&from) {
            if starts(UI_PREFIXES) {
                errs.push(format!("engine crate {from} depends on UI crate {to}"));
            }
            if APPS.contains(&to) {
                errs.push(format!("engine crate {from} depends on app {to}"));
            }
        }
    }
    errs
}

/// Find `unsafe` used as a keyword in Rust source (blocks, fns, impls, externs, attributes).
/// Line comments are ignored. Returns 1-based line numbers.
pub fn unsafe_lines(src: &str) -> Vec<usize> {
    // Built at runtime so this file doesn't flag itself.
    let kw: String = ["un", "safe"].concat();
    let mut hits = Vec::new();
    for (i, line) in src.lines().enumerate() {
        let code = match line.find("//") {
            Some(c) => line.get(..c).unwrap_or(""),
            None => line,
        };
        let mut search = 0;
        while let Some(pos) = code
            .get(search..)
            .and_then(|s| s.find(&kw))
            .map(|p| p + search)
        {
            let before = code.get(..pos).and_then(|s| s.chars().next_back());
            let after = code.get(pos + kw.len()..).unwrap_or("");
            let word_start = before.is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '"'));
            let rest = after.trim_start();
            let keyword_use = ["{", "fn", "impl", "extern", "trait", "("]
                .iter()
                .any(|t| rest.starts_with(t))
                && !after.starts_with(|c: char| c.is_alphanumeric() || c == '_');
            if word_start && keyword_use {
                hits.push(i + 1);
                break;
            }
            search = pos + kw.len();
        }
    }
    hits
}

/// Scan all `.rs` files under `root/{crates,apps,xtask}` except klip-platform.
pub fn scan_unsafe(root: &Path) -> Result<Vec<String>> {
    let mut errs = Vec::new();
    for dir in ["crates", "apps", "xtask"] {
        for file in crate::rust_files(&root.join(dir))? {
            let rel = file
                .strip_prefix(root)
                .unwrap_or(&file)
                .to_string_lossy()
                .replace('\\', "/");
            if rel.starts_with("crates/klip-platform/") {
                continue;
            }
            let src = std::fs::read_to_string(&file)?;
            for line in unsafe_lines(&src) {
                errs.push(format!("{rel}:{line}: unsafe outside klip-platform"));
            }
        }
    }
    Ok(errs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(from: &str, to: &str, kind: &str) -> Edge {
        Edge {
            from: from.into(),
            to: to.into(),
            kind: kind.into(),
        }
    }

    fn names() -> Vec<String> {
        PURE.iter()
            .chain(ENGINE)
            .chain(APPS)
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn good_graph_passes() {
        let edges = [
            edge("klip-buffer", "klip-media", "normal"),
            edge("klip-capture", "gstreamer", "normal"),
            edge("klip-recorder", "klip-capture", "normal"),
            edge("klip-cli", "klip-recorder", "normal"),
            edge("klip-mp4", "tempfile", "dev"),
        ];
        assert!(check_edges(&names(), &edges).is_empty());
    }

    #[test]
    fn planted_violations_fail() {
        let bad = [
            edge("klip-buffer", "gstreamer", "normal"),
            edge("klip-mp4", "windows", "dev"),
            edge("klip-media", "klip-capture", "normal"),
            edge("klip-recorder", "tauri", "normal"),
            edge("klip-capture", "klip-cli", "normal"),
        ];
        for e in &bad {
            assert_eq!(
                check_edges(&names(), std::slice::from_ref(e)).len(),
                1,
                "{e:?}"
            );
        }
        let mut n = names();
        n.push("klip-mystery".into());
        assert_eq!(check_edges(&n, &[]).len(), 1);
    }

    #[test]
    fn unsafe_detection() {
        let kw = ["un", "safe"].concat();
        let src = format!(
            "fn a() {{ {kw} {{ x() }} }}\n// {kw} {{ comment }}\nlet s = \"{kw} {{\";\n#![deny({kw}_code)]\n{kw} fn f() {{}}\n#[{kw}(no_mangle)]\nlet not_{kw} = 1;\n"
        );
        assert_eq!(unsafe_lines(&src), vec![1, 5, 6]);
    }
}
