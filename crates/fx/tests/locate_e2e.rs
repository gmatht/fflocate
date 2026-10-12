//! `locate` compatibility end-to-end: invoke the `ff` binary through a
//! `locate` symlink (argv[0] dispatch) against a real daemon and assert
//! GNU-locate behaviour: substring/glob/regex matching, flags, exit codes.
//!
//! Needs release binaries like the other e2e suites: gated behind
//! `FINDEX_BIN_DIR` (set by `scripts/e2e.sh`); without it the tests skip.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn bin_dir() -> Option<PathBuf> {
    let d = std::env::var("FINDEX_BIN_DIR").ok().map(PathBuf::from)?;
    (d.join(format!("findexd{}", std::env::consts::EXE_SUFFIX))
        .exists()
        && d.join(format!("ff{}", std::env::consts::EXE_SUFFIX))
            .exists())
    .then_some(d)
}

fn backend_arg() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "inotify"
    }
    #[cfg(not(target_os = "linux"))]
    {
        "fake"
    }
}

/// Isolated IPC endpoint for this test. Unix socket file everywhere except
/// Windows, where it is loopback TCP text (no unix sockets). The port is
/// OS-assigned per call (bind-then-release; the millisecond TOCTOU is
/// accepted test-harness practice).
fn test_socket(dir: &std::path::Path) -> PathBuf {
    #[cfg(windows)]
    {
        let _ = dir;
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("ephemeral port");
        let port = l.local_addr().expect("bound addr").port();
        PathBuf::from(format!("127.0.0.1:{port}"))
    }
    #[cfg(not(windows))]
    {
        dir.join("findex.sock")
    }
}

/// Canonical comparison form: verbatim `\\?\` prefixes stripped, separators
/// normalized to `/`, so OS-built expected paths compare equal to the
/// index's `/`-canonical reported paths. Identity off Windows.
fn canon(p: &std::path::Path) -> String {
    let s = p.to_string_lossy().into_owned();
    #[cfg(not(windows))]
    {
        s
    }
    #[cfg(windows)]
    {
        let s = s
            .strip_prefix(r"\\?\UNC\")
            .map(|r| format!(r"\\{r}"))
            .or_else(|| s.strip_prefix(r"\\?\").map(str::to_string))
            .unwrap_or(s);
        s.replace('\\', "/")
    }
}

/// Spawn a daemon over `root`, and symlink `ff` to `locate` in a fresh dir.
/// Returns (daemon child, socket, locate path).
fn setup(root: &Path, tag: &str) -> (std::process::Child, PathBuf, PathBuf) {
    let bindir = bin_dir().unwrap();
    let d = tempfile::tempdir().unwrap();
    let state = d.path().join("state");
    let socket = test_socket(d.path());
    // Keep the tempdir alive via the returned paths' parent: leak it like the
    // fault suites do by forgetting (the daemon holds state under it).
    let dir = d.keep();
    let child = Command::new(bindir.join(format!("findexd{}", std::env::consts::EXE_SUFFIX)))
        .arg("--socket")
        .arg(&socket)
        .arg("--state-dir")
        .arg(&state)
        .arg("--root")
        .arg(root)
        .arg("--backend")
        .arg(backend_arg())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn findexd");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let linkdir = dir.join(format!("bin-{tag}"));
    std::fs::create_dir_all(&linkdir).unwrap();
    // Symlink on unix; plain copy as locate.exe on Windows (symlinks need
    // privileges there) — argv[0] accepts the `.exe` form.
    #[cfg(unix)]
    let locate = {
        let l = linkdir.join("locate");
        std::os::unix::fs::symlink(
            bindir.join(format!("ff{}", std::env::consts::EXE_SUFFIX)),
            &l,
        )
        .unwrap();
        l
    };
    #[cfg(windows)]
    let locate = {
        let l = linkdir.join("locate.exe");
        std::fs::copy(
            bindir.join(format!("ff{}", std::env::consts::EXE_SUFFIX)),
            &l,
        )
        .unwrap();
        l
    };
    (child, socket, locate)
}

fn kill(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Settle the index via the real `ff`, then run the symlinked `locate`.
fn locate(locate_bin: &Path, socket: &Path, fxbin: &Path, args: &[&str]) -> (String, String, i32) {
    // Deterministic settle first (locate itself has no sync flag).
    let _ = Command::new(fxbin)
        .args(["--sync", "", "-l", "1", ""])
        .env("FINDEX_SOCKET", socket)
        .output();
    let out = Command::new(locate_bin)
        .args(args)
        .env("FINDEX_SOCKET", socket)
        .output()
        .expect("run locate");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

fn lines(s: &str) -> Vec<String> {
    s.lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

fn fixture_tree() -> (tempfile::TempDir, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("tree");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("foo.c"), b"x").unwrap();
    std::fs::write(root.join("bar.txt"), b"x").unwrap();
    std::fs::write(root.join("sub").join("baz.rs"), b"x").unwrap();
    std::fs::write(root.join("sub").join("notes.txt"), b"x").unwrap();
    (d, root)
}

#[test]
fn locate_substring_glob_and_regex() {
    let Some(bindir) = bin_dir() else {
        eprintln!("skipping: set FINDEX_BIN_DIR to target/release (see scripts/e2e.sh)");
        return;
    };
    let (_tree, root) = fixture_tree();
    let (mut daemon, socket, locate_bin) = setup(&root, "basic");
    let fxbin = bindir.join(format!("ff{}", std::env::consts::EXE_SUFFIX));

    // Bare substring behaves as *PATTERN*.
    let (out, _, code) = locate(&locate_bin, &socket, &fxbin, &["foo"]);
    assert_eq!(code, 0);
    assert_eq!(lines(&out), vec![canon(&root.join("foo.c"))]);

    // Shell glob matched against the whole path.
    let (out, _, code) = locate(&locate_bin, &socket, &fxbin, &["*.rs"]);
    assert_eq!(code, 0);
    assert_eq!(lines(&out), vec![canon(&root.join("sub/baz.rs"))]);

    // Glob does not leak across the anchor: '*.txt' at any depth, but only .txt.
    let (out, _, _) = locate(&locate_bin, &socket, &fxbin, &["*.txt"]);
    let mut got = lines(&out);
    got.sort();
    assert_eq!(
        got,
        vec![
            canon(&root.join("bar.txt")),
            canon(&root.join("sub/notes.txt")),
        ]
    );

    // Explicit regex, unanchored.
    let (out, _, code) = locate(&locate_bin, &socket, &fxbin, &["-r", "ba.*\\.txt$"]);
    assert_eq!(code, 0);
    assert_eq!(lines(&out), vec![canon(&root.join("bar.txt"))]);

    // No match → exit 1, no output.
    let (out, _, code) = locate(&locate_bin, &socket, &fxbin, &["no-such-file-xyz"]);
    assert_eq!(code, 1);
    assert!(out.is_empty());

    kill(&mut daemon);
}

#[test]
fn locate_flags_count_limit_case_all_basename() {
    let Some(bindir) = bin_dir() else {
        eprintln!("skipping: set FINDEX_BIN_DIR to target/release (see scripts/e2e.sh)");
        return;
    };
    let (_tree, root) = fixture_tree();
    let (mut daemon, socket, locate_bin) = setup(&root, "flags");
    let fxbin = bindir.join(format!("ff{}", std::env::consts::EXE_SUFFIX));
    let hit = |p: &str| canon(&root.join(p));

    // -c counts.
    let (out, _, code) = locate(&locate_bin, &socket, &fxbin, &["-c", ".txt"]);
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "2");

    // -l truncates.
    let (out, _, code) = locate(&locate_bin, &socket, &fxbin, &["-l", "1", ".txt"]);
    assert_eq!(code, 0);
    assert_eq!(lines(&out).len(), 1);

    // -i is case-insensitive; default is sensitive.
    let (out, _, code) = locate(&locate_bin, &socket, &fxbin, &["FOO"]);
    assert_eq!(code, 1);
    assert!(out.is_empty());
    let (out, _, code) = locate(&locate_bin, &socket, &fxbin, &["-i", "FOO"]);
    assert_eq!(code, 0);
    assert_eq!(lines(&out), vec![hit("foo.c")]);

    // Multiple patterns OR by default; -A ANDs them.
    let (out, _, _) = locate(&locate_bin, &socket, &fxbin, &["foo", "bar"]);
    assert_eq!(lines(&out).len(), 2);
    let (out, _, code) = locate(&locate_bin, &socket, &fxbin, &["-A", "foo", ".c"]);
    assert_eq!(code, 0);
    assert_eq!(lines(&out), vec![hit("foo.c")]);

    // -b scopes to the basename: 'sub/*.txt' as a basename glob matches
    // nothing, while '*.txt' matches both .txt files.
    let (out, _, code) = locate(&locate_bin, &socket, &fxbin, &["-b", "sub/*.txt"]);
    assert_eq!(code, 1);
    assert!(out.is_empty());
    let (out, _, code) = locate(&locate_bin, &socket, &fxbin, &["-b", "*.txt"]);
    assert_eq!(code, 0);
    assert_eq!(lines(&out).len(), 2);

    // -0 separates with NUL.
    let (out, _, code) = locate(&locate_bin, &socket, &fxbin, &["-0", "foo"]);
    assert_eq!(code, 0);
    assert_eq!(out.as_bytes(), format!("{}\0", hit("foo.c")).as_bytes());

    kill(&mut daemon);
}

#[test]
#[cfg(unix)]
fn locate_existing_filters_dangling_symlink() {
    let Some(bindir) = bin_dir() else {
        eprintln!("skipping: set FINDEX_BIN_DIR to target/release (see scripts/e2e.sh)");
        return;
    };
    let (_tree, root) = fixture_tree();
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("gone-target"), root.join("dangling")).unwrap();
    let (mut daemon, socket, locate_bin) = setup(&root, "existing");
    let fxbin = bindir.join(format!("ff{}", std::env::consts::EXE_SUFFIX));
    let hit = canon(&root.join("dangling"));

    // The symlink itself is indexed, so plain locate lists it.
    let (out, _, code) = locate(&locate_bin, &socket, &fxbin, &["dangling"]);
    assert_eq!(code, 0);
    assert_eq!(lines(&out), vec![hit.clone()]);

    // -e with the default -P (symlink_metadata) still lists it: the link
    // exists even though its target does not.
    let (out, _, code) = locate(&locate_bin, &socket, &fxbin, &["-e", "dangling"]);
    assert_eq!(code, 0);
    assert_eq!(lines(&out), vec![hit]);

    // -e -L follows the link: dangling target → filtered out, exit 1.
    let (out, _, code) = locate(&locate_bin, &socket, &fxbin, &["-e", "-L", "dangling"]);
    assert_eq!(code, 1);
    assert!(out.is_empty());

    kill(&mut daemon);
}

#[test]
fn locate_stats_and_help_do_not_search() {
    let Some(bindir) = bin_dir() else {
        eprintln!("skipping: set FINDEX_BIN_DIR to target/release (see scripts/e2e.sh)");
        return;
    };
    let (_tree, root) = fixture_tree();
    let (mut daemon, socket, locate_bin) = setup(&root, "meta");
    let fxbin = bindir.join(format!("ff{}", std::env::consts::EXE_SUFFIX));

    let (out, _, code) = locate(&locate_bin, &socket, &fxbin, &["-S"]);
    assert_eq!(code, 0);
    assert!(out.contains("entries"), "stats output: {out}");

    let (out, _, code) = locate(&locate_bin, &socket, &fxbin, &["--help"]);
    assert_eq!(code, 0);
    assert!(out.contains("USAGE"), "help output");

    // Unknown flags are usage errors (exit 2), like ff.
    let (_, _, code) = locate(&locate_bin, &socket, &fxbin, &["-Z"]);
    assert_eq!(code, 2);

    kill(&mut daemon);
}
