//! Lenient loading (repair field by field) and strict merge-patch changes.

use serde_json::{Map, Value};
use serde_path_to_error::Segment;

use crate::{Issue, MAX_LIST, MAX_PROFILES, SCHEMA_VERSION, Settings, SettingsError};

/// Most fields removed one by one before giving up on a file.
const MAX_REPAIRS: usize = 512;

/// Result of a lenient parse.
pub(crate) struct Parsed {
    pub settings: Settings,
    pub issues: Vec<Issue>,
    /// Nothing could be used: the defaults were taken as a whole.
    pub reset: bool,
}

pub(crate) fn from_json(text: &str) -> (Settings, Vec<Issue>) {
    let p = parse(text);
    (p.settings, p.issues)
}

pub(crate) fn parse(text: &str) -> Parsed {
    match serde_json::from_str::<Value>(text) {
        Ok(v) => from_value(v),
        Err(e) => reset(Issue::new(
            "",
            format!("not valid JSON ({e}); using the defaults"),
        )),
    }
}

pub(crate) fn reset(issue: Issue) -> Parsed {
    Parsed {
        settings: Settings::default(),
        issues: vec![issue],
        reset: true,
    }
}

fn path_string(p: &serde_path_to_error::Path) -> String {
    let s = p.to_string();
    if s == "." { String::new() } else { s }
}

pub(crate) fn from_value(mut v: Value) -> Parsed {
    let Some(obj) = v.as_object() else {
        return reset(Issue::new("", "not a JSON object; using the defaults"));
    };
    let mut issues = Vec::new();
    if let Some(n) = obj.get("version").and_then(Value::as_u64)
        && n > u64::from(SCHEMA_VERSION)
    {
        issues.push(Issue::new(
            "version",
            format!("written by a newer Scout (version {n}); settings it added are ignored"),
        ));
    }
    cap_sizes(&mut v, "", &mut issues);
    let mut repairs = 0usize;
    let settings = loop {
        match serde_path_to_error::deserialize::<_, Settings>(&v) {
            Ok(s) => break s,
            Err(e) => {
                let path = path_string(e.path());
                let msg = e.inner().to_string();
                repairs = repairs.saturating_add(1);
                if repairs > MAX_REPAIRS || !remove_at(&mut v, e.path()) {
                    issues.push(Issue::new(
                        path,
                        format!("{msg}; using the defaults for everything"),
                    ));
                    return Parsed {
                        settings: Settings::default(),
                        issues,
                        reset: true,
                    };
                }
                issues.push(Issue::new(path, format!("{msg}; using the default")));
            }
        }
    };
    let mut settings = settings;
    issues.extend(settings.validate());
    Parsed {
        settings,
        issues,
        reset: false,
    }
}

/// Truncate oversized arrays and objects before deserialising, so repairing a hostile
/// file stays cheap (each repair re-reads the whole value).
fn cap_sizes(v: &mut Value, path: &str, issues: &mut Vec<Issue>) {
    match v {
        Value::Array(a) => {
            if a.len() > MAX_LIST {
                issues.push(Issue::new(
                    path,
                    format!("more than {MAX_LIST} entries; the rest were dropped"),
                ));
                a.truncate(MAX_LIST);
            }
            for (i, e) in a.iter_mut().enumerate() {
                cap_sizes(e, &format!("{path}[{i}]"), issues);
            }
        }
        Value::Object(o) => {
            if o.len() > MAX_PROFILES {
                issues.push(Issue::new(
                    path,
                    format!("more than {MAX_PROFILES} entries; the rest were dropped"),
                ));
                let keep: Map<String, Value> =
                    std::mem::take(o).into_iter().take(MAX_PROFILES).collect();
                *o = keep;
            }
            for (k, e) in o.iter_mut() {
                let p = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                cap_sizes(e, &p, issues);
            }
        }
        _ => {}
    }
}

enum Step {
    Key(String),
    Index(usize),
}

/// Remove what the error points at: the innermost list element on the path (a bad entry
/// goes as a whole), else the field itself (it then gets its default).
fn remove_at(v: &mut Value, path: &serde_path_to_error::Path) -> bool {
    let steps: Vec<Step> = path
        .iter()
        .filter_map(|s| match s {
            Segment::Seq { index } => Some(Step::Index(*index)),
            Segment::Map { key } => Some(Step::Key(key.clone())),
            Segment::Enum { .. } | Segment::Unknown => None,
        })
        .collect();
    let cut = steps
        .iter()
        .rposition(|s| matches!(s, Step::Index(_)))
        .or_else(|| steps.len().checked_sub(1));
    let Some(cut) = cut else { return false };
    let (parents, rest) = steps.split_at(cut);
    let Some(last) = rest.first() else {
        return false;
    };
    let mut node = v;
    for s in parents {
        let next = match (s, node) {
            (Step::Key(k), Value::Object(m)) => m.get_mut(k),
            (Step::Index(i), Value::Array(a)) => a.get_mut(*i),
            _ => None,
        };
        match next {
            Some(n) => node = n,
            None => return false,
        }
    }
    match (last, node) {
        (Step::Key(k), Value::Object(m)) => m.remove(k).is_some(),
        (Step::Index(i), Value::Array(a)) if *i < a.len() => {
            a.remove(*i);
            true
        }
        _ => false,
    }
}

/// RFC 7396: objects merge, `null` removes, anything else replaces.
fn merge_patch(target: &mut Value, patch: &Value) {
    let Value::Object(p) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(Map::new());
    }
    if let Value::Object(t) = target {
        for (k, pv) in p {
            if pv.is_null() {
                t.remove(k);
            } else {
                merge_patch(t.entry(k.clone()).or_insert(Value::Null), pv);
            }
        }
    }
}

/// The first key in `patch` that the settings don't have.
fn unknown_key(patch: &Value, back: &Value, path: &str) -> Option<String> {
    let join = |k: &str| {
        if path.is_empty() {
            k.to_string()
        } else {
            format!("{path}.{k}")
        }
    };
    match (patch, back) {
        (Value::Object(p), Value::Object(b)) => p.iter().find_map(|(k, pv)| {
            if pv.is_null() {
                return None;
            }
            match b.get(k) {
                None => Some(join(k)),
                Some(bv) => unknown_key(pv, bv, &join(k)),
            }
        }),
        (Value::Array(p), Value::Array(b)) => p
            .iter()
            .zip(b.iter())
            .enumerate()
            .find_map(|(i, (pv, bv))| unknown_key(pv, bv, &format!("{path}[{i}]"))),
        _ => None,
    }
}

/// The problem part of an issue message (`problem; what was done about it`).
fn problem(i: Issue) -> Issue {
    let message = i.message.split("; ").next().unwrap_or_default().to_string();
    Issue {
        path: i.path,
        message,
    }
}

pub(crate) fn apply_patch(cur: &Settings, patch: &Value) -> Result<Settings, SettingsError> {
    let Value::Object(p) = patch else {
        return Err(SettingsError::Invalid(Issue::new(
            "",
            "a settings change must be a JSON object",
        )));
    };
    let mut p = p.clone();
    p.remove("version");
    let patch = Value::Object(p);
    let mut v = cur.to_value()?;
    merge_patch(&mut v, &patch);
    let mut s: Settings = serde_path_to_error::deserialize(&v).map_err(|e| {
        SettingsError::Invalid(Issue::new(path_string(e.path()), e.inner().to_string()))
    })?;
    let back = s.to_value()?;
    if let Some(k) = unknown_key(&patch, &back, "") {
        return Err(SettingsError::Invalid(Issue::new(k, "unknown setting")));
    }
    if let Some(i) = s.validate().into_iter().next() {
        return Err(SettingsError::Invalid(problem(i)));
    }
    Ok(s)
}
