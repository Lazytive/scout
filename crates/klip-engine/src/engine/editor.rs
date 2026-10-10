//! Editor commands (M8.4): a clip's project file and what the preview plays.
//!
//! `editor.project.get {path}` → the project (a new one when none is saved), its repair
//! issues, and the clip's `SourceInfo`. `editor.project.set {path, project}` → the
//! project repaired and saved to `.klip/projects/<key>.json`. `editor.proxy {path}` →
//! what the preview should play: the clip itself for H.264, otherwise its H.264 proxy
//! (`ready` with its path, or `making` with the job id of the export that makes it).

use std::path::{Path, PathBuf};

use base64::Engine as _;
use klip_control::CommandError;
use klip_edit::{MAX_FILE_BYTES, Project, SourceInfo, TextLayer};
use klip_library::{ClipInfo, Library, probe_file};
use serde_json::{Value, json};

use super::export::ExportKind;
use super::library::str_arg;
use super::{Inner, lock};

/// The editor's view of a clip: the index entry when it has one, a probe otherwise.
pub(super) fn source_of(info: &ClipInfo) -> SourceInfo {
    SourceInfo {
        width: info.width,
        height: info.height,
        duration_ms: info.duration_ms.unwrap_or(0),
        fps: info.fps,
        bookmarks_ms: info.bookmarks_ms.clone(),
    }
}

fn issues_json(issues: &[klip_edit::Issue]) -> Value {
    serde_json::to_value(issues).unwrap_or(Value::Array(Vec::new()))
}

/// A project fitted to its clip: the clip's length wins, then repaired.
pub(super) fn fit_project(
    mut project: Project,
    source: &SourceInfo,
) -> (Project, Vec<klip_edit::Issue>) {
    if source.duration_ms > 0 {
        project.source_ms = source.duration_ms;
    }
    let issues = project.repair();
    (project, issues)
}

impl Inner {
    /// What is known about a clip (the index, else the file).
    pub(super) fn clip_info(
        &self,
        lib: &Library,
        path: &Path,
        key: &str,
    ) -> Result<ClipInfo, CommandError> {
        {
            let state = lock(&self.library);
            if state.root() == Some(lib.root())
                && let Some(info) = state.info_of(key)
            {
                return Ok(info);
            }
        }
        probe_file(path).map_err(|e| CommandError::new("failed", e))
    }

    /// The saved project of `key`, or a new one; `(project, issues, status)`.
    pub(super) fn load_project(
        &self,
        lib: &Library,
        key: &str,
        source: &SourceInfo,
    ) -> Result<(Project, Vec<klip_edit::Issue>, &'static str), CommandError> {
        let path = lib
            .project_path(key)
            .ok_or_else(|| CommandError::bad_args("bad clip key"))?;
        let (project, mut issues, status) = if path.is_file() {
            let loaded = {
                let _guard = lock(&self.library_lock);
                klip_edit::load(&path)
            };
            let status = match loaded.status {
                klip_edit::LoadStatus::Loaded => "saved",
                klip_edit::LoadStatus::Repaired => "repaired",
                klip_edit::LoadStatus::Reset => "reset",
                klip_edit::LoadStatus::Missing => "new",
            };
            (loaded.project, loaded.issues, status)
        } else {
            (Project::new(source.duration_ms), Vec::new(), "new")
        };
        let (project, more) = fit_project(project, source);
        issues.extend(more);
        Ok((project, issues, status))
    }

    fn project_json(
        key: &str,
        path: &Path,
        project: &Project,
        issues: &[klip_edit::Issue],
        status: &str,
        source: &SourceInfo,
    ) -> Result<Value, CommandError> {
        let value = project
            .to_value()
            .map_err(|e| CommandError::new("internal", e.to_string()))?;
        Ok(json!({
            "key": key,
            "path": path.to_string_lossy(),
            "project": value,
            "issues": issues_json(issues),
            "status": status,
            "source": source,
        }))
    }

    pub(super) fn editor_project_get(&self, args: &Value) -> Result<Value, CommandError> {
        let lib = self.clip_library()?;
        let (path, key) = self.clip_arg(&lib, str_arg(args, "path")?)?;
        let source = source_of(&self.clip_info(&lib, &path, &key)?);
        let (project, issues, status) = self.load_project(&lib, &key, &source)?;
        Self::project_json(&key, &path, &project, &issues, status, &source)
    }

    pub(super) fn editor_project_set(&self, args: &Value) -> Result<Value, CommandError> {
        let lib = self.clip_library()?;
        let (path, key) = self.clip_arg(&lib, str_arg(args, "path")?)?;
        let value = args
            .get("project")
            .filter(|v| v.is_object())
            .cloned()
            .ok_or_else(|| CommandError::bad_args("project must be an object"))?;
        let bytes = serde_json::to_vec(&value)
            .map(|v| v.len())
            .unwrap_or(usize::MAX);
        if bytes as u64 > MAX_FILE_BYTES {
            return Err(CommandError::bad_args("project is too big"));
        }
        let source = source_of(&self.clip_info(&lib, &path, &key)?);
        let (project, mut issues) = Project::from_value(value);
        let (project, more) = fit_project(project, &source);
        issues.extend(more);
        let file: PathBuf = lib
            .project_path(&key)
            .ok_or_else(|| CommandError::bad_args("bad clip key"))?;
        {
            let _guard = lock(&self.library_lock);
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| CommandError::new("failed", e.to_string()))?;
            }
            klip_edit::save(&file, &project)
                .map_err(|e| CommandError::new("failed", e.to_string()))?;
        }
        Self::project_json(&key, &path, &project, &issues, "saved", &source)
    }

    /// A text layer as the export draws it, for the preview: `klip_edit::render_layer`
    /// at the output size, as a PNG (base64) with its place in the frame.
    pub(super) fn editor_text_render(&self, args: &Value) -> Result<Value, CommandError> {
        let lib = self.clip_library()?;
        self.clip_arg(&lib, str_arg(args, "path")?)?;
        let layer: TextLayer = args
            .get("layer")
            .cloned()
            .filter(Value::is_object)
            .and_then(|v| serde_json::from_value(v).ok())
            .ok_or_else(|| CommandError::bad_args("layer must be a text layer"))?;
        let side = |name: &str| -> Result<u32, CommandError> {
            args.get(name)
                .and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok())
                .filter(|v| (1..=klip_edit::text::render::MAX_FRAME_SIDE).contains(v))
                .ok_or_else(|| CommandError::bad_args(format!("{name} must be 1..=4096")))
        };
        let (width, height) = (side("width")?, side("height")?);
        let bitmap = klip_edit::render_layer(&layer, width, height)
            .map_err(|e| CommandError::bad_args(e.to_string()))?;
        let png = bitmap
            .png()
            .map_err(|e| CommandError::new("failed", e.to_string()))?;
        Ok(json!({
            "x": bitmap.x,
            "y": bitmap.y,
            "width": bitmap.width,
            "height": bitmap.height,
            "frame_width": bitmap.frame_width,
            "frame_height": bitmap.frame_height,
            "png": base64::engine::general_purpose::STANDARD.encode(png),
        }))
    }

    /// What the preview plays for `path`.
    pub(super) fn editor_proxy(&self, args: &Value) -> Result<Value, CommandError> {
        let lib = self.clip_library()?;
        let (path, key) = self.clip_arg(&lib, str_arg(args, "path")?)?;
        let info = self.clip_info(&lib, &path, &key)?;
        if info.codec.as_deref() == Some("H.264") {
            return Ok(json!({ "status": "original", "path": path.to_string_lossy(), "key": key }));
        }
        let proxy = lib
            .proxy_path(&key)
            .ok_or_else(|| CommandError::bad_args("bad clip key"))?;
        if proxy.is_file() {
            return Ok(json!({ "status": "ready", "path": proxy.to_string_lossy(), "key": key }));
        }
        if let Some(id) = self.export_running_for(&key, "proxy") {
            return Ok(json!({ "status": "making", "id": id, "key": key }));
        }
        let (id, _) = self.start_export(ExportKind::Proxy, &lib, path, key.clone(), &info)?;
        Ok(json!({ "status": "making", "id": id, "key": key }))
    }
}
