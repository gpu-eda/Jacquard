//! Integration tests for `locate_and_check`: hard-error paths with stub
//! binaries that simulate "missing", "too-old", "too-new", and "version
//! probe failed" flavours of OpenSTA. WS-RH.1.

#![cfg(unix)]

use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::PathBuf;

use opensta_to_ir::opensta::{
    locate_and_check, LocateError, MAX_TESTED_OPENSTA_VERSION, MIN_TESTED_OPENSTA_VERSION,
};
use tempfile::TempDir;

/// Write `script` to `dir/sta`, mark it executable, and return the path.
///
/// Opens the file with mode 0o755 in a single syscall, `sync_all`s, and
/// drops the writer — then briefly polls a no-op `execve` until the
/// kernel stops returning ETXTBSY ("Text file busy"). Without the poll,
/// Linux occasionally rejects an immediate execve even after the writer
/// is closed (the inode's writer_count decrement is not synchronous with
/// `close(2)`), which surfaces in `locate_and_check` as `VersionProbeFailed
/// { kind: ExecutableFileBusy }` and breaks the test's exit-code
/// assertions.
fn write_stub_script(dir: &TempDir, script: &str) -> PathBuf {
    let path = dir.path().join("sta");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o755)
        .open(&path)
        .unwrap();
    f.write_all(script.as_bytes()).unwrap();
    f.sync_all().unwrap();
    drop(f);
    wait_for_execve_ready(&path);
    path
}

/// Block (briefly) until `path` can be `execve`'d without ETXTBSY.
fn wait_for_execve_ready(path: &std::path::Path) {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match Command::new(path)
            .arg("--__probe-ready")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                let _ = child.wait();
                return;
            }
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                if Instant::now() > deadline {
                    panic!("execve still returning ETXTBSY after 2s for {path:?}");
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("unexpected spawn error probing {path:?}: {e}"),
        }
    }
}

/// Stub that prints the given version on `-version` and exits 0.
fn write_version_stub(dir: &TempDir, version_output: &str) -> PathBuf {
    let script = format!(
        "#!/usr/bin/env bash\nif [ \"$1\" = \"-version\" ]; then\n  printf '%s' '{version_output}'\n  exit 0\nfi\nexit 0\n",
    );
    write_stub_script(dir, &script)
}

/// Stub that exits non-zero on `-version`.
fn write_failing_stub(dir: &TempDir) -> PathBuf {
    write_stub_script(
        dir,
        "#!/usr/bin/env bash\nif [ \"$1\" = \"-version\" ]; then\n  echo 'oops' >&2\n  exit 1\nfi\nexit 0\n",
    )
}

#[test]
fn locate_returns_not_found_for_nonexistent_path() {
    let nope = std::path::Path::new("/definitely/does/not/exist/sta");
    let err = locate_and_check(Some(nope)).unwrap_err();
    assert!(matches!(err, LocateError::NotFound));
}

#[test]
fn locate_accepts_min_tested_version() {
    let dir = TempDir::new().unwrap();
    let stub = write_version_stub(&dir, &format!("{MIN_TESTED_OPENSTA_VERSION}"));
    let located = locate_and_check(Some(&stub)).expect("min tested version should be accepted");
    assert_eq!(located.version, MIN_TESTED_OPENSTA_VERSION);
}

#[test]
fn locate_rejects_too_old_version() {
    assert!(
        MIN_TESTED_OPENSTA_VERSION.minor > 0 || MIN_TESTED_OPENSTA_VERSION.patch > 0,
        "test needs adjusting if MIN_TESTED is at a major boundary"
    );
    let too_old = format!(
        "{}.{}.{}",
        MIN_TESTED_OPENSTA_VERSION.major,
        MIN_TESTED_OPENSTA_VERSION.minor.saturating_sub(1),
        0
    );
    let dir = TempDir::new().unwrap();
    let stub = write_version_stub(&dir, &too_old);
    let err = locate_and_check(Some(&stub)).unwrap_err();
    match err {
        LocateError::TooOld {
            found, required, ..
        } => {
            assert!(found < required);
        }
        other => panic!("expected TooOld, got {other:?}"),
    }
}

#[test]
fn locate_flags_newer_than_tested() {
    let too_new = format!("{}.0.0", MAX_TESTED_OPENSTA_VERSION.major + 1);
    let dir = TempDir::new().unwrap();
    let stub = write_version_stub(&dir, &too_new);
    let located = locate_and_check(Some(&stub)).expect("newer-than-tested should still locate");
    assert!(located.version > MAX_TESTED_OPENSTA_VERSION);
}

#[test]
fn locate_reports_unparseable_version_output() {
    let dir = TempDir::new().unwrap();
    let stub = write_version_stub(&dir, "this is not a version string");
    let err = locate_and_check(Some(&stub)).unwrap_err();
    assert!(matches!(err, LocateError::VersionUnparseable { .. }));
}

#[test]
fn locate_reports_failing_version_probe() {
    let dir = TempDir::new().unwrap();
    let stub = write_failing_stub(&dir);
    let err = locate_and_check(Some(&stub)).unwrap_err();
    match err {
        LocateError::VersionProbeNonZero { .. } => {}
        other => panic!("expected VersionProbeNonZero, got {other:?}"),
    }
}
