//! Read-only WebDAV over the library (the "eventual WebDAV" from goals.md).
//!
//! Class 1, read-only, hand-rolled: OPTIONS + PROPFIND (depth 0/1) + GET.
//! Read-only needs no LOCK, no PUT, no MOVE — every mutating method answers
//! 405 — which shrinks "a WebDAV server" to two handlers and some XML.
//!
//! The tree is virtual: `/dav/YYYY/MM/filename`, from `date_taken` falling
//! back to import time — a phone file manager gets a date-organised library
//! instead of the hash-sharded CAS layout on disk. Name collisions within a
//! month get a short id suffix (`-<id8>`, then `-<id8>-<n>`), so every asset
//! is always reachable under a unique name.

use crate::{AppState, ServerError};
use axum::extract::State;
use axum::http::{Method, StatusCode, header};
use axum::response::Response;
use chrono::Datelike;
use eidetic_db::DavAsset;
use std::collections::BTreeMap;

/// One month's files, keyed by the visible (deduplicated) name.
type MonthFiles = BTreeMap<String, DavAsset>;
/// year -> month -> files.
type DavTree = BTreeMap<i32, BTreeMap<u32, MonthFiles>>;

/// A free name for `original` within a month: `<stem>-<id8>.<ext>` first,
/// then `<stem>-<id8>-<n>.<ext>` for n = 2, 3, ... until one is unused.
fn collision_name(original: &str, id: eidetic_core::AssetId, month: &MonthFiles) -> String {
    let id8 = &id.as_uuid().simple().to_string()[..8];
    let (stem, ext) = match original.rsplit_once('.') {
        Some((stem, ext)) => (stem, format!(".{ext}")),
        None => (original, String::new()),
    };
    let mut candidate = format!("{stem}-{id8}{ext}");
    let mut n = 2u32;
    while month.contains_key(&candidate) {
        candidate = format!("{stem}-{id8}-{n}{ext}");
        n += 1;
    }
    candidate
}

fn build_tree(assets: Vec<DavAsset>) -> DavTree {
    let mut tree: DavTree = BTreeMap::new();
    let mut colliders = Vec::new();
    // First pass: an asset whose own filename is free keeps it, so a
    // generated suffix can never take a name another file really has.
    for asset in assets {
        let month = tree
            .entry(asset.taken.year())
            .or_default()
            .entry(asset.taken.month())
            .or_default();
        if month.contains_key(&asset.original_filename) {
            colliders.push(asset);
        } else {
            month.insert(asset.original_filename.clone(), asset);
        }
    }
    // Second pass, in listing order (the query orders by taken, id), so names
    // never flip between requests.
    for asset in colliders {
        let month = tree
            .entry(asset.taken.year())
            .or_default()
            .entry(asset.taken.month())
            .or_default();
        let name = collision_name(&asset.original_filename, asset.id, month);
        let previous = month.insert(name, asset);
        debug_assert!(previous.is_none(), "collision_name returned a used name");
    }
    tree
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Percent-encode one path segment for an href (RFC 3986 unreserved kept).
fn href_encode(segment: &str) -> String {
    segment
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn dav_response(href: &str, display: &str, entry: Option<&DavAsset>) -> String {
    let specifics = match entry {
        None => "<D:resourcetype><D:collection/></D:resourcetype>".to_string(),
        Some(a) => format!(
            "<D:resourcetype/>\
             <D:getcontentlength>{}</D:getcontentlength>\
             <D:getcontenttype>{}</D:getcontenttype>\
             <D:getlastmodified>{}</D:getlastmodified>",
            a.file_size,
            xml_escape(a.mime_type.as_deref().unwrap_or("application/octet-stream")),
            a.taken.format("%a, %d %b %Y %H:%M:%S GMT"),
        ),
    };
    format!(
        "<D:response><D:href>{href}</D:href>\
         <D:propstat><D:prop>\
         <D:displayname>{}</D:displayname>{specifics}\
         </D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>\
         </D:response>",
        xml_escape(display),
    )
}

fn multistatus(body: String) -> Response {
    Response::builder()
        .status(StatusCode::MULTI_STATUS)
        .header(header::CONTENT_TYPE, "application/xml; charset=utf-8")
        .body(axum::body::Body::from(format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
             <D:multistatus xmlns:D=\"DAV:\">{body}</D:multistatus>"
        )))
        .expect("static headers")
}

fn options_response() -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header("DAV", "1")
        .header(header::ALLOW, "OPTIONS, GET, HEAD, PROPFIND")
        .body(axum::body::Body::empty())
        .expect("static headers")
}

fn method_not_allowed() -> Response {
    Response::builder()
        .status(StatusCode::METHOD_NOT_ALLOWED)
        .header(header::ALLOW, "OPTIONS, GET, HEAD, PROPFIND")
        .body(axum::body::Body::from("read-only WebDAV"))
        .expect("static headers")
}

/// `/dav` and everything under it, all methods.
pub(crate) async fn dav(
    State(state): State<AppState>,
    req: axum::extract::Request,
) -> Result<Response, ServerError> {
    let method = req.method().clone();
    // Everything after /dav, split into decoded segments.
    let raw_path = req.uri().path().trim_start_matches("/dav");
    let segments: Vec<String> = raw_path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(percent_decode)
        .collect();

    if method == Method::OPTIONS {
        return Ok(options_response());
    }
    let is_propfind = method.as_str() == "PROPFIND";
    let is_get = method == Method::GET || method == Method::HEAD;
    if !is_propfind && !is_get {
        return Ok(method_not_allowed());
    }

    let tree = build_tree(
        state
            .repo
            .fetch_dav_listing()
            .await
            .map_err(ServerError::DbFailed)?,
    );

    // Depth: 0 = just this resource, anything else treated as 1. Infinity
    // would be the whole library in one response; clients that want that can
    // walk.
    let depth_zero = req
        .headers()
        .get("Depth")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|d| d.trim() == "0");

    let not_found = || ServerError::NotFound(format!("dav path {raw_path:?}"));

    match segments.as_slice() {
        [] => {
            if is_get {
                return Ok(method_not_allowed());
            }
            let mut body = dav_response("/dav/", "eidetic", None);
            if !depth_zero {
                for year in tree.keys() {
                    let y: i32 = *year;
                    body += &dav_response(&format!("/dav/{y}/"), &y.to_string(), None);
                }
            }
            Ok(multistatus(body))
        }
        [year] => {
            let y: i32 = year.parse().map_err(|_| not_found())?;
            let months = tree.get(&y).ok_or_else(not_found)?;
            if is_get {
                return Ok(method_not_allowed());
            }
            let mut body = dav_response(&format!("/dav/{y}/"), year, None);
            if !depth_zero {
                for m in months.keys() {
                    body += &dav_response(&format!("/dav/{y}/{m:02}/"), &format!("{m:02}"), None);
                }
            }
            Ok(multistatus(body))
        }
        [year, month] => {
            let y: i32 = year.parse().map_err(|_| not_found())?;
            let m: u32 = month.parse().map_err(|_| not_found())?;
            let files = tree.get(&y).and_then(|t| t.get(&m)).ok_or_else(not_found)?;
            if is_get {
                return Ok(method_not_allowed());
            }
            let mut body = dav_response(&format!("/dav/{y}/{m:02}/"), month, None);
            if !depth_zero {
                for (name, asset) in files {
                    body += &dav_response(
                        &format!("/dav/{y}/{m:02}/{}", href_encode(name)),
                        name,
                        Some(asset),
                    );
                }
            }
            Ok(multistatus(body))
        }
        [year, month, name] => {
            let y: i32 = year.parse().map_err(|_| not_found())?;
            let m: u32 = month.parse().map_err(|_| not_found())?;
            let asset = tree
                .get(&y)
                .and_then(|t| t.get(&m))
                .and_then(|files| files.get(name.as_str()))
                .ok_or_else(not_found)?
                .clone();

            if is_propfind {
                let href = format!("/dav/{y}/{m:02}/{}", href_encode(name));
                return Ok(multistatus(dav_response(&href, name, Some(&asset))));
            }
            // GET/HEAD: same Range-capable file serving as /raw.
            use tower::ServiceExt;
            use tower_http::services::ServeFile;
            let mime: mime::Mime = asset
                .mime_type
                .as_deref()
                .and_then(|s| s.parse().ok())
                .unwrap_or(mime::APPLICATION_OCTET_STREAM);
            let response = match ServeFile::new_with_mime(&asset.storage_path, &mime)
                .oneshot(req)
                .await
            {
                Ok(response) => response,
                Err(infallible) => match infallible {},
            };
            Ok(response.map(axum::body::Body::new))
        }
        _ => Err(not_found()),
    }
}

/// Minimal percent-decoding for path segments.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(h), Some(l)) = (
                bytes.get(i + 1).and_then(|b| (*b as char).to_digit(16)),
                bytes.get(i + 2).and_then(|b| (*b as char).to_digit(16)),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn asset_with_id(name: &str, y: i32, m: u32, id: eidetic_core::AssetId) -> DavAsset {
        DavAsset {
            id,
            original_filename: name.to_string(),
            storage_path: std::path::PathBuf::from(format!("/lib/{}", id.as_uuid())),
            file_size: 42,
            mime_type: Some("image/jpeg".into()),
            taken: chrono::Utc.with_ymd_and_hms(y, m, 15, 12, 0, 0).unwrap(),
        }
    }

    fn asset(name: &str, y: i32, m: u32) -> DavAsset {
        asset_with_id(name, y, m, eidetic_core::AssetId::new())
    }

    /// An id whose first 8 hex chars are `12345678`.
    fn id12345678(n: u8) -> eidetic_core::AssetId {
        eidetic_core::AssetId::from(
            uuid::Uuid::parse_str(&format!("12345678-0000-4000-8000-0000000000{n:02}")).unwrap(),
        )
    }

    fn names(tree: &DavTree, y: i32, m: u32) -> BTreeMap<String, eidetic_core::AssetId> {
        tree[&y][&m]
            .iter()
            .map(|(k, v)| (k.clone(), v.id))
            .collect()
    }

    fn expect(
        got: &BTreeMap<String, eidetic_core::AssetId>,
        want: &[(&str, eidetic_core::AssetId)],
    ) {
        let want: BTreeMap<String, _> = want.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        assert_eq!(*got, want);
    }

    #[test]
    fn tree_groups_by_year_month_and_dedups_names() {
        let (first, second) = (asset("a.jpg", 2026, 8), asset("a.jpg", 2026, 8));
        let (fid, sid) = (first.id, second.id);
        let tree = build_tree(vec![first, second, asset("b.jpg", 2025, 1)]);
        assert_eq!(tree.len(), 2);
        let id8 = sid.as_uuid().simple().to_string()[..8].to_string();
        expect(
            &names(&tree, 2026, 8),
            &[("a.jpg", fid), (&format!("a-{id8}.jpg"), sid)],
        );
        assert!(tree[&2025][&1].contains_key("b.jpg"));
    }

    #[test]
    fn natural_name_equal_to_generated_suffix_keeps_its_name() {
        let a = asset("a.jpg", 2026, 8);
        let b = asset("a-12345678.jpg", 2026, 8);
        let c = asset_with_id("a.jpg", 2026, 8, id12345678(1));
        let want = [
            ("a.jpg", a.id),
            ("a-12345678.jpg", b.id),
            ("a-12345678-2.jpg", c.id),
        ];
        let tree = build_tree(vec![a.clone(), b.clone(), c.clone()]);
        expect(&names(&tree, 2026, 8), &want);
        let tree = build_tree(vec![a, c, b]);
        expect(&names(&tree, 2026, 8), &want);
    }

    #[test]
    fn several_fallback_retries() {
        let dup = asset_with_id("a.jpg", 2026, 8, id12345678(1));
        let dup_id = dup.id;
        let tree = build_tree(vec![
            asset("a-12345678.jpg", 2026, 8),
            asset("a-12345678-2.jpg", 2026, 8),
            asset("a-12345678-3.jpg", 2026, 8),
            asset("a.jpg", 2026, 8),
            dup,
        ]);
        let got = names(&tree, 2026, 8);
        assert_eq!(got.len(), 5);
        assert_eq!(got["a-12345678-4.jpg"], dup_id);
    }

    #[test]
    fn repeated_id_prefixes() {
        let input = vec![
            asset("a.jpg", 2026, 8),
            asset_with_id("a.jpg", 2026, 8, id12345678(1)),
            asset_with_id("a.jpg", 2026, 8, id12345678(2)),
        ];
        let ids: Vec<_> = input.iter().map(|a| a.id).collect();
        let tree = build_tree(input);
        expect(
            &names(&tree, 2026, 8),
            &[
                ("a.jpg", ids[0]),
                ("a-12345678.jpg", ids[1]),
                ("a-12345678-2.jpg", ids[2]),
            ],
        );
    }

    #[test]
    fn extensionless_names() {
        let input = vec![
            asset("README", 2026, 8),
            asset_with_id("README", 2026, 8, id12345678(1)),
            asset("README-12345678", 2026, 8),
        ];
        let ids: Vec<_> = input.iter().map(|a| a.id).collect();
        let tree = build_tree(input);
        expect(
            &names(&tree, 2026, 8),
            &[
                ("README", ids[0]),
                ("README-12345678-2", ids[1]),
                ("README-12345678", ids[2]),
            ],
        );
    }

    #[test]
    fn months_are_independent() {
        let sep = asset_with_id("a.jpg", 2026, 9, id12345678(1));
        let sid = sep.id;
        let tree = build_tree(vec![
            asset("a.jpg", 2026, 8),
            asset("a-12345678.jpg", 2026, 8),
            sep,
        ]);
        assert_eq!(tree[&2026][&8].len(), 2);
        expect(&names(&tree, 2026, 9), &[("a.jpg", sid)]);
    }

    #[test]
    fn naming_is_deterministic() {
        let input = vec![
            asset("a.jpg", 2026, 8),
            asset_with_id("a.jpg", 2026, 8, id12345678(1)),
            asset("a-12345678.jpg", 2026, 8),
            asset("a.jpg", 2026, 8),
        ];
        let one = build_tree(input.clone());
        let two = build_tree(input);
        assert_eq!(names(&one, 2026, 8), names(&two, 2026, 8));
        assert_eq!(names(&one, 2026, 8).len(), 4);
    }

    #[test]
    fn href_encoding_round_trips() {
        let name = "IMG 0042 (chai & pakora).jpg";
        assert_eq!(percent_decode(&href_encode(name)), name);
        assert!(!href_encode(name).contains(' '));
        assert!(!href_encode(name).contains('&'));
    }

    #[test]
    fn xml_escapes_entities() {
        assert_eq!(xml_escape("a<b&c>d"), "a&lt;b&amp;c&gt;d");
    }
}
