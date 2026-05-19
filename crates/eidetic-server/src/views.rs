//! Maud HTML helpers for the server.

use chrono::{DateTime, Utc};
use eidetic_core::{AssetId, Sha256};
use maud::{DOCTYPE, Markup, html};

/// One tile in a thumbnail grid. Used by `/` and `/search`.
pub(crate) struct GridTile {
    pub(crate) id: AssetId,
    pub(crate) hash: Sha256,
    pub(crate) alt: String,
    /// Optional cosine-similarity score [0, 1]. Some(score) means render a badge.
    pub(crate) score: Option<f32>,
}

/// Detail-page input. Mirrors `eidetic_db::AssetDetail`'s fields the page
/// actually displays.
#[allow(dead_code)]
pub(crate) struct DetailView {
    pub(crate) id: AssetId,
    pub(crate) hash: Sha256,
    pub(crate) original_filename: String,
    pub(crate) mime_type: Option<String>,
    pub(crate) file_size: i64,
    pub(crate) imported_at: DateTime<Utc>,
    pub(crate) date_taken: Option<DateTime<Utc>>,
    pub(crate) latitude: Option<f64>,
    pub(crate) longitude: Option<f64>,
    pub(crate) camera_make: Option<String>,
    pub(crate) camera_model: Option<String>,
    pub(crate) thumbnails_generated: bool,
}

const INLINE_CSS: &str = r#"
* { box-sizing: border-box; }
body { font-family: -apple-system, system-ui, sans-serif; margin: 0; background: #fafafa; color: #111; }
header { padding: 1rem 1.5rem; background: white; border-bottom: 1px solid #e5e5e5; }
header a { color: #111; text-decoration: none; font-weight: 600; }
main { padding: 1.5rem; max-width: 1400px; margin: 0 auto; }
form.search { display: flex; gap: 0.5rem; margin-bottom: 1.5rem; }
form.search input[type=text] { flex: 1; padding: 0.6rem 0.8rem; border: 1px solid #ccc; border-radius: 4px; font-size: 1rem; }
form.search button { padding: 0.6rem 1rem; background: #111; color: white; border: 0; border-radius: 4px; cursor: pointer; }
h2 { margin: 1.5rem 0 1rem; font-weight: 600; font-size: 1.1rem; color: #555; }
.grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(180px, 1fr)); gap: 0.6rem; }
.tile { position: relative; aspect-ratio: 1 / 1; overflow: hidden; border-radius: 4px; background: #eee; }
.tile img { width: 100%; height: 100%; object-fit: cover; display: block; }
.tile .score { position: absolute; bottom: 0.3rem; right: 0.3rem; background: rgba(0,0,0,0.7); color: white; padding: 0.1rem 0.4rem; border-radius: 3px; font-size: 0.75rem; }
.detail { display: grid; grid-template-columns: 2fr 1fr; gap: 2rem; }
.detail img.preview { width: 100%; border-radius: 4px; background: #eee; }
.detail dl { margin: 0; }
.detail dt { font-size: 0.8rem; color: #777; margin-top: 0.8rem; }
.detail dd { margin: 0.2rem 0 0 0; }
.detail a.download { display: inline-block; margin-top: 1rem; padding: 0.6rem 1rem; background: #111; color: white; text-decoration: none; border-radius: 4px; }
.empty { color: #777; font-style: italic; }
"#;

pub(crate) fn layout(title: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) " · Eidetic" }
                style { (INLINE_CSS) }
            }
            body {
                header { a href="/" { "Eidetic" } }
                main { (body) }
            }
        }
    }
}

pub(crate) fn search_form(default_q: &str) -> Markup {
    html! {
        form class="search" action="/search" method="get" {
            input type="text" name="q" placeholder="Search photos…" value=(default_q);
            button type="submit" { "Search" }
        }
    }
}

pub(crate) fn asset_grid(tiles: &[GridTile]) -> Markup {
    if tiles.is_empty() {
        return html! { p class="empty" { "No results." } };
    }
    html! {
        div class="grid" {
            @for tile in tiles {
                a href=(format!("/assets/{}", tile.id)) class="tile" {
                    img src=(format!("/thumbs/m/{}", tile.hash)) loading="lazy" alt=(tile.alt);
                    @if let Some(score) = tile.score {
                        span class="score" { (format!("{:.0}%", score * 100.0)) }
                    }
                }
            }
        }
    }
}

#[allow(dead_code)]
pub(crate) fn detail_page(view: &DetailView) -> Markup {
    html! {
        div class="detail" {
            div {
                @if view.thumbnails_generated {
                    img class="preview"
                        src=(format!("/thumbs/m/{}", view.hash))
                        alt=(view.original_filename);
                } @else {
                    p class="empty" {
                        "Thumbnails pending — run `eidetic thumbnail` to generate."
                    }
                }
                a class="download" href=(format!("/assets/{}/raw", view.id)) {
                    "Download original"
                }
            }
            div {
                dl {
                    dt { "Filename" } dd { (view.original_filename) }
                    @if let Some(mime) = &view.mime_type {
                        dt { "Type" } dd { (mime) }
                    }
                    dt { "Size" } dd { (format_bytes(view.file_size)) }
                    dt { "Imported" } dd { (view.imported_at.format("%Y-%m-%d %H:%M").to_string()) }
                    @if let Some(taken) = view.date_taken {
                        dt { "Taken" } dd { (taken.format("%Y-%m-%d %H:%M").to_string()) }
                    }
                    @if let (Some(make), Some(model)) = (&view.camera_make, &view.camera_model) {
                        dt { "Camera" } dd { (make) " " (model) }
                    } @else if let Some(make) = &view.camera_make {
                        dt { "Camera" } dd { (make) }
                    }
                    @if let (Some(lat), Some(lon)) = (view.latitude, view.longitude) {
                        dt { "GPS" } dd { (format!("{lat:.4}, {lon:.4}")) }
                    }
                }
            }
        }
    }
}

#[allow(dead_code)]
fn format_bytes(n: i64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let n = n as f64;
    if n >= GB {
        format!("{:.2} GB", n / GB)
    } else if n >= MB {
        format!("{:.1} MB", n / MB)
    } else if n >= KB {
        format!("{:.0} KB", n / KB)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample_hash() -> Sha256 {
        Sha256::from_hex("0000000000000000000000000000000000000000000000000000000000000000")
            .expect("valid hex")
    }

    #[test]
    fn layout_renders_title_and_html_skeleton() {
        let m = layout("Test", html! { p { "hello" } });
        let s = m.into_string();
        assert!(s.contains("<!DOCTYPE html>"));
        assert!(s.contains("<title>Test · Eidetic</title>"));
        assert!(s.contains("<p>hello</p>"));
    }

    #[test]
    fn asset_grid_empty_renders_empty_message() {
        let s = asset_grid(&[]).into_string();
        assert!(s.contains("No results."));
        assert!(!s.contains("<div class=\"grid\""));
    }

    #[test]
    fn asset_grid_renders_one_tile_per_input() {
        let tiles = vec![
            GridTile {
                id: AssetId::new(),
                hash: sample_hash(),
                alt: "first.jpg".into(),
                score: None,
            },
            GridTile {
                id: AssetId::new(),
                hash: sample_hash(),
                alt: "second.jpg".into(),
                score: Some(0.87),
            },
        ];
        let s = asset_grid(&tiles).into_string();
        let tile_count = s.matches("class=\"tile\"").count();
        assert_eq!(tile_count, 2);
        assert!(s.contains("87%"));
    }

    #[test]
    fn detail_page_with_thumbnails_renders_preview() {
        let view = DetailView {
            id: AssetId::new(),
            hash: sample_hash(),
            original_filename: "foo.jpg".to_string(),
            mime_type: Some("image/jpeg".to_string()),
            file_size: 2048,
            imported_at: Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap(),
            date_taken: None,
            latitude: None,
            longitude: None,
            camera_make: None,
            camera_model: None,
            thumbnails_generated: true,
        };
        let s = detail_page(&view).into_string();
        assert!(s.contains("class=\"preview\""));
        assert!(s.contains("Download original"));
        assert!(s.contains("2 KB"));
        assert!(!s.contains("Thumbnails pending"));
    }

    #[test]
    fn detail_page_without_thumbnails_shows_pending_notice() {
        let view = DetailView {
            id: AssetId::new(),
            hash: sample_hash(),
            original_filename: "foo.jpg".to_string(),
            mime_type: None,
            file_size: 0,
            imported_at: Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap(),
            date_taken: None,
            latitude: None,
            longitude: None,
            camera_make: None,
            camera_model: None,
            thumbnails_generated: false,
        };
        let s = detail_page(&view).into_string();
        assert!(s.contains("Thumbnails pending"));
        assert!(!s.contains("class=\"preview\""));
    }
}
