//! The **daemon binary** must speak the fuzzy dialect the GUI sends.
//!
//! Every other fuzzy test in this repository calls `handle_request` in-process,
//! so all of them pass while a different `fflocated` — an installed release, an
//! old build still answering on the default socket — fails the same question.
//! That is one of the ways "the GUI found nothing for `FreeBiff` on macOS" can
//! happen with a current GUI, and it is invisible to a unit test.
//!
//! So this starts the daemon this checkout just built (`target/<profile>/fflocated`
//! or the profile directory cargo exported), puts a `FreeBuff` under a throwaway
//! root, and asks it the question the GUI asks — through `DaemonClient`, i.e. the
//! same socket protocol, the same quoting, the same `fuzzy`/`max_distance`
//! fields. The budgets are the daemon's default and the GUI's default.
//!
//! Runs on macOS only because that is where AppKit's absence of a working
//! distance entry made this the suspect; nothing here needs AppKit, so it can be
//! widened to any host with `fflocated` built.

#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use ff_gui::{effective_query, gather_options, DaemonClient, DEFAULT_MAX_DISTANCE};


    /// The daemon this repository built, not whatever is installed.
    fn daemon_binary() -> Option<PathBuf> {
        let name = "fflocated";
        let mut dirs: Vec<PathBuf> = Vec::new();
        // `cargo test -p ff-gui` does not build another package's binary, so the
        // step that runs this test builds fflocated first; these are the places
        // it lands (and `$CARGO_TARGET_DIR` when the runner moves the target).
        for profile in ["debug", "release"] {
            dirs.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target").join(profile));
        }
        if let Ok(target) = std::env::var("CARGO_TARGET_DIR") {
            for profile in ["debug", "release"] {
                dirs.push(PathBuf::from(&target).join(profile));
            }
        }
        dirs.into_iter().map(|d| d.join(name)).find(|p| p.is_file())
    }

    /// Kill the daemon even when an assertion below panics.
    struct Kill(Child);
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let exe = match daemon_binary() {
        Some(p) => p,
        None => {
            // Not a skip: this step builds the daemon, so a missing binary is a
            // broken step, not an absent prerequisite.
            panic!("no fflocated binary found; build it before running this test");
        }
    };
    let root = tempfile::tempdir()?;
    std::fs::create_dir_all(root.path().join("FreeBuff"))?;
    let state = tempfile::tempdir()?;
    let socket = state.path().join("daemon.sock");
    println!(
        "daemon fuzzy: starting {} on {} (root {})",
        exe.display(),
        socket.display(),
        root.path().display()
    );
    let child = Command::new(&exe)
        .arg("--foreground")
        .arg("--backend")
        .arg("fake")
        .arg("--root")
        .arg(root.path())
        .arg("--state-dir")
        .arg(state.path())
        .arg("--socket")
        .arg(&socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{} would not start: {e}", exe.display()))?;
    let _kill = Kill(child);

    // The client points at this daemon explicitly: an inherited
    // `$FFLOCATE_SOCKET` would otherwise send the question to whatever a
    // person happens to have running.

    // The question, exactly as the GUI builds it: Basic box text quoted, Fuzzy
    // ticked, the distance row at the app's default.
    let opts = gather_options(false, true, false, DEFAULT_MAX_DISTANCE);
    let raw = effective_query("FreeBiff", "").expect("quotable");
    println!("daemon fuzzy: sending {raw:?} with {opts:?}");

    let client = DaemonClient::with_socket(socket.clone());
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last = String::from("no reply yet");
    while Instant::now() < deadline {
        match client.search_with(&raw, 0, &opts) {
            Ok(outcome) if outcome.paths.iter().any(|p| p.ends_with("/FreeBuff")) => {
                println!(
                    "daemon fuzzy: ok — the daemon answered FreeBiff with {} ({} total)",
                    outcome.paths.join(", "),
                    outcome.total
                );
                return Ok(());
            }
            Ok(outcome) => {
                last = format!(
                    "{} hits, none named FreeBuff: {:?}",
                    outcome.total,
                    outcome.paths.iter().take(5).collect::<Vec<_>>()
                );
            }
            Err(e) => last = format!("{e}"),
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!(
        "the daemon at {} did not answer FreeBiff -> FreeBuff within 30s ({last}); \
         is this the fflocated this checkout built?",
        exe.display()
    );
}

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("daemon fuzzy: this walk starts a daemon binary; it runs on the macOS step");
}
