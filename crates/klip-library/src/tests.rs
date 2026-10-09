use std::time::{Duration, SystemTime};

use super::*;

fn clip(dir: &Path, rel: &str, bytes: usize, age_secs: u64) -> PathBuf {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, vec![0u8; bytes]).unwrap();
    let f = File::options().write(true).open(&path).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(age_secs))
        .unwrap();
    path
}

fn keys(files: &[ClipFile]) -> Vec<&str> {
    files.iter().map(|f| f.key.as_str()).collect()
}

#[test]
fn scan_finds_finished_clips_two_levels_deep() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    clip(root, "a.mp4", 10, 50);
    clip(root, "Game One/b.MP4", 20, 40);
    clip(root, "Game One/deeper/c.mp4", 30, 30);
    clip(root, "Game One/notes.txt", 5, 30);
    clip(root, "Game One/d.mp4.tmp", 5, 30);
    clip(root, "Game Two/rec.mp4", 40, 20);
    fs::write(root.join("Game Two/rec.mp4.recording.json"), "{}").unwrap();
    clip(root, ".klip/e.mp4", 5, 10);
    let lib = Library::new(root);
    let mut files = lib.scan();
    files.sort_by(|a, b| a.key.cmp(&b.key));
    assert_eq!(keys(&files), vec!["Game One/b.MP4", "a.mp4"]);
    assert_eq!(files[0].bytes, 20);
    assert!(Library::new(root.join("missing")).scan().is_empty());
}

#[test]
fn stars_round_trip_and_bad_files_are_set_aside() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let a = clip(root, "G/a.mp4", 1, 1);
    let lib = Library::new(root);
    assert_eq!(lib.load_meta(), Meta::default());
    lib.set_starred(&a, true).unwrap();
    assert!(lib.load_meta().clips["G/a.mp4"].starred);
    lib.set_starred(&a, false).unwrap();
    assert!(lib.load_meta().clips.is_empty());
    assert!(
        lib.set_starred(Path::new("/elsewhere/x.mp4"), true)
            .is_err()
    );

    fs::write(lib.meta_path(), "{ not json").unwrap();
    assert_eq!(lib.load_meta(), Meta::default());
    assert!(root.join(".klip/library.json.bad").exists());

    // Hostile keys are dropped.
    fs::write(
        lib.meta_path(),
        r#"{"version":1,"clips":{"../x.mp4":{"starred":true},"C:/y.mp4":{"starred":true},
            "a//b.mp4":{"starred":true},"ok/z.mp4":{"starred":true}}}"#,
    )
    .unwrap();
    let m = lib.load_meta();
    assert_eq!(m.clips.keys().collect::<Vec<_>>(), vec!["ok/z.mp4"]);
}

#[test]
fn over_cap_removes_oldest_unstarred_only() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let lib = Library::new(root);
    clip(root, "G/old.mp4", 100, 500);
    let starred = clip(root, "G/starred-oldest.mp4", 100, 900);
    clip(root, "H/mid.mp4", 100, 300);
    clip(root, "G/new.mp4", 100, 100);
    let just_saved = clip(root, "G/just-saved.mp4", 100, 0);
    lib.set_starred(&starred, true).unwrap();
    let files = lib.scan();
    let meta = lib.load_meta();
    let u = usage(&files, &meta);
    assert_eq!(
        (u.clips, u.bytes, u.starred, u.starred_bytes),
        (5, 500, 1, 100)
    );

    assert!(over_cap(&files, &meta, 0, &[]).is_empty(), "0 = no cap");
    assert!(over_cap(&files, &meta, 500, &[]).is_empty(), "at the cap");
    assert_eq!(keys(&over_cap(&files, &meta, 450, &[])), vec!["G/old.mp4"]);
    assert_eq!(
        keys(&over_cap(&files, &meta, 250, &[])),
        vec!["G/old.mp4", "H/mid.mp4", "G/new.mp4"]
    );
    // Starred and protected clips alone are over the cap: everything else goes.
    let gone = over_cap(&files, &meta, 50, std::slice::from_ref(&just_saved));
    assert_eq!(keys(&gone), vec!["G/old.mp4", "H/mid.mp4", "G/new.mp4"]);

    let mut m = meta.clone();
    assert!(!forget(&mut m, &["G/old.mp4".into()]));
    assert!(forget(&mut m, &["G/starred-oldest.mp4".into()]));
    assert!(m.clips.is_empty());
}

#[test]
fn hostile_meta_sizes_are_capped() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::new(dir.path());
    fs::create_dir_all(dir.path().join(META_DIR)).unwrap();
    let big = vec![b' '; usize::try_from(MAX_META_BYTES).unwrap() + 1];
    fs::write(lib.meta_path(), big).unwrap();
    assert_eq!(lib.load_meta(), Meta::default());
}
