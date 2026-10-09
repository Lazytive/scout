//! M7 tests: names and tags, rename, the info index, queries, the thumbnail cache.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use klip_media::{Codec, Colorimetry, NANOS_PER_SEC, Packet, TrackInfo, TrackParams, VideoParams};
use klip_mp4::{Marker, Mp4Options, Mp4Track};
use proptest::prelude::*;

use super::*;

fn set_age(path: &Path, age_secs: u64) {
    let f = File::options().write(true).open(path).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(age_secs))
        .unwrap();
}

/// A real (tiny) MP4: `frames` frames of 60 fps H.264-tagged video, created at `created`.
fn mp4(root: &Path, rel: &str, frames: usize, created: u64, bookmarks: &[i64]) -> PathBuf {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let info = TrackInfo {
        codec: Codec::H264,
        codec_data: Bytes::from_static(&[1, 0x64, 0, 0x28, 0xFF, 0xE1, 0, 0]),
        params: TrackParams::Video(VideoParams {
            width: 1920,
            height: 1080,
            colorimetry: Some(Colorimetry::BT709),
        }),
        bitrate: None,
    };
    let frame = NANOS_PER_SEC / 60;
    let packets: Vec<Packet> = (0..frames)
        .map(|i| Packet {
            pts: i as i64 * frame,
            dts: i as i64 * frame,
            duration: frame,
            keyframe: i % 60 == 0,
            data: Bytes::from(vec![0u8; 100]),
        })
        .collect();
    let markers: Vec<Marker> = bookmarks
        .iter()
        .map(|ms| Marker {
            at: ms * 1_000_000,
            title: "Bookmark".into(),
        })
        .collect();
    klip_mp4::save_mp4_atomic(
        &path,
        &[Mp4Track::new(&info, &packets)],
        &Mp4Options {
            creation_time_unix: created,
            markers: &markers,
            ..Default::default()
        },
    )
    .unwrap();
    path
}

fn now_ms() -> u64 {
    crate::index::ms_since_epoch(SystemTime::now())
}

#[test]
fn clip_names() {
    assert_eq!(check_clip_name("  Ace clutch  ").unwrap(), "Ace clutch");
    assert_eq!(check_clip_name("Ace.mp4").unwrap(), "Ace");
    assert_eq!(check_clip_name("v1.2 final").unwrap(), "v1.2 final");
    assert_eq!(
        check_clip_name("日本語のクリップ").unwrap(),
        "日本語のクリップ"
    );
    for bad in [
        "",
        "   ",
        ".mp4",
        "a/b",
        "a\\b",
        "a:b",
        "what?",
        "a*",
        "<x>",
        "a|b",
        "\"q\"",
        "tab\there",
        ".hidden",
        "dots...",
        "CON",
        "con.txt",
        "COM1",
        "lpt9",
        "NUL ",
    ] {
        assert!(check_clip_name(bad).is_err(), "{bad:?} should be refused");
    }
    assert!(check_clip_name(&"x".repeat(MAX_NAME_CHARS)).is_ok());
    assert!(check_clip_name(&"x".repeat(MAX_NAME_CHARS + 1)).is_err());
    // COM10 and CONSOLE are fine.
    assert!(check_clip_name("COM10").is_ok());
    assert!(check_clip_name("CONSOLE").is_ok());
}

#[test]
fn tags_are_cleaned_and_deduplicated() {
    let t = |v: &[&str]| normalize_tags(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    assert_eq!(
        t(&["  ace ", "Ace", "clutch\tround", "", "  ", "ACE"]).unwrap(),
        vec!["ace", "clutch round"]
    );
    assert!(t(&[&"x".repeat(MAX_TAG_CHARS + 1)]).is_err());
    let many: Vec<String> = (0..=MAX_TAGS).map(|i| format!("t{i}")).collect();
    assert!(normalize_tags(&many).is_err());
    assert_eq!(normalize_tags(&many[..MAX_TAGS]).unwrap().len(), MAX_TAGS);
}

proptest! {
    #[test]
    fn names_and_tags_never_panic(s in "\\PC{0,200}", v in proptest::collection::vec("\\PC{0,50}", 0..80)) {
        if let Ok(n) = check_clip_name(&s) {
            prop_assert!(!n.contains('/') && !n.contains('\\') && !n.is_empty());
        }
        if let Ok(tags) = normalize_tags(&v) {
            prop_assert!(tags.len() <= MAX_TAGS);
            prop_assert!(tags.iter().all(|t| !t.is_empty() && t.chars().count() <= MAX_TAG_CHARS));
        }
    }
}

#[test]
fn tags_round_trip_and_hostile_tags_are_cleaned() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let a = mp4(root, "G/a.mp4", 10, 0, &[]);
    let lib = Library::new(root);
    let tags = lib
        .set_tags(&a, &["Ace".into(), " clutch ".into(), "ace".into()])
        .unwrap();
    assert_eq!(tags, vec!["Ace", "clutch"]);
    assert_eq!(lib.load_meta().clips["G/a.mp4"].tags, tags);
    lib.set_starred(&a, true).unwrap();
    lib.set_tags(&a, &[]).unwrap();
    assert!(lib.load_meta().clips["G/a.mp4"].starred, "stars stay");
    lib.set_starred(&a, false).unwrap();
    assert!(lib.load_meta().clips.is_empty(), "empty entries go");
    assert!(lib.set_tags(&a, &["x".repeat(500)]).is_err());

    let many: Vec<String> = (0..1000).map(|i| format!("\"t{i}\"")).collect();
    fs::write(
        lib.meta_path(),
        format!(
            r#"{{"version":1,"clips":{{"G/a.mp4":{{"tags":["  a\u0007b ","A B",{}]}}}}}}"#,
            many.join(",")
        ),
    )
    .unwrap();
    let m = lib.load_meta();
    let tags = &m.clips["G/a.mp4"].tags;
    assert_eq!(tags.len(), MAX_TAGS);
    assert_eq!(tags[0], "a b");
}

#[test]
fn rename_moves_metadata_and_thumbs_and_never_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let lib = Library::new(root);
    let a = mp4(root, "G/a.mp4", 10, 0, &[]);
    let b = mp4(root, "G/b.mp4", 20, 0, &[]);
    lib.set_starred(&a, true).unwrap();
    lib.set_tags(&a, &["ace".into()]).unwrap();
    let poster = lib.thumb_path("G/a.mp4", ThumbKind::Poster).unwrap();
    fs::create_dir_all(poster.parent().unwrap()).unwrap();
    fs::write(&poster, b"jpeg").unwrap();
    let before = fs::read(&a).unwrap();

    let new = lib.rename(&a, "Ace clutch").unwrap();
    assert_eq!(new, root.join("G/Ace clutch.mp4"));
    assert!(!a.exists());
    assert_eq!(fs::read(&new).unwrap(), before, "content untouched");
    let m = lib.load_meta();
    assert!(m.clips["G/Ace clutch.mp4"].starred);
    assert_eq!(m.clips["G/Ace clutch.mp4"].tags, vec!["ace"]);
    assert!(!m.clips.contains_key("G/a.mp4"));
    assert!(!poster.exists());
    assert!(
        lib.thumb_path("G/Ace clutch.mp4", ThumbKind::Poster)
            .unwrap()
            .exists()
    );

    // Never replaces another clip.
    assert!(matches!(
        lib.rename(&new, "b"),
        Err(LibraryError::Exists(_))
    ));
    assert!(b.exists() && new.exists());
    // Bad names, missing clips, recordings in progress.
    assert!(matches!(
        lib.rename(&new, "a/b"),
        Err(LibraryError::BadName(_))
    ));
    assert!(matches!(
        lib.rename(&root.join("G/none.mp4"), "x"),
        Err(LibraryError::NoClip)
    ));
    assert!(lib.rename(Path::new("/elsewhere/x.mp4"), "x").is_err());
    let rec = mp4(root, "G/rec.mp4", 10, 0, &[]);
    fs::write(root.join("G/rec.mp4.recording.json"), "{}").unwrap();
    assert!(matches!(
        lib.rename(&rec, "y"),
        Err(LibraryError::Recording)
    ));
    // Same name: nothing to do.
    assert_eq!(lib.rename(&new, "Ace clutch").unwrap(), new);
}

#[cfg(windows)]
#[test]
fn case_only_rename_works_on_windows() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let lib = Library::new(root);
    let a = mp4(root, "ace.mp4", 10, 0, &[]);
    let new = lib.rename(&a, "Ace").unwrap();
    let names: Vec<String> = fs::read_dir(root)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".mp4"))
        .collect();
    assert_eq!(names, vec!["Ace.mp4"]);
    assert!(new.exists());
}

#[test]
fn index_probes_new_and_changed_clips_only() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let lib = Library::new(root);
    let a = mp4(root, "G/a.mp4", 120, 1_760_000_000, &[500]);
    fs::write(root.join("broken.mp4"), b"not an mp4 at all").unwrap();
    let mut ix = lib.load_index();
    assert!(lib.refresh_index(&lib.scan(), &mut ix));
    let e = &ix.clips["G/a.mp4"];
    let info = e.info.as_ref().unwrap();
    assert_eq!(info.duration_ms, Some(2000));
    assert_eq!((info.width, info.height), (1920, 1080));
    assert_eq!(info.codec.as_deref(), Some("H.264"));
    assert!((info.fps.unwrap() - 60.0).abs() < 0.01);
    assert_eq!(info.bookmarks_ms, vec![500]);
    assert_eq!(info.created_unix, 1_760_000_000);
    let broken = &ix.clips["broken.mp4"];
    assert!(broken.info.is_none() && broken.error.is_some());

    lib.save_index(&ix).unwrap();
    let mut again = lib.load_index();
    assert_eq!(again, ix);
    assert!(!lib.refresh_index(&lib.scan(), &mut again), "nothing new");

    // A changed file is read again; a removed one is dropped.
    set_age(&a, 10);
    fs::remove_file(root.join("broken.mp4")).unwrap();
    assert!(lib.refresh_index(&lib.scan(), &mut again));
    assert_eq!(again.clips.keys().collect::<Vec<_>>(), vec!["G/a.mp4"]);
}

#[test]
fn hostile_index_files_are_rebuilt() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::new(dir.path());
    fs::create_dir_all(dir.path().join(META_DIR)).unwrap();
    for bad in ["", "{", "[]", r#"{"version":99,"clips":{}}"#] {
        fs::write(lib.index_path(), bad).unwrap();
        assert_eq!(lib.load_index(), Index::default(), "{bad}");
    }
    let long = "c".repeat(100);
    fs::write(
        lib.index_path(),
        format!(
            r#"{{"version":1,"clips":{{"../x.mp4":{{}},"a.mp4":{{"bytes":1,"info":{{"codec":"{long}","fps":1e300,"bookmarks_ms":[{}]}}}}}}}}"#,
            vec!["1"; 1000].join(",")
        ),
    )
    .unwrap();
    let ix = lib.load_index();
    assert_eq!(ix.clips.len(), 1);
    let i = ix.clips["a.mp4"].info.as_ref().unwrap();
    assert!(i.codec.as_ref().unwrap().len() <= 16);
    assert!(i.fps.is_none());
    assert_eq!(i.bookmarks_ms.len(), 255);
}

struct Fixture {
    _dir: tempfile::TempDir,
    lib: Library,
}

/// Four clips: two in "Apex", one in "CS2", one loose; one starred, tags on two.
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let lib = Library::new(&root);
    let day = 86_400u64;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let a = mp4(&root, "Apex/Klip_a.mp4", 120, now - 3 * day, &[]);
    let b = mp4(&root, "Apex/Ace clutch.mp4", 600, now - day, &[]);
    mp4(&root, "CS2/Klip_c.mp4", 300, now - 10 * day, &[]);
    // No believable creation time: the file's time counts.
    let d = mp4(&root, "loose.mp4", 60, 0, &[]);
    set_age(&d, 3600);
    lib.set_starred(&b, true).unwrap();
    lib.set_tags(&b, &["Ace".into(), "ranked".into()]).unwrap();
    lib.set_tags(&a, &["ranked".into()]).unwrap();
    Fixture { _dir: dir, lib }
}

fn run(f: &Fixture, q: Query) -> Listing {
    let files = f.lib.scan();
    let mut ix = Index::default();
    f.lib.refresh_index(&files, &mut ix);
    query(
        &files,
        &f.lib.load_meta(),
        &ix,
        &f.lib.thumb_versions(),
        &q,
        now_ms(),
    )
}

fn names(l: &Listing) -> Vec<&str> {
    l.clips.iter().map(|c| c.name.as_str()).collect()
}

#[test]
fn query_sorts_filters_and_counts() {
    let f = fixture();
    let all = run(&f, Query::default());
    assert_eq!(names(&all), vec!["loose", "Ace clutch", "Klip_a", "Klip_c"]);
    assert_eq!((all.total, all.matched, all.starred), (4, 4, 1));
    let games: Vec<(&str, usize)> = all
        .games
        .iter()
        .map(|g| (g.name.as_str(), g.count))
        .collect();
    assert_eq!(games, vec![("", 1), ("Apex", 2), ("CS2", 1)]);
    let tags: Vec<(&str, usize)> = all
        .tags
        .iter()
        .map(|g| (g.name.as_str(), g.count))
        .collect();
    assert_eq!(tags, vec![("ranked", 2), ("Ace", 1)]);
    let ace = &all.clips[1];
    assert_eq!(ace.game.as_deref(), Some("Apex"));
    assert!(ace.starred);
    assert_eq!(ace.info.as_ref().unwrap().duration_ms, Some(10_000));

    let q = |q: Query| {
        names(&run(&f, q))
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        q(Query {
            sort: Sort::Oldest,
            ..Default::default()
        }),
        ["Klip_c", "Klip_a", "Ace clutch", "loose"]
    );
    assert_eq!(
        q(Query {
            sort: Sort::Longest,
            ..Default::default()
        }),
        ["Ace clutch", "Klip_c", "Klip_a", "loose"]
    );
    assert_eq!(
        q(Query {
            sort: Sort::Shortest,
            ..Default::default()
        })[0],
        "loose"
    );
    assert_eq!(
        q(Query {
            sort: Sort::Name,
            ..Default::default()
        }),
        ["Ace clutch", "Klip_a", "Klip_c", "loose"]
    );
    assert_eq!(
        q(Query {
            game: Some("Apex".into()),
            ..Default::default()
        }),
        ["Ace clutch", "Klip_a"]
    );
    assert_eq!(
        q(Query {
            game: Some(String::new()),
            ..Default::default()
        }),
        ["loose"]
    );
    assert_eq!(
        q(Query {
            starred: true,
            ..Default::default()
        }),
        ["Ace clutch"]
    );
    assert_eq!(
        q(Query {
            tag: Some("RANKED".into()),
            ..Default::default()
        }),
        ["Ace clutch", "Klip_a"]
    );
    // Every word must match the name, game or a tag, in any case.
    assert_eq!(
        q(Query {
            search: "apex ACE".into(),
            ..Default::default()
        }),
        ["Ace clutch"]
    );
    assert_eq!(
        q(Query {
            search: "ranked".into(),
            ..Default::default()
        }),
        ["Ace clutch", "Klip_a"]
    );
    assert_eq!(
        q(Query {
            search: "cs2".into(),
            ..Default::default()
        }),
        ["Klip_c"]
    );
    assert!(
        q(Query {
            search: "nothing".into(),
            ..Default::default()
        })
        .is_empty()
    );
    let two_days = now_ms() - 2 * 86_400_000;
    assert_eq!(
        q(Query {
            since_ms: Some(two_days),
            ..Default::default()
        }),
        ["loose", "Ace clutch"]
    );
    let limited = run(
        &f,
        Query {
            limit: Some(1),
            ..Default::default()
        },
    );
    assert_eq!((limited.clips.len(), limited.matched), (1, 4));
}

#[test]
fn query_args_are_strict() {
    assert!(serde_json::from_str::<Query>(r#"{"search":"x","sort":"longest"}"#).is_ok());
    assert!(serde_json::from_str::<Query>(r#"{"bogus":1}"#).is_err());
    assert!(serde_json::from_str::<Query>(r#"{"sort":"sideways"}"#).is_err());
}

#[test]
fn thumb_versions_and_pruning() {
    let f = fixture();
    let lib = &f.lib;
    let live = lib.thumb_path("Apex/Klip_a.mp4", ThumbKind::Strip).unwrap();
    let gone = lib
        .thumb_path("Apex/deleted.mp4", ThumbKind::Poster)
        .unwrap();
    let tmp = lib.thumbs_dir().join("x.jpg.tmp");
    fs::create_dir_all(lib.thumbs_dir()).unwrap();
    for p in [&live, &gone, &tmp] {
        fs::write(p, b"jpeg").unwrap();
    }
    fs::write(lib.thumbs_dir().join("notes.txt"), b"keep").unwrap();
    let v: HashMap<String, ThumbVersions> = lib.thumb_versions();
    assert!(v["Apex/Klip_a.mp4"].strip.is_some());
    assert!(v["Apex/Klip_a.mp4"].poster.is_none());
    let files = lib.scan();
    let keys: HashSet<&str> = files.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(lib.prune_thumbs(&keys), 1);
    assert!(live.exists() && !gone.exists());
    assert!(
        tmp.exists(),
        "a fresh temp file may belong to a running job"
    );
    lib.remove_thumbs("Apex/Klip_a.mp4");
    assert!(!live.exists());
    assert!(lib.thumb_path("../x.mp4", ThumbKind::Poster).is_none());
}
