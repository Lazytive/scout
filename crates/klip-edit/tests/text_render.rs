//! Text rasterisation: golden hashes (the same bytes on every machine) and hostile input.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use klip_edit::text::render::{MAX_FRAME_SIDE, RenderError};
use klip_edit::{Position, TextBitmap, TextLayer, measure, render, render_layer, text};
use proptest::prelude::*;

fn fnv(b: &TextBitmap) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for &x in bytes {
            h ^= u64::from(x);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    for v in [b.frame_width, b.frame_height, b.x, b.y, b.width, b.height] {
        feed(&v.to_le_bytes());
    }
    feed(&b.rgba);
    format!("{h:016x}")
}

const SAMPLE: &str = "Clutch 1v4 — gg wp\nno way 🎮";

/// (preset, frame, hash). Rendering changes on purpose → update these and bump
/// `RENDER_VERSION`; otherwise a change here is a regression.
const GOLDEN: &[(&str, (u32, u32), &str)] = &[
    ("bold", (1920, 1080), "452eb5eee18bdd48"),
    ("bold", (1080, 1920), "0c9d554d56eff0ff"),
    ("pill", (1920, 1080), "6d0c3d8ed1bc6935"),
    ("pill", (1080, 1920), "b51ceecb758bb930"),
    ("caption", (1920, 1080), "cca387c31a5c8165"),
    ("caption", (1080, 1920), "3840edc38424ae92"),
    ("title", (1920, 1080), "fc4a6e15334682e8"),
    ("title", (1080, 1920), "814f8715ae65a62a"),
];

/// Composites a bitmap over a grey frame (for looking at the output).
fn preview_png(b: &TextBitmap) -> Vec<u8> {
    let (w, h) = (b.frame_width as usize, b.frame_height as usize);
    let mut frame = vec![0u8; w * h * 4];
    for p in frame.as_chunks_mut::<4>().0 {
        p.copy_from_slice(&[64, 70, 80, 255]);
    }
    for y in 0..b.height as usize {
        for x in 0..b.width as usize {
            let [r, g, bl, a] = b.pixel(x as u32, y as u32).unwrap();
            let i = ((y + b.y as usize) * w + x + b.x as usize) * 4;
            let a = u32::from(a);
            for (k, s) in [r, g, bl].into_iter().enumerate() {
                let d = u32::from(frame[i + k]);
                frame[i + k] = ((u32::from(s) * a + d * (255 - a)) / 255) as u8;
            }
        }
    }
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, b.frame_width, b.frame_height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header().unwrap();
    wr.write_image_data(&frame).unwrap();
    wr.finish().unwrap();
    out
}

#[test]
fn golden_hashes_of_presets_at_two_sizes() {
    let dump = std::env::var_os("KLIP_TEXT_DUMP").map(std::path::PathBuf::from);
    let mut actual = Vec::new();
    for (id, (w, h), _) in GOLDEN {
        let style = text::preset(id).unwrap();
        let b = render(SAMPLE, style, style.position, *w, *h).unwrap();
        assert!(!b.is_blank());
        assert_eq!(
            b,
            render(SAMPLE, style, style.position, *w, *h).unwrap(),
            "deterministic"
        );
        if let Some(dir) = &dump {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join(format!("{id}-{w}x{h}.png")), preview_png(&b)).unwrap();
        }
        actual.push((*id, (*w, *h), fnv(&b)));
    }
    let expected: Vec<(&str, (u32, u32), String)> = GOLDEN
        .iter()
        .map(|(i, s, h)| (*i, *s, h.to_string()))
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn layer_with_unknown_preset_uses_the_default() {
    let layer = TextLayer {
        preset: "nope".into(),
        text: "hi".into(),
        ..TextLayer::default()
    };
    let b = render_layer(&layer, 640, 360).unwrap();
    let d = render(
        "hi",
        text::preset(text::DEFAULT_PRESET).unwrap(),
        Position::Middle,
        640,
        360,
    )
    .unwrap();
    assert_eq!(b, d);
}

fn nasty() -> Vec<String> {
    vec![
        String::new(),
        "\u{0}\u{1}\u{7f}\u{80}\u{9f}".into(),
        "\u{202E}reversed\u{202C} text".into(),
        "مرحبا بالعالم".into(),
        "שלום עולם".into(),
        "🎮🔥👍🏽👨‍👩‍👧‍👦".into(),
        "a\u{200B}b\u{200C}c\u{200D}d\u{FEFF}e".into(),
        "\n\n\n\n\n".into(),
        "﷽﷽﷽﷽﷽﷽﷽﷽﷽﷽".into(),
        "ﬃ ﬄ ĳ Ǆ".into(),
        "ß".repeat(300),
        "W".repeat(200),
        "x\ty\r\nz".into(),
        "\u{E0100}\u{1F3FB}\u{061C}\u{180E}".into(),
    ]
}

#[test]
fn nasty_strings_never_panic() {
    for s in nasty() {
        for style in text::PRESETS {
            for (w, h) in [
                (1, 1),
                (7, 3),
                (64, 64),
                (320, 180),
                (180, 320),
                (1920, 1080),
            ] {
                let b = render(&s, style, Position::Middle, w, h).unwrap();
                assert!(b.x + b.width <= w && b.y + b.height <= h, "{s:?} {w}x{h}");
                let m = measure(&s, style, Position::Middle, w, h).unwrap();
                assert!(
                    m.lines.iter().map(|l| l.chars().count()).sum::<usize>()
                        <= text::MAX_TEXT_CHARS
                );
                assert_eq!(b.rgba.len(), (b.width * b.height * 4) as usize);
                b.png().unwrap();
            }
        }
    }
}

#[test]
fn frame_limits() {
    let bold = text::preset("bold").unwrap();
    assert!(matches!(
        render("x", bold, Position::Top, 0, 1),
        Err(RenderError::Frame(0, 1))
    ));
    assert!(render("x", bold, Position::Top, MAX_FRAME_SIDE, 2).is_ok());
    assert!(render("x", bold, Position::Top, 2, MAX_FRAME_SIDE).is_ok());
}

fn any_position() -> impl Strategy<Value = Position> {
    prop_oneof![
        Just(Position::Top),
        Just(Position::Middle),
        Just(Position::Bottom)
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

    #[test]
    fn any_string_any_small_frame(
        chars in proptest::collection::vec(any::<char>(), 0..260),
        preset_i in 0usize..4,
        pos in any_position(),
        w in 1u32..400,
        h in 1u32..400,
    ) {
        let s: String = chars.into_iter().collect();
        let style = &text::PRESETS[preset_i];
        let b = render(&s, style, pos, w, h).unwrap();
        prop_assert!(b.x + b.width <= w && b.y + b.height <= h);
        prop_assert_eq!(b.rgba.len(), (b.width * b.height * 4) as usize);
        let m = measure(&s, style, pos, w, h).unwrap();
        prop_assert!(m.lines.len() <= text::MAX_TEXT_LINES);
        prop_assert!(m.font_px >= 1.0);
        prop_assert!(m.lines.iter().all(|l| !l.starts_with(' ') && !l.ends_with(' ')));
        prop_assert!(m.lines.iter().all(|l| l.chars().all(|c| !c.is_control())));
        prop_assert!(m.lines.iter().map(|l| l.chars().count()).sum::<usize>() <= text::MAX_TEXT_CHARS);
    }

    #[test]
    fn any_bytes_as_text(bytes in proptest::collection::vec(any::<u8>(), 0..300)) {
        let s = String::from_utf8_lossy(&bytes);
        let b = render(&s, text::preset("pill").unwrap(), Position::Bottom, 320, 180).unwrap();
        prop_assert!(b.x + b.width <= 320 && b.y + b.height <= 180);
    }
}

/// `cargo test -p klip-edit --release -- --ignored timing --nocapture`: how long one
/// layer takes to render (numbers for ROADMAP.md).
#[test]
#[ignore]
fn timing() {
    for (w, h) in [(1920, 1080), (1080, 1920), (3840, 2160)] {
        for style in text::PRESETS {
            let t = std::time::Instant::now();
            let n = 5;
            let mut bytes = 0;
            for _ in 0..n {
                let b = render(SAMPLE, style, style.position, w, h).unwrap();
                bytes = b.png().unwrap().len();
            }
            let ms = t.elapsed().as_secs_f64() * 1000.0 / f64::from(n);
            println!(
                "timing: {:<8} {w}x{h}: {ms:.1} ms per render incl. PNG ({bytes} B)",
                style.id
            );
        }
    }
}
