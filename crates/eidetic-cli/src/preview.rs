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
//! the project read is capped in size and time and refuses non-regular
//! files, at most [`MAX_TILES`] slots per sheet, tiles at most
//! [`TILE_EDGE`] px, sources over [`MAX_SOURCE_PIXELS`] refused by the
//! decoder, every ffmpeg buffer capped by `-max_alloc` (the framing filters
//! crop before they scale, so no intermediate outgrows its source or tile),
//! and a timeout per ffmpeg run and for the whole sheet. Nothing is written outside a temp
//! dir that is removed on return; the project and its renders are only read.
//!
//! Whether the sheet may leave the machine is not decided here: `eidetic
//! mcp` only exposes it behind `--allow-previews`.

use crate::project::ReelProject;
use crate::reel::{self, Slot, SlotKind, Transition};
use anyhow::{Context, Result, bail, ensure};
use image::{Rgb, RgbImage};
use std::path::Path;
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
/// Project files are a few KB to a few hundred; anything bigger is not one.
const MAX_PROJECT_BYTES: u64 = 8 << 20;
/// Reading a small local file; this only catches a hung network mount.
const LOAD_TIMEOUT: Duration = Duration::from_secs(10);
/// One ffmpeg frame grab. Seeking into a local file takes well under a
/// second; this only catches a wedged decoder or a stalled disk.
const FRAME_TIMEOUT: Duration = Duration::from_secs(20);
/// The whole sheet, so a project of slow slots cannot hold the server.
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
/// rather than guessing at a frame.
pub fn tile_timestamp(n: usize, slot: &Slot) -> Result<f64> {
    ensure!(
        slot.duration.is_finite() && slot.duration > 0.0,
        "slot {n} has an unusable duration ({}); retime it with edit_reel",
        slot.duration
    );
    match slot.kind {
        SlotKind::Photo | SlotKind::PhotoScenic => Ok(0.0),
        SlotKind::Video => {
            ensure!(
                slot.start.is_finite() && slot.start >= 0.0,
                "slot {n} has an unusable start ({}); swap it with edit_reel",
                slot.start
            );
            let mid = slot.start + slot.duration / 2.0;
            ensure!(mid.is_finite(), "slot {n}'s midpoint is not finite");
            Ok(mid)
        }
    }
}

/// Load the project a preview names: a regular file of at most
/// [`MAX_PROJECT_BYTES`], read off the async executor within
/// [`LOAD_TIMEOUT`]. A read stuck in the kernel (a dead network mount)
/// cannot be cancelled, but it no longer holds the server: the call fails
/// and the blocked thread is abandoned.
pub async fn load_project(path: &Path) -> Result<ReelProject> {
    let owned = path.to_path_buf();
    let load =
        tokio::task::spawn_blocking(move || ReelProject::load_bounded(&owned, MAX_PROJECT_BYTES));
    tokio::time::timeout(LOAD_TIMEOUT, load)
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "reading {} took longer than {}s; is it on a reachable disk?",
                path.display(),
                LOAD_TIMEOUT.as_secs()
            )
        })?
        .context("project reader panicked")?
}

/// Build the sheet for slots `first..` (1-based) of `proj`, at most
/// [`MAX_TILES`] of them.
pub async fn contact_sheet(proj: &ReelProject, first: usize) -> Result<Sheet> {
    let total = proj.slots.len();
    ensure!(total > 0, "the project has no slots to preview");
    ensure!(
        (1..=total).contains(&first),
        "first={first} is outside the cut list (slots 1-{total})"
    );
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
        ensure!(
            slot.source.is_file(),
            "slot {n}'s source is missing: {} — swap or drop the slot",
            slot.source.display()
        );
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
            grab_frame(ffmpeg, n, slot, tile.at, &vf, &png).await?;
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

/// Decode one framed still of `slot` at `at` seconds into `png`.
async fn grab_frame(
    ffmpeg: &Path,
    n: usize,
    slot: &Slot,
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
        .arg(&slot.source)
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
    use std::path::PathBuf;

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
