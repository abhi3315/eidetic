//! Read-only WebDAV over the library (the "eventual WebDAV" from goals.md).
//!
//! Class 1, read-only, hand-rolled: OPTIONS + PROPFIND (depth 0/1) + GET.
//! Read-only needs no LOCK, no PUT, no MOVE — every mutating method answers
//! 405 — which shrinks "a WebDAV server" to two handlers and some XML.
//!
//! The tree is virtual: `/dav/YYYY/MM/filename`, from `date_taken` falling
//! back to import time — a phone file manager gets a date-organised library
//! instead of the hash-sharded CAS layout on disk. Name collisions within a
//! month get a short id suffix, so every asset is always reachable.

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

fn build_tree(assets: Vec<DavAsset>) -> DavTree {
    let mut tree: DavTree = BTreeMap::new();
    for asset in assets {
        let month = tree
            .entry(asset.taken.year())
            .or_default()
            .entry(asset.taken.month())
            .or_default();
        let mut name = asset.original_filename.clone();
        if month.contains_key(&name) {
            // Deterministic collision suffix: first 8 chars of the asset id,
            // inserted before the extension. Listing order is stable (query
            // orders by taken, id), so names never flip between requests.
            let id8 = &asset.id.as_uuid().simple().to_string()[..8];
            name = match name.rsplit_once('.') {
                Some((stem, ext)) => format!("{stem}-{id8}.{ext}"),
                None => format!("{name}-{id8}"),
            };
        }
        month.insert(name, asset);
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

    fn asset(name: &str, y: i32, m: u32) -> DavAsset {
        DavAsset {
            id: eidetic_core::AssetId::new(),
            original_filename: name.to_string(),
            storage_path: std::path::PathBuf::from(format!("/lib/{name}")),
            file_size: 42,
            mime_type: Some("image/jpeg".into()),
            taken: chrono::Utc.with_ymd_and_hms(y, m, 15, 12, 0, 0).unwrap(),
        }
    }

    #[test]
    fn tree_groups_by_year_month_and_dedups_names() {
        let tree = build_tree(vec![
            asset("a.jpg", 2026, 8),
            asset("a.jpg", 2026, 8),
            asset("b.jpg", 2025, 1),
        ]);
        assert_eq!(tree.len(), 2);
        let aug = &tree[&2026][&8];
        assert_eq!(aug.len(), 2, "collision kept both files");
        assert!(aug.contains_key("a.jpg"));
        assert!(
            aug.keys()
                .any(|k| k.starts_with("a-") && k.ends_with(".jpg")),
            "suffixed duplicate name: {:?}",
            aug.keys().collect::<Vec<_>>()
        );
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
