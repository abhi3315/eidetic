//! Contact sheets of a reel's cut list — the agent gets eyes
//! (goals-v0.9.md phase 1).
//!
//! One tile per slot: the midpoint frame of a video slot's span, or the
//! photo itself, fitted into the project's frame shape the way the render
//! fits it, with the slot number, duration and incoming transition drawn
//! underneath. Every tile is decoded by ffmpeg — the same decoder, display
//! rotation and framing filters the render uses — so a tile shows what the
//! reel will, sideways clips included.
//!
//! Everything is bounded because the input is an arbitrary project file:
//! the project read is bounded by `ReelProject::load_untrusted`, only
//! sources that resolve to regular files inside the library are decoded
//! (one outside it refuses the whole project — see [`library_sources`]), at most
//! [`MAX_TILES`] slots per sheet, tiles at most [`TILE_EDGE`] px, sources over [`MAX_SOURCE_PIXELS`] refused by the
//! decoder, every ffmpeg buffer capped by `-max_alloc` (the framing filters
//! crop before they scale, so no intermediate outgrows its source or tile),
//! and a timeout per ffmpeg run and for the whole sheet. Nothing is written outside a temp
//! dir that is removed on return; the project and its renders are only read.
//!
//! Whether the sheet may leave the machine is not decided here: `eidetic
//! mcp` only exposes it behind `--allow-previews`.

use crate::project::ReelProject;
use crate::reel::{self, Slot, SlotKind, Transition};
use anyhow::{Context, Result, anyhow, bail, ensure};
use image::{Rgb, RgbImage};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

/// Slots per sheet; longer reels page with `first`.
pub const MAX_TILES: usize = 24;
/// Long edge of one tile, px.
pub const TILE_EDGE: u32 = 320;
/// Decoder refusal threshold (ffmpeg `-max_pixels`): 50 MP covers any phone
/// original; a crafted 60000x60000 PNG must not be decoded.
const MAX_SOURCE_PIXELS: u64 = 50_000_000;
/// ffmpeg `-max_alloc`: the largest single buffer a frame grab may allocate.
/// The biggest legitimate one is a decoded source at the pixel cap (50 MP
/// RGBA = 200 MB); framing filters crop before they scale, so nothing
/// downstream is larger. Anything that asks for more fails instead of
/// taking the machine's memory.
const MAX_ALLOC_BYTES: u64 = 256 << 20;
/// One ffmpeg frame grab, and separately the check that resolves every
/// slot's source before any grab. Seeking into a local file takes well
/// under a second; this only catches a wedged decoder or a stalled disk.
const FRAME_TIMEOUT: Duration = Duration::from_secs(20);
/// Every frame grab of the sheet, so a project of slow slots cannot hold
/// the server. The source check runs before it, outside this budget.
const SHEET_TIMEOUT: Duration = Duration::from_secs(90);
const JPEG_QUALITY: u8 = 80;

/// Label glyphs are 5x7 cells drawn at this scale.
const GLYPH_SCALE: u32 = 2;
const GLYPH_ADVANCE: u32 = 6 * GLYPH_SCALE;
const LINE_HEIGHT: u32 = 7 * GLYPH_SCALE;
const LABEL_PAD: u32 = 4;
const LABEL_HEIGHT: u32 = 2 * LINE_HEIGHT + 3 * LABEL_PAD;
/// Narrow (tall-portrait) tiles still get a cell wide enough for a label.
const MIN_CELL_WIDTH: u32 = 13 * GLYPH_ADVANCE;
const GAP: u32 = 4;

/// A finished sheet.
pub struct Sheet {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// The tiles, in sheet order (left to right, top to bottom).
    pub tiles: Vec<Tile>,
}

pub struct Tile {
    /// 1-based slot number, as in the cut list.
    pub n: usize,
    pub kind: SlotKind,
    /// Seconds into the source the tile was taken at (0 for photos).
    pub at: f64,
    pub duration: f64,
    pub transition_in: Transition,
}

/// Tile size for a `width`x`height` project: its aspect, long edge
/// [`TILE_EDGE`], both sides even (the framing filters end in yuv420p).
pub fn tile_size(width: u32, height: u32) -> Result<(u32, u32)> {
    ensure!(
        width > 0 && height > 0,
        "project frame {width}x{height} has no area"
    );
    let long = width.max(height) as f64;
    let side = |v: u32| (((v as f64 * TILE_EDGE as f64 / long).round() as u32) & !1).max(16);
    Ok((side(width), side(height)))
}

/// Where in its source a slot's tile is taken: the midpoint of the span
/// the reel plays, or 0 for a photo. Refuses spans no render could play
/// rather than guessing at a frame (`ReelProject::validate` refuses them
/// first; this keeps the function safe on its own).
pub fn tile_timestamp(n: usize, slot: &Slot) -> Result<f64> {
    ensure!(
        slot.duration.is_finite() && slot.duration > 0.0,
        "slot {n} has an unusable duration ({})",
        slot.duration
    );
    match slot.kind {
        SlotKind::Photo | SlotKind::PhotoScenic => Ok(0.0),
        SlotKind::Video => {
            ensure!(
                slot.start.is_finite() && slot.start >= 0.0,
                "slot {n} has an unusable start ({})",
                slot.start
            );
            let mid = slot.start + slot.duration / 2.0;
            ensure!(mid.is_finite(), "slot {n}'s midpoint is not finite");
            Ok(mid)
        }
    }
}

/// Every slot's source, resolved (symlinks and `..` followed), if each one
/// is a regular file inside `library_dir`; any other source refuses the
/// whole project, whichever page is asked for. Both sides are resolved, so
/// a library directory reached through a symlink still matches.
///
/// Hard links and a path component swapped after this check are not
/// caught; only someone who can already write to the library can do either.
fn library_sources(slots: &[Slot], library_dir: &Path) -> Result<Vec<PathBuf>> {
    let root = std::fs::canonicalize(library_dir).map_err(|e| {
        anyhow!(
            "the library directory {} cannot be resolved ({e}); preview_sheet only shows \
             files from the library",
            library_dir.display()
        )
    })?;
    slots
        .iter()
        .enumerate()
        .map(|(i, slot)| {
            let n = i + 1;
            let real = match std::fs::canonicalize(&slot.source) {
                Ok(real) => real,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(
                    "slot {n}'s source is missing: {} — swap or drop the slot",
                    slot.source.display()
                ),
                Err(e) => bail!(
                    "slot {n}'s source cannot be resolved ({e}): {}",
                    slot.source.display()
                ),
            };
            ensure!(
                real.starts_with(&root),
                "slot {n}'s source is outside the library ({}): {} — preview_sheet only shows \
                 files from the library; swap the slot or re-plan with create_reel",
                root.display(),
                slot.source.display()
            );
            // Directories and FIFOs would fail or block the decoder.
            ensure!(
                std::fs::metadata(&real).is_ok_and(|m| m.is_file()),
                "slot {n}'s source is not a regular file: {} — swap or drop the slot",
                slot.source.display()
            );
            Ok(real)
        })
        .collect()
}

/// Build the sheet for slots `first..` (1-based) of `proj`, at most
/// [`MAX_TILES`] of them. Every slot's source must lie inside
/// `library_dir` (see [`library_sources`]), which is checked before ffmpeg
/// is even looked up.
pub async fn contact_sheet(proj: &ReelProject, first: usize, library_dir: &Path) -> Result<Sheet> {
    let total = proj.slots.len();
    ensure!(total > 0, "the project has no slots to preview");
    ensure!(
        (1..=total).contains(&first),
        "first={first} is outside the cut list (slots 1-{total})"
    );
    // Resolving paths blocks, and a stalled mount must not hold the server.
    let (all, lib) = (proj.slots.clone(), library_dir.to_path_buf());
    let sources = tokio::time::timeout(
        FRAME_TIMEOUT,
        tokio::task::spawn_blocking(move || library_sources(&all, &lib)),
    )
    .await
    .map_err(|_| {
        anyhow!(
            "checking the slot sources took longer than {}s; the library may be on a slow or \
             sleeping disk — retry once it is reachable",
            FRAME_TIMEOUT.as_secs()
        )
    })?
    .context("the slot source check panicked")??;
    let Some(ffmpeg) = eidetic_ingest::video::ffmpeg() else {
        bail!("ffmpeg not found; install it (or set EIDETIC_FFMPEG_PATH) to build preview sheets");
    };
    let fit = reel::Fit::parse(&proj.frame, proj.height > proj.width)?;
    let (tw, th) = tile_size(proj.width, proj.height)?;
    let slots: Vec<(usize, &Slot)> = proj
        .slots
        .iter()
        .enumerate()
        .skip(first - 1)
        .take(MAX_TILES)
        .map(|(i, s)| (i + 1, s))
        .collect();

    // Validate everything before spending a subprocess on anything.
    let mut tiles = Vec::with_capacity(slots.len());
    for &(n, slot) in &slots {
        let at = tile_timestamp(n, slot)?;
        tiles.push(Tile {
            n,
            kind: slot.kind,
            at,
            duration: slot.duration,
            transition_in: slot.transition_in,
        });
    }

    let cell_w = tw.max(MIN_CELL_WIDTH);
    let cell_h = th + LABEL_HEIGHT;
    let count = tiles.len() as u32;
    // Near-square sheet: columns so the grid's aspect is about 1.
    let cols =
        ((count as f64 * cell_h as f64 / cell_w as f64).sqrt().ceil() as u32).clamp(1, count);
    let rows = count.div_ceil(cols);
    let width = GAP + cols * (cell_w + GAP);
    let height = GAP + rows * (cell_h + GAP);
    let mut sheet = RgbImage::from_pixel(width, height, Rgb([0, 0, 0]));

    let work = tempfile::tempdir().context("create a preview work dir")?;
    let grab_all = async {
        for (k, (&(n, slot), tile)) in slots.iter().zip(&tiles).enumerate() {
            let png = work.path().join(format!("tile_{k:03}.png"));
            let vf = reel::still_filter(slot, tw, th, fit);
            // The resolved path, so ffmpeg opens exactly what was checked.
            grab_frame(ffmpeg, n, slot, &sources[n - 1], tile.at, &vf, &png).await?;
            let frame = decode_tile(n, &png, (tw, th))?;
            let (col, row) = (k as u32 % cols, k as u32 / cols);
            let x = GAP + col * (cell_w + GAP);
            let y = GAP + row * (cell_h + GAP);
            image::imageops::replace(&mut sheet, &frame, (x + (cell_w - tw) / 2) as i64, y as i64);
            draw_label(&mut sheet, x, y + th, cell_w, tile, n == 1);
        }
        Ok::<_, anyhow::Error>(())
    };
    tokio::time::timeout(SHEET_TIMEOUT, grab_all)
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "building the preview sheet took longer than {}s; the media may be on a \
                 slow or sleeping disk — retry once it is reachable",
                SHEET_TIMEOUT.as_secs()
            )
        })??;
    drop(work);

    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, JPEG_QUALITY)
        .encode_image(&sheet)
        .context("encode the preview sheet")?;
    Ok(Sheet {
        jpeg,
        width,
        height,
        tiles,
    })
}

/// Decode one framed still of `slot`, read from `source`, at `at` seconds
/// into `png`. Errors name the source as the project spells it.
async fn grab_frame(
    ffmpeg: &Path,
    n: usize,
    slot: &Slot,
    source: &Path,
    at: f64,
    vf: &str,
    png: &Path,
) -> Result<()> {
    let mut cmd = tokio::process::Command::new(ffmpeg);
    cmd.args(["-nostdin", "-y", "-v", "error"])
        .args(["-max_alloc", &MAX_ALLOC_BYTES.to_string()])
        .args(["-max_pixels", &MAX_SOURCE_PIXELS.to_string()]);
    if slot.kind == SlotKind::Video {
        cmd.args(["-ss", &format!("{at:.3}")]);
    }
    cmd.arg("-i")
        .arg(source)
        .args(["-frames:v", "1", "-an", "-vf", vf])
        .args(["-f", "image2", "-c:v", "png"])
        .arg(png)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        // A timed-out grab is dropped mid-wait; this is what reaps it.
        .kill_on_drop(true);
    let out = tokio::time::timeout(FRAME_TIMEOUT, cmd.output())
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "ffmpeg took longer than {}s to decode slot {n} ({}); the file may be corrupt or on a stalled disk",
                FRAME_TIMEOUT.as_secs(),
                slot.source.display()
            )
        })?
        .context("run ffmpeg for a preview frame")?;
    if !out.status.success() {
        let detail = String::from_utf8_lossy(&out.stderr);
        let detail = detail.trim();
        // The tail is where ffmpeg says why; keep the error one screen.
        let tail = &detail[detail.floor_char_boundary(detail.len().saturating_sub(400))..];
        bail!(
            "ffmpeg could not decode slot {n} ({}): {tail} — the file may be corrupt or \
             in a format ffmpeg lacks; swap or drop the slot",
            slot.source.display()
        );
    }
    // Seeking past the end exits 0 having written nothing.
    ensure!(
        png.is_file(),
        "slot {n} has no frame at {at:.2}s of {} — its span runs past the end of the \
         clip; retime or swap it",
        slot.source.display()
    );
    Ok(())
}

/// Load a tile ffmpeg wrote, refusing anything but the size it was asked for.
fn decode_tile(n: usize, png: &Path, (tw, th): (u32, u32)) -> Result<RgbImage> {
    let mut reader = image::ImageReader::open(png)
        .with_context(|| format!("read the preview frame for slot {n}"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(tw);
    limits.max_image_height = Some(th);
    reader.limits(limits);
    let frame = reader
        .decode()
        .with_context(|| format!("decode the preview frame for slot {n}"))?
        .into_rgb8();
    ensure!(
        frame.dimensions() == (tw, th),
        "slot {n}'s preview frame is {}x{}, expected {tw}x{th}",
        frame.width(),
        frame.height()
    );
    Ok(frame)
}

/// The two label lines for a tile: `#3 2.50s` and its incoming joint.
pub fn label_lines(tile: &Tile, opens: bool) -> [String; 2] {
    let joint = match tile.transition_in {
        // The first shot has no predecessor, so no joint is ever rendered.
        _ if opens => "OPEN".to_string(),
        Transition::Cut => "CUT".to_string(),
        Transition::Crossfade { secs } => format!("XFADE {secs:.2}s"),
        Transition::DipToBlack { secs } => format!("DIP {secs:.2}s"),
        Transition::Slide { secs } => format!("SLIDE {secs:.2}s"),
        Transition::Whip { secs } => format!("WHIP {secs:.2}s"),
    };
    [format!("#{} {:.2}s", tile.n, tile.duration), joint]
}

fn draw_label(sheet: &mut RgbImage, x: u32, y: u32, cell_w: u32, tile: &Tile, opens: bool) {
    let max_chars = (cell_w / GLYPH_ADVANCE) as usize;
    for (line, text) in label_lines(tile, opens).iter().enumerate() {
        let ly = y + LABEL_PAD + line as u32 * (LINE_HEIGHT + LABEL_PAD);
        for (i, c) in text.chars().take(max_chars).enumerate() {
            draw_glyph(sheet, x + i as u32 * GLYPH_ADVANCE, ly, c);
        }
    }
}

fn draw_glyph(sheet: &mut RgbImage, x: u32, y: u32, c: char) {
    let Some(rows) = glyph(c) else { return };
    for (r, bits) in rows.iter().enumerate() {
        for col in 0..5u32 {
            if bits & (0b10000 >> col) == 0 {
                continue;
            }
            for dy in 0..GLYPH_SCALE {
                for dx in 0..GLYPH_SCALE {
                    let (px, py) = (x + col * GLYPH_SCALE + dx, y + r as u32 * GLYPH_SCALE + dy);
                    if px < sheet.width() && py < sheet.height() {
                        sheet.put_pixel(px, py, Rgb([255, 255, 255]));
                    }
                }
            }
        }
    }
}

/// A 5x7 bitmap for every character a label can contain; bit 4 is the
/// leftmost column. The labels are generated, so this is the whole alphabet.
fn glyph(c: char) -> Option<[u8; 7]> {
    Some(match c {
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C],
        '#' => [0x0A, 0x0A, 0x1F, 0x0A, 0x1F, 0x0A, 0x0A],
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'N' => [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        // Lower case so "2.50s" cannot be misread as "2.505".
        's' => [0x00, 0x00, 0x0F, 0x10, 0x0E, 0x01, 0x1E],
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use eidetic_core::AssetId;
    use eidetic_db::SearchResult;
    use std::collections::HashMap;

    fn slot(kind: SlotKind, start: f64, duration: f64) -> Slot {
        Slot {
            asset: AssetId::new(),
            source: PathBuf::from("/nowhere"),
            kind,
            start,
            duration,
            focus_x: None,
            score: 0.0,
            pinned: false,
            transition_in: Transition::Cut,
            motion: None,
            geometry: None,
            near_duplicates: None,
        }
    }

    #[test]
    fn video_tiles_come_from_the_middle_of_the_played_span() {
        assert_eq!(
            tile_timestamp(1, &slot(SlotKind::Video, 1.0, 1.0)).unwrap(),
            1.5
        );
        assert_eq!(
            tile_timestamp(1, &slot(SlotKind::Video, 0.0, 4.0)).unwrap(),
            2.0
        );
        assert_eq!(
            tile_timestamp(1, &slot(SlotKind::Photo, 0.0, 3.0)).unwrap(),
            0.0
        );
    }

    #[test]
    fn unplayable_spans_are_refused_not_guessed() {
        for (start, dur) in [
            (0.0, f64::NAN),
            (0.0, f64::INFINITY),
            (0.0, 0.0),
            (0.0, -1.0),
            (f64::NAN, 1.0),
            (-2.0, 1.0),
            (f64::MAX, f64::MAX),
        ] {
            let err = tile_timestamp(7, &slot(SlotKind::Video, start, dur)).unwrap_err();
            assert!(err.to_string().contains("slot 7"), "{start}/{dur}: {err}");
        }
        assert!(tile_timestamp(2, &slot(SlotKind::Photo, 0.0, f64::NAN)).is_err());
    }

    #[test]
    fn tiles_keep_the_project_aspect_within_bounds() {
        assert_eq!(tile_size(1920, 1080).unwrap(), (320, 180));
        assert_eq!(tile_size(1080, 1920).unwrap(), (180, 320));
        assert_eq!(tile_size(u32::MAX, 1).unwrap(), (320, 16));
        assert!(tile_size(0, 1080).is_err());
    }

    /// A search hit for a file stored at `lib/ab/cd/<hash>.<ext>`, with
    /// dummy bytes there (and at its medium thumbnail when it has one).
    fn stored(lib: &Path, byte: u8, ext: &str, video: bool, thumbs: bool) -> SearchResult {
        let hash = eidetic_core::Sha256::from_hex(&format!("{byte:02x}").repeat(32)).unwrap();
        let hex = hash.to_string();
        let storage_path = lib
            .join(&hex[..2])
            .join(&hex[2..4])
            .join(format!("{hex}.{ext}"));
        let mut files = vec![storage_path.clone()];
        if thumbs {
            files.push(eidetic_ingest::thumbnail::thumbnail_path(
                lib,
                &hash,
                eidetic_ingest::thumbnail::ThumbSize::Medium,
            ));
        }
        for f in files {
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, b"media").unwrap();
        }
        SearchResult {
            id: AssetId::new(),
            storage_path,
            score: 0.9,
            frame_ts: video.then_some(1.0),
            duration_secs: video.then_some(4.0),
            speech: None,
            mime_type: None,
            file_size: 0,
            thumbnails_generated: thumbs,
            date_taken: None,
            camera_make: None,
            camera_model: None,
            latitude: None,
            longitude: None,
        }
    }

    #[test]
    fn planned_sources_pass_the_library_check() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("library");
        let hits = [
            stored(&lib, 0xab, "mp4", true, false),
            stored(&lib, 0xcd, "png", false, true),
            stored(&lib, 0xef, "png", false, false),
        ];
        let slots = reel::plan(
            &hits,
            60.0,
            &lib,
            &HashMap::new(),
            &reel::Signals::default(),
        );
        assert_eq!(slots.len(), 3);
        // One slot reads a thumbnail, one an original photo, one a video.
        assert!(
            slots
                .iter()
                .any(|s| s.source.starts_with(lib.join(".thumbs")))
        );
        let real_lib = std::fs::canonicalize(&lib).unwrap();
        let want: Vec<PathBuf> = slots
            .iter()
            .map(|s| std::fs::canonicalize(&s.source).unwrap())
            .collect();
        assert_eq!(library_sources(&slots, &lib).unwrap(), want);
        assert!(want.iter().all(|p| p.starts_with(&real_lib)));

        // The same library reached through a symlinked directory, with the
        // project spelling sources through the link or the real path.
        #[cfg(unix)]
        {
            let link = dir.path().join("library-link");
            std::os::unix::fs::symlink(&lib, &link).unwrap();
            assert_eq!(library_sources(&slots, &link).unwrap(), want);
            let via_link = reel::plan(
                &hits,
                60.0,
                &link,
                &HashMap::new(),
                &reel::Signals::default(),
            );
            assert_eq!(library_sources(&via_link, &link).unwrap(), want);
            assert_eq!(library_sources(&via_link, &lib).unwrap(), want);
        }
    }

    #[test]
    fn sources_outside_the_library_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (lib, outside) = (dir.path().join("library"), dir.path().join("outside"));
        std::fs::create_dir_all(lib.join("album")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let ok = lib.join("ok.png");
        let secret = outside.join("secret.png");
        std::fs::write(&ok, b"media").unwrap();
        std::fs::write(&secret, b"TOP SECRET").unwrap();
        let with = |source: PathBuf| {
            let mut bad = slot(SlotKind::Photo, 0.0, 1.0);
            bad.source = source;
            let mut good = slot(SlotKind::Photo, 0.0, 1.0);
            good.source = ok.clone();
            vec![good, bad]
        };
        let refused = |source: PathBuf, want: &str| {
            let err = library_sources(&with(source.clone()), &lib)
                .unwrap_err()
                .to_string();
            assert!(err.contains("slot 2's source"), "{source:?}: {err}");
            assert!(err.contains(want), "{source:?}: want {want:?}: {err}");
            assert!(!err.contains("TOP SECRET"), "{err}");
        };

        refused(secret.clone(), "outside the library");
        refused(
            lib.join("..").join("outside").join("secret.png"),
            "outside the library",
        );
        refused(lib.join("album"), "not a regular file");
        refused(lib.join("gone.png"), "is missing");
        refused(outside.join("gone.png"), "is missing");
        // Any failure other than "not found" is not reported as missing.
        #[cfg(unix)]
        refused(ok.join("x.png"), "cannot be resolved");
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink(&secret, lib.join("link.png")).unwrap();
            refused(lib.join("link.png"), "outside the library");
            symlink(&outside, lib.join("sub")).unwrap();
            refused(lib.join("sub").join("secret.png"), "outside the library");
        }
        // A library directory that does not exist refuses everything.
        let err = library_sources(&with(ok.clone())[..1], &dir.path().join("nope"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("cannot be resolved"), "{err}");
    }

    #[test]
    fn labels_carry_slot_duration_and_joint_in_the_font() {
        let mut t = Tile {
            n: 12,
            kind: SlotKind::Video,
            at: 0.0,
            duration: 2.5,
            transition_in: Transition::Crossfade { secs: 0.8 },
        };
        assert_eq!(label_lines(&t, false), ["#12 2.50s", "XFADE 0.80s"]);
        assert_eq!(label_lines(&t, true)[1], "OPEN");
        for tr in [
            Transition::Cut,
            Transition::DipToBlack { secs: 0.7 },
            Transition::Slide { secs: 0.4 },
            Transition::Whip { secs: 0.2 },
        ] {
            t.transition_in = tr;
            for line in label_lines(&t, false) {
                for c in line.chars().filter(|c| *c != ' ') {
                    assert!(glyph(c).is_some(), "no glyph for {c:?} in {line:?}");
                }
                assert!(
                    line.len() as u32 * GLYPH_ADVANCE <= MIN_CELL_WIDTH,
                    "{line}"
                );
            }
        }
    }
}
