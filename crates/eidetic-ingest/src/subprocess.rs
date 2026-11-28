//! Run a media subprocess (ffmpeg, ffprobe) to completion under a deadline.
//!
//! A decoder wedged on a corrupt file would otherwise hold its caller (an
//! ingest worker, an MCP render) forever. On expiry the child, and only that
//! child, is killed and reaped, and the error names the program, the file the
//! run was about, the elapsed time and the end of its stderr.
//!
//! Synchronous on purpose: callers run it from plain threads and from
//! blocking sections of async tasks alike.

use crate::{Error, Result};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

/// How much of stderr an error message carries.
pub const STDERR_TAIL_BYTES: usize = 1024;

/// Most of stderr a run keeps. Past it only the latest bytes survive, so a
/// decoder that keeps logging errors cannot grow memory without bound.
pub const STDERR_CAP_BYTES: usize = 8 << 20;

/// How long stderr may stay open once the child has exited (or been
/// killed). Longer means something other than the child holds it. Stdout
/// has no such grace: a descendant holding it runs into the deadline.
pub const STDERR_GRACE: Duration = Duration::from_secs(1);

/// Media length assumed when the real one is unknown (no container duration,
/// or no ffprobe to read it).
pub const ASSUMED_MEDIA_SECS: f64 = 3600.0;

/// Longest media length a [`Budget`] scales to.
const MAX_MEDIA_SECS: f64 = 24.0 * 3600.0;

/// A completed run: whatever stdout became, plus its stderr.
#[derive(Debug)]
pub struct Finished<T> {
    pub stdout: T,
    /// All of stderr, or only its last [`STDERR_CAP_BYTES`] when
    /// `stderr_truncated` is set.
    pub stderr: Vec<u8>,
    pub stderr_truncated: bool,
}

/// A deadline that grows with the length of the media being processed:
/// `base + per_media_sec × duration`.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    base: Duration,
    per_media_sec: f64,
}

impl Budget {
    pub const fn new(base: Duration, per_media_sec: f64) -> Self {
        Self {
            base,
            per_media_sec,
        }
    }

    /// The deadline for `media_secs` of media. Unknown, NaN, negative or
    /// infinite durations count as [`ASSUMED_MEDIA_SECS`]; huge ones are
    /// clamped to a day of media.
    pub fn deadline(&self, media_secs: Option<f64>) -> Duration {
        let secs = media_secs
            .filter(|s| s.is_finite() && *s >= 0.0)
            .unwrap_or(ASSUMED_MEDIA_SECS)
            .min(MAX_MEDIA_SECS);
        let scaled =
            Duration::try_from_secs_f64(secs * self.per_media_sec).unwrap_or(Duration::MAX);
        self.base.saturating_add(scaled)
    }
}

/// Run `cmd` to completion within `deadline`, collecting stdout.
///
/// `subject` is the file the run is about (its input, or the file it
/// produces when there is no single input); errors name it. A non-zero exit
/// is [`Error::SubprocessFailed`], expiry is [`Error::SubprocessTimeout`].
pub fn run(cmd: &mut Command, subject: &Path, deadline: Duration) -> Result<Finished<Vec<u8>>> {
    run_with(cmd, subject, deadline, |mut out| {
        let mut buf = Vec::new();
        out.read_to_end(&mut buf)?;
        Ok(buf)
    })
}

/// [`run`], but stdout is handed to `read` on a reader thread, so a large
/// output can be decoded as it streams instead of being buffered first.
/// `read` must read to EOF.
pub fn run_with<T, F>(
    cmd: &mut Command,
    subject: &Path,
    deadline: Duration,
    read: F,
) -> Result<Finished<T>>
where
    T: Send + 'static,
    F: FnOnce(ChildStdout) -> io::Result<T> + Send + 'static,
{
    let program = program_name(cmd);
    let io_err = |source: io::Error| Error::Io {
        path: subject.to_path_buf(),
        source,
    };
    let start = Instant::now();
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| Error::Io {
            path: PathBuf::from(cmd.get_program()),
            source,
        })?;
    let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
    let mut child = Reaped(child);
    let (Some(stdout), Some(stderr)) = (stdout, stderr) else {
        return Err(io_err(io::Error::other(
            "child output pipes were not captured",
        )));
    };

    // Each reader drains its pipe to EOF and reports once, so a full pipe
    // can never stall the child and the waits below never block on a join.
    let (out_tx, out_rx) = mpsc::channel();
    thread::Builder::new()
        .name(format!("{program}-stdout"))
        .spawn(move || {
            let _ = out_tx.send(read(stdout));
        })
        .map_err(io_err)?;
    let (err_tx, err_rx) = mpsc::channel();
    thread::Builder::new()
        .name(format!("{program}-stderr"))
        .spawn(move || {
            let _ = err_tx.send(read_capped(stderr));
        })
        .map_err(io_err)?;

    let remaining = || deadline.saturating_sub(start.elapsed());
    let timed_out = |child: Reaped, err_rx: &Receiver<(Vec<u8>, bool)>| {
        drop(child);
        let elapsed = start.elapsed();
        let (stderr, _) = err_rx.recv_timeout(STDERR_GRACE).unwrap_or_default();
        Error::SubprocessTimeout {
            program: program.clone(),
            subject: subject.to_path_buf(),
            elapsed,
            deadline,
            stderr_tail: stderr_tail(&stderr),
        }
    };

    // Stdout first: this blocks without polling for as long as the child
    // is producing output.
    let stdout = match out_rx.recv_timeout(remaining()) {
        Ok(read) => read.map_err(io_err)?,
        Err(RecvTimeoutError::Timeout) => return Err(timed_out(child, &err_rx)),
        Err(RecvTimeoutError::Disconnected) => {
            return Err(io_err(io::Error::other("stdout reader thread panicked")));
        }
    };

    // Stdout at EOF normally means the child is exiting; poll briefly.
    let mut nap = Duration::from_millis(1);
    let status = loop {
        if let Some(status) = child.0.try_wait().map_err(io_err)? {
            break status;
        }
        let left = remaining();
        if left.is_zero() {
            return Err(timed_out(child, &err_rx));
        }
        thread::sleep(nap.min(left));
        nap = (nap * 2).min(Duration::from_millis(5));
    };

    // The child is gone, so its stderr must reach EOF almost at once. If
    // something else still holds the pipe, stderr may be incomplete, and a
    // caller parsing it must not see a truncated success.
    let (stderr, stderr_truncated) = match err_rx.recv_timeout(STDERR_GRACE) {
        Ok(read) => read,
        Err(RecvTimeoutError::Timeout) => {
            return Err(io_err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{program} exited but its stderr stayed open"),
            )));
        }
        Err(RecvTimeoutError::Disconnected) => {
            return Err(io_err(io::Error::other("stderr reader thread panicked")));
        }
    };

    if !status.success() {
        return Err(Error::SubprocessFailed {
            program,
            subject: subject.to_path_buf(),
            status,
            elapsed: start.elapsed(),
            stderr_tail: stderr_tail(&stderr),
        });
    }
    Ok(Finished {
        stdout,
        stderr,
        stderr_truncated,
    })
}

/// Read `r` to EOF, keeping at most the last [`STDERR_CAP_BYTES`]; the flag
/// says whether anything was dropped. The buffer may reach twice the cap
/// before its front is dropped, so the trimming copy stays rare.
fn read_capped(mut r: impl Read) -> (Vec<u8>, bool) {
    let mut buf = Vec::new();
    let mut truncated = false;
    let mut chunk = [0u8; 8192];
    loop {
        match r.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > 2 * STDERR_CAP_BYTES {
                    buf.drain(..buf.len() - STDERR_CAP_BYTES);
                    truncated = true;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    if buf.len() > STDERR_CAP_BYTES {
        buf.drain(..buf.len() - STDERR_CAP_BYTES);
        truncated = true;
    }
    (buf, truncated)
}

/// The last [`STDERR_TAIL_BYTES`] of `stderr`, trimmed, lossily decoded and
/// cut on a character boundary, with a leading "…" when cut.
pub fn stderr_tail(stderr: &[u8]) -> String {
    let text = stderr.trim_ascii();
    if text.len() <= STDERR_TAIL_BYTES {
        return String::from_utf8_lossy(text).into_owned();
    }
    let mut cut = text.len() - STDERR_TAIL_BYTES;
    // Skip UTF-8 continuation bytes so the tail starts on a whole character.
    while cut < text.len() && text[cut] & 0xC0 == 0x80 {
        cut += 1;
    }
    format!("…{}", String::from_utf8_lossy(&text[cut..]))
}

/// The program's file name, for error messages.
fn program_name(cmd: &Command) -> String {
    let program = Path::new(cmd.get_program());
    program
        .file_name()
        .unwrap_or(program.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// Kills and reaps the child on drop, so every early return (deadline, a
/// failing reader, a panic) leaves no process behind. Once the child has
/// been reaped, `kill` is a no-op that sends no signal.
struct Reaped(Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_tail_keeps_short_output_whole() {
        assert_eq!(stderr_tail(b"  boom\n"), "boom");
        assert_eq!(stderr_tail(b""), "");
    }

    #[test]
    fn read_capped_keeps_short_stderr_whole_and_long_stderr_by_its_end() {
        assert_eq!(read_capped(&b"note\n"[..]), (b"note\n".to_vec(), false));

        let long = io::repeat(b'x')
            .take(2 * STDERR_CAP_BYTES as u64 + 5)
            .chain(&b"END"[..]);
        let (kept, truncated) = read_capped(long);
        assert!(truncated);
        assert_eq!(kept.len(), STDERR_CAP_BYTES);
        assert!(kept.ends_with(b"xEND"));
    }

    #[test]
    fn stderr_tail_cuts_long_output_on_a_char_boundary() {
        // Multi-byte characters straddle every possible cut point.
        let long = "é".repeat(STDERR_TAIL_BYTES);
        let tail = stderr_tail(long.as_bytes());
        assert!(tail.starts_with('…'), "{tail}");
        assert!(!tail.contains('\u{FFFD}'), "cut mid-character");
        assert!(tail.len() <= STDERR_TAIL_BYTES + '…'.len_utf8());
    }
}
