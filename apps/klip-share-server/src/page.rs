//! The player page `/c/{id}`: a `<video>` and the Open Graph tags Discord (and other
//! chat apps) read to play the clip inline. No script; everything shown is escaped.

use crate::store::Meta;

/// `s` safe inside HTML text and double-quoted attributes.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// The page for `meta`; `base` is the public URL without a trailing `/`.
pub fn render(meta: &Meta, base: &str, site: &str) -> String {
    let page = format!("{base}{}/{}", klip_share::PATH_PUBLIC, meta.id);
    let video = format!("{page}.mp4");
    let poster = meta.poster.as_ref().map(|ext| format!("{page}.{ext}"));
    let title = if meta.title.is_empty() {
        "Clip".to_string()
    } else {
        meta.title.clone()
    };
    let (t, s, p, v) = (escape(&title), escape(site), escape(&page), escape(&video));
    let secure = if video.starts_with("https://") {
        format!("\n<meta property=\"og:video:secure_url\" content=\"{v}\">")
    } else {
        String::new()
    };
    let (og_image, poster_attr) = match &poster {
        Some(url) => {
            let u = escape(url);
            (
                format!("\n<meta property=\"og:image\" content=\"{u}\">"),
                format!(" poster=\"{u}\""),
            )
        }
        None => (String::new(), String::new()),
    };
    let (w, h) = (meta.width, meta.height);
    format!(
        r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="robots" content="noindex, nofollow">
<title>{t} · {s}</title>
<meta property="og:site_name" content="{s}">
<meta property="og:title" content="{t}">
<meta property="og:type" content="video.other">
<meta property="og:url" content="{p}">
<meta property="og:video" content="{v}">
<meta property="og:video:url" content="{v}">{secure}
<meta property="og:video:type" content="video/mp4">
<meta property="og:video:width" content="{w}">
<meta property="og:video:height" content="{h}">{og_image}
<meta name="twitter:card" content="summary_large_image">
<meta name="theme-color" content="#2d8cf0">
<style>
html,body{{margin:0;height:100%;background:#111;color:#ddd;font:15px/1.4 system-ui,sans-serif}}
main{{min-height:100%;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:10px;padding:12px;box-sizing:border-box}}
video{{max-width:100%;max-height:85vh;background:#000;border-radius:6px}}
p{{margin:0;display:flex;gap:16px;align-items:baseline;flex-wrap:wrap;justify-content:center}}
a{{color:#8ab4f8}}
</style>
</head>
<body>
<main>
<video src="{v}"{poster_attr} controls playsinline preload="metadata" width="{w}" height="{h}"></video>
<p><span>{t}</span><a href="{v}" download>Download</a></p>
</main>
</body>
</html>
"##
    )
}

/// `/`: says what this is, links nowhere.
pub fn home(site: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"robots\" content=\"noindex\"><title>{s}</title></head><body style=\"background:#111;color:#ddd;font:15px system-ui;padding:24px\"><p>{s}: a Scout share server. Clips here are only reachable by their link.</p></body></html>",
        s = escape(site)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> Meta {
        Meta {
            id: "0123456789abcdef0123456789abcdef".into(),
            title: "Ace <script>alert(1)</script> \"round\"".into(),
            created_unix: 1,
            expires_unix: 0,
            bytes: 1,
            width: 1920,
            height: 1080,
            duration_ms: 1,
            codec: "H.264".into(),
            poster: Some("jpg".into()),
            manage_hash: String::new(),
            uploader: String::new(),
        }
    }

    #[test]
    fn the_page_has_absolute_open_graph_video_tags() {
        let html = render(&meta(), "https://clips.example.com", "Scout");
        let v = "https://clips.example.com/c/0123456789abcdef0123456789abcdef.mp4";
        assert!(html.contains(&format!("<meta property=\"og:video\" content=\"{v}\">")));
        assert!(html.contains(&format!("og:video:secure_url\" content=\"{v}\"")));
        assert!(html.contains("og:video:width\" content=\"1920\""));
        assert!(html.contains(
            "og:image\" content=\"https://clips.example.com/c/0123456789abcdef0123456789abcdef.jpg\""
        ));
        assert!(!html.contains("<script>"), "the title is escaped");
        assert!(html.contains("Ace &lt;script&gt;alert(1)&lt;/script&gt; &quot;round&quot;"));
        let plain = render(&meta(), "http://box:8080", "Scout");
        assert!(!plain.contains("secure_url"));
    }
}
