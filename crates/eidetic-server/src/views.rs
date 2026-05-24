use chrono::{DateTime, Utc};
use eidetic_core::{AssetId, Sha256};
use maud::{DOCTYPE, Markup, html};

pub(crate) struct GridTile {
    pub(crate) id: AssetId,
    pub(crate) hash: Sha256,
    pub(crate) alt: String,
    /// Cosine similarity [0, 1]. Some(_) renders the score badge.
    pub(crate) score: Option<f32>,
    pub(crate) mime_type: String,
    pub(crate) thumbnails_generated: bool,
    pub(crate) file_size: i64,
}

/// Max bytes we're willing to embed inline as `<img src="/raw">`.
/// Above this, render a placeholder so a single grid tile doesn't drag
/// in 50 MB of bandwidth.
const MAX_INLINE_BYTES: i64 = 25 * 1024 * 1024; // 25 MiB

/// The MIME allowlist is deliberately conservative. HEIC works on Safari
/// only, and DNG/ARW/CR3 don't work anywhere. For those, plus oversized
/// files, show a placeholder rather than a broken-image icon or a runaway
/// download.
fn should_render_inline(mime: &str, file_size: i64) -> bool {
    let renderable = matches!(
        mime,
        "image/jpeg"
            | "image/png"
            | "image/gif"
            | "image/webp"
            | "image/avif"
            | "image/bmp"
            | "image/svg+xml"
    );
    renderable && file_size <= MAX_INLINE_BYTES
}

fn ext_badge(filename: &str) -> String {
    std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_uppercase())
        .unwrap_or_else(|| "FILE".to_string())
}

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
    pub(crate) lens_model: Option<String>,
    pub(crate) focal_length: Option<f32>,
    pub(crate) focal_length_35mm: Option<f32>,
    pub(crate) aperture: Option<f32>,
    pub(crate) shutter: Option<String>,
    pub(crate) iso: Option<i32>,
    pub(crate) altitude: Option<f64>,
    pub(crate) country_name: Option<String>,
    pub(crate) admin1: Option<String>,
    pub(crate) place: Option<String>,
    pub(crate) place_distance_m: Option<f32>,
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
.tile-placeholder { width: 100%; height: 100%; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 0.4rem; padding: 0.5rem; background: linear-gradient(135deg, #e8e8e8 0%, #d6d6d6 100%); }
.tile-placeholder .ext-badge { background: #111; color: white; padding: 0.2rem 0.6rem; border-radius: 3px; font-size: 0.75rem; font-weight: 600; letter-spacing: 0.05em; }
.tile-placeholder .filename { font-size: 0.7rem; color: #555; text-align: center; word-break: break-all; overflow: hidden; display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical; }
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
                    @if tile.thumbnails_generated {
                        img src=(format!("/thumbs/m/{}", tile.hash)) loading="lazy" alt=(tile.alt);
                    } @else if should_render_inline(&tile.mime_type, tile.file_size) {
                        // Browser-renderable image without a generated thumbnail: serve
                        // the original. Size is bounded by MAX_INLINE_BYTES.
                        img src=(format!("/assets/{}/raw", tile.id)) loading="lazy" alt=(tile.alt);
                    } @else {
                        // Video, RAW, oversized image, or HEIC-without-thumbnail.
                        // Placeholder with extension badge + filename. Detail page
                        // has the download link.
                        div class="tile-placeholder" {
                            span class="ext-badge" { (ext_badge(&tile.alt)) }
                            span class="filename" { (tile.alt) }
                        }
                    }
                    @if let Some(score) = tile.score {
                        span class="score" { (format!("{:.0}%", score * 100.0)) }
                    }
                }
            }
        }
    }
}

pub(crate) fn detail_page(view: &DetailView) -> Markup {
    let render_original_inline = view
        .mime_type
        .as_deref()
        .is_some_and(|m| should_render_inline(m, view.file_size));
    html! {
        div class="detail" {
            div {
                @if view.thumbnails_generated {
                    img class="preview"
                        src=(format!("/thumbs/m/{}", view.hash))
                        alt=(view.original_filename);
                } @else if render_original_inline {
                    img class="preview"
                        src=(format!("/assets/{}/raw", view.id))
                        alt=(view.original_filename);
                } @else {
                    p class="empty" {
                        "No preview available. This is "
                        (view.mime_type.as_deref().unwrap_or("an unknown type"))
                        " (" (format_bytes(view.file_size)) "). Use the download link below."
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
                    @if let Some(lens) = &view.lens_model {
                        dt { "Lens" } dd { (lens) }
                    }
                    @if let Some(f) = view.focal_length {
                        dt { "Focal length" }
                        dd {
                            (format!("{f:.1} mm"))
                            @if let Some(eq) = view.focal_length_35mm {
                                " (" (format!("{eq:.0}")) " mm equiv.)"
                            }
                        }
                    }
                    @if let Some(a) = view.aperture {
                        dt { "Aperture" } dd { (format!("f/{a:.1}")) }
                    }
                    @if let Some(s) = &view.shutter {
                        dt { "Shutter" } dd { (s) " s" }
                    }
                    @if let Some(iso) = view.iso {
                        dt { "ISO" } dd { (iso) }
                    }
                    @if let Some(place) = &view.place {
                        dt { "Place" }
                        dd {
                            (place)
                            @if let Some(a) = &view.admin1 { ", " (a) }
                            @if let Some(c) = &view.country_name { ", " (c) }
                            @if let Some(d) = view.place_distance_m {
                                " (" (format_distance(d)) ")"
                            }
                        }
                    }
                    @if let (Some(lat), Some(lon)) = (view.latitude, view.longitude) {
                        dt { "GPS" }
                        dd {
                            (format!("{lat:.4}, {lon:.4}"))
                            @if let Some(alt) = view.altitude {
                                " (" (format!("{alt:.0}")) " m)"
                            }
                        }
                    }
                }
            }
        }
    }
}

fn format_distance(m: f32) -> String {
    if m >= 1_000.0 {
        format!("{:.1} km", m / 1_000.0)
    } else {
        format!("{:.0} m", m)
    }
}

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

    fn tile(alt: &str, mime: &str, thumbs: bool, size: i64, score: Option<f32>) -> GridTile {
        GridTile {
            id: AssetId::new(),
            hash: sample_hash(),
            alt: alt.into(),
            score,
            mime_type: mime.into(),
            thumbnails_generated: thumbs,
            file_size: size,
        }
    }

    #[test]
    fn asset_grid_serves_original_for_unthumbnailed_renderable_image() {
        let tiles = vec![tile(
            "broken.jpg",
            "image/jpeg",
            false,
            5 * 1024 * 1024,
            None,
        )];
        let s = asset_grid(&tiles).into_string();
        // No /thumbs/m/ link, but a /raw link with loading=lazy.
        assert!(!s.contains("/thumbs/m/"));
        assert!(s.contains("/raw"));
        assert!(s.contains("loading=\"lazy\""));
        assert!(!s.contains("tile-placeholder"));
    }

    #[test]
    fn asset_grid_placeholders_for_videos() {
        let tiles = vec![tile(
            "beach.MOV",
            "video/quicktime",
            false,
            120 * 1024 * 1024,
            None,
        )];
        let s = asset_grid(&tiles).into_string();
        assert!(s.contains("tile-placeholder"));
        assert!(s.contains("MOV"));
        assert!(s.contains("beach.MOV"));
        assert!(!s.contains("<img"));
    }

    #[test]
    fn asset_grid_placeholders_for_unrenderable_image_mime() {
        // DNG without a generated thumbnail (e.g., extraction failed for a
        // pathological file). Browser can't render DNG bytes → placeholder.
        let tiles = vec![tile(
            "IMG_1234.DNG",
            "image/x-adobe-dng",
            false,
            20 * 1024 * 1024,
            None,
        )];
        let s = asset_grid(&tiles).into_string();
        assert!(s.contains("tile-placeholder"));
        assert!(s.contains("DNG"));
    }

    #[test]
    fn should_render_inline_allowlist() {
        assert!(should_render_inline("image/jpeg", 1024));
        assert!(should_render_inline("image/png", 1024));
        assert!(should_render_inline("image/webp", 1024));
        assert!(should_render_inline("image/svg+xml", 1024));
        // Browser can't render these as <img>.
        assert!(!should_render_inline("image/x-adobe-dng", 1024));
        assert!(!should_render_inline("image/heic", 1024));
        assert!(!should_render_inline("video/quicktime", 1024));
        // Oversized renderable type still falls through.
        assert!(!should_render_inline("image/jpeg", 26 * 1024 * 1024));
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
            lens_model: None,
            focal_length: None,
            focal_length_35mm: None,
            aperture: None,
            shutter: None,
            iso: None,
            altitude: None,
            country_name: None,
            admin1: None,
            place: None,
            place_distance_m: None,
            thumbnails_generated: true,
        };
        let s = detail_page(&view).into_string();
        assert!(s.contains("class=\"preview\""));
        assert!(s.contains("Download original"));
        assert!(s.contains("2 KB"));
        assert!(!s.contains("Thumbnails pending"));
    }

    fn detail_view(mime: Option<&str>, thumbs: bool, size: i64) -> DetailView {
        DetailView {
            id: AssetId::new(),
            hash: sample_hash(),
            original_filename: "foo".into(),
            mime_type: mime.map(str::to_string),
            file_size: size,
            imported_at: Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap(),
            date_taken: None,
            latitude: None,
            longitude: None,
            camera_make: None,
            camera_model: None,
            lens_model: None,
            focal_length: None,
            focal_length_35mm: None,
            aperture: None,
            shutter: None,
            iso: None,
            altitude: None,
            country_name: None,
            admin1: None,
            place: None,
            place_distance_m: None,
            thumbnails_generated: thumbs,
        }
    }

    #[test]
    fn detail_page_serves_original_for_renderable_unthumbnailed_image() {
        let view = detail_view(Some("image/jpeg"), false, 3 * 1024 * 1024);
        let s = detail_page(&view).into_string();
        assert!(s.contains("class=\"preview\""));
        assert!(s.contains("/raw"));
        assert!(!s.contains("No preview available"));
    }

    #[test]
    fn detail_page_shows_no_preview_message_for_video() {
        let view = detail_view(Some("video/quicktime"), false, 100 * 1024 * 1024);
        let s = detail_page(&view).into_string();
        assert!(s.contains("No preview available"));
        assert!(s.contains("video/quicktime"));
        assert!(!s.contains("class=\"preview\""));
        // Download link is always present as the escape hatch.
        assert!(s.contains("Download original"));
    }

    #[test]
    fn detail_page_renders_place_above_gps() {
        let mut view = detail_view(Some("image/jpeg"), true, 1024);
        view.place = Some("Dalhousie".into());
        view.admin1 = Some("Himachal Pradesh".into());
        view.country_name = Some("India".into());
        view.place_distance_m = Some(2794.0);
        view.latitude = Some(32.539);
        view.longitude = Some(75.972);
        let s = detail_page(&view).into_string();
        let place_idx = s.find("Place").expect("Place label missing");
        let gps_idx = s.find("GPS").expect("GPS label missing");
        assert!(place_idx < gps_idx, "Place must render above GPS");
        assert!(s.contains("Dalhousie, Himachal Pradesh, India"));
        assert!(s.contains("2.8 km"));
    }

    #[test]
    fn detail_page_omits_place_row_when_no_place_data() {
        let view = detail_view(Some("image/jpeg"), true, 1024);
        let s = detail_page(&view).into_string();
        assert!(!s.contains(">Place<"));
    }
}
