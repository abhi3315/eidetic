//! The deadline runner against stub "media tools" written as shell scripts.
//! Run as `sh <script> <dir>` so nothing executes a just-written file.
#![cfg(unix)]

use eidetic_ingest::Error;
use eidetic_ingest::subprocess::{self, Budget, STDERR_GRACE, STDERR_TAIL_BYTES};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(2);

fn stub(dir: &Path, script: &str) -> Command {
    let path = dir.join("stub.sh");
    std::fs::write(&path, script).unwrap();
    let mut cmd = Command::new("sh");
    cmd.arg(&path).arg(dir);
    cmd
}

fn read_pid(path: PathBuf) -> String {
    std::fs::read_to_string(path).unwrap().trim().to_string()
}

/// `kill -0`: does a process with this pid exist (a zombie still does)?
fn alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn kill(pid: &str) {
    let _ = Command::new("kill").args(["-9", pid]).status();
}

#[test]
fn a_child_that_never_exits_is_killed_and_reaped_alone() {
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = stub(
        dir.path(),
        "sleep 600 </dev/null >/dev/null 2>&1 & echo $! > \"$1/sibling.pid\"\n\
         echo $$ > \"$1/child.pid\"\n\
         echo started >&2\n\
         exec sleep 600\n",
    );
    let subject = dir.path().join("corrupt.mp4");
    let start = Instant::now();
    let err = subprocess::run(&mut cmd, &subject, DEADLINE).unwrap_err();
    let took = start.elapsed();

    let sibling = read_pid(dir.path().join("sibling.pid"));
    let still_running = alive(&sibling);
    kill(&sibling);

    let Error::SubprocessTimeout { elapsed, .. } = &err else {
        panic!("expected a timeout, got {err}");
    };
    assert!(*elapsed >= DEADLINE, "{elapsed:?}");
    assert!(took < Duration::from_secs(8), "{took:?}");
    let msg = err.to_string();
    assert!(msg.starts_with("sh on "), "{msg}");
    assert!(msg.contains(&subject.display().to_string()), "{msg}");
    assert!(msg.contains("killed after 2."), "{msg}");
    assert!(msg.contains("started"), "stderr tail: {msg}");

    let child = read_pid(dir.path().join("child.pid"));
    assert!(!alive(&child), "child {child} was killed and reaped");
    assert!(still_running, "only the child was killed, not its sibling");
}

#[test]
fn a_failing_child_reports_a_bounded_stderr_tail() {
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = stub(
        dir.path(),
        "i=0; while [ $i -lt 5000 ]; do echo \"line $i of noise\" >&2; i=$((i+1)); done\n\
         echo 'final: decoder exploded' >&2\n\
         exit 3\n",
    );
    let err = subprocess::run(&mut cmd, &dir.path().join("in.mov"), DEADLINE * 5).unwrap_err();
    assert!(matches!(err, Error::SubprocessFailed { .. }), "{err}");
    let msg = err.to_string();
    assert!(msg.contains("final: decoder exploded"), "{msg}");
    assert!(msg.contains("exit status: 3"), "{msg}");
    assert!(!msg.contains("line 0 "), "tail only: {msg}");
    assert!(msg.len() < STDERR_TAIL_BYTES + 512, "{} bytes", msg.len());
}

#[test]
fn a_clean_exit_with_stderr_held_open_is_not_a_truncated_success() {
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = stub(
        dir.path(),
        "sleep 30 </dev/null >/dev/null & echo $! > \"$1/holder.pid\"\nexit 0\n",
    );
    let start = Instant::now();
    let result = subprocess::run(
        &mut cmd,
        &dir.path().join("in.mov"),
        Duration::from_secs(20),
    );
    let took = start.elapsed();
    kill(&read_pid(dir.path().join("holder.pid")));

    let err = result.unwrap_err();
    let Error::Io { source, .. } = &err else {
        panic!("expected a pipes-held error, got {err}");
    };
    assert_eq!(source.kind(), std::io::ErrorKind::TimedOut, "{err}");
    assert!(took < STDERR_GRACE + Duration::from_secs(3), "{took:?}");
}

#[test]
fn a_child_that_closes_stdout_but_keeps_running_times_out() {
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = stub(dir.path(), "exec >&-\nexec sleep 600\n");
    let start = Instant::now();
    let err = subprocess::run(&mut cmd, &dir.path().join("in.mov"), DEADLINE).unwrap_err();
    assert!(matches!(err, Error::SubprocessTimeout { .. }), "{err}");
    assert!(start.elapsed() < Duration::from_secs(8));
}

#[test]
fn a_successful_run_hands_back_its_output() {
    let dir = tempfile::tempdir().unwrap();
    let subject = dir.path().join("in.mov");
    let out = subprocess::run(&mut stub(dir.path(), "printf hello\n"), &subject, DEADLINE).unwrap();
    assert_eq!(out.stdout, b"hello");

    let out = subprocess::run_with(
        &mut stub(dir.path(), "printf streamed; echo note >&2\n"),
        &subject,
        DEADLINE,
        |mut stdout| {
            let mut s = String::new();
            stdout.read_to_string(&mut s)?;
            Ok(s.len())
        },
    )
    .unwrap();
    assert_eq!(out.stdout, "streamed".len());
    assert_eq!(out.stderr, b"note\n");
}

#[test]
fn a_failing_reader_kills_and_reaps_the_child() {
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = stub(
        dir.path(),
        "echo $$ > \"$1/child.pid\"\nprintf x\nexec sleep 600\n",
    );
    let start = Instant::now();
    let err = subprocess::run_with(
        &mut cmd,
        &dir.path().join("in.mov"),
        DEADLINE * 5,
        |mut out| {
            // The first byte follows the pid file, so the stub has written it.
            out.read_exact(&mut [0u8; 1])?;
            Err::<(), _>(std::io::Error::other("bad samples"))
        },
    )
    .unwrap_err();
    assert!(matches!(err, Error::Io { .. }), "{err}");
    assert!(
        start.elapsed() < DEADLINE,
        "returned without waiting out the deadline"
    );
    assert!(!alive(&read_pid(dir.path().join("child.pid"))));
}

#[test]
fn budgets_scale_with_duration_and_never_panic() {
    let b = Budget::new(Duration::from_secs(10), 2.0);
    assert_eq!(b.deadline(Some(5.0)), Duration::from_secs(20));
    let assumed = b.deadline(None);
    assert_eq!(assumed, Duration::from_secs(10 + 2 * 3600));
    assert_eq!(b.deadline(Some(f64::NAN)), assumed);
    assert_eq!(b.deadline(Some(-3.0)), assumed);
    assert_eq!(b.deadline(Some(f64::INFINITY)), assumed);
    assert_eq!(
        b.deadline(Some(1e300)),
        Duration::from_secs(10 + 2 * 86_400)
    );
}
