//! Daemon results -> rendered rows -> captured pixels, on AppKit.
//!
//! The other macOS tests finish at the list's own state (a selection, a sort,
//! a scroll offset). This one starts where a real run starts — a daemon child
//! process answering a query — and finishes at a bitmap: the rows those hits
//! produce are fed into the same results list the app builds, laid out in a
//! real window, and captured through AppKit's display path
//! (`bitmapImageRepForCachingDisplayInRect:` + `cacheDisplayInRect:`, i.e.
//! `backends_macos_adapter::capture`, the primitive the app's own screenshot
//! path uses). Nothing here returns fixture pixels: the paths come from the
//! daemon's reply, and the assertions are about what was drawn.
//!
//! What it proves:
//!
//! * the capture has real dimensions and the list painted ink in it;
//! * a second, different result set changes the pixels where the rows are, so
//!   the glyphs on screen are the names of the hits that were searched for,
//!   not a fixed picture;
//! * an empty list is a visibly different image again (so "some pixels" is not
//!   just the window chrome).
//!
//! `harness = false`: AppKit has to be initialised on the process's main thread.

#[cfg(all(target_os = "macos", feature = "macos", not(feature = "zork")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use rswidgets::compat::CellValue;
    use rswidgets::prelude::*;

    // --- A daemon, the way the probe starts one ---
    let fixture = tempfile::tempdir()?;
    let root = fixture.path().join("root");
    std::fs::create_dir_all(root.join("sub"))?;
    for name in [
        "alpha-pixels-1.txt",
        "alpha-pixels-2.md",
        "sub/alpha-pixels-3.rs",
        "beta-pixels-1.txt",
        "beta-pixels-2.md",
        "sub/beta-pixels-3.rs",
        // A file neither query may match: three hits each, not four.
        "unrelated.rs",
    ] {
        std::fs::write(root.join(name), "pixel fixture\n")?;
    }
    let socket = fixture.path().join("ff.sock");
    // The test binary does not live in `target/debug` itself: cargo puts a
    // harness-free test under `target/debug/build/ff-gui/<hash>/out/`, so walk
    // up until the debug directory that holds `fflocated` turns up.
    let exe = std::env::current_exe()?;
    let mut found = None;
    let mut dir = exe.parent().map(|p| p.to_path_buf());
    for _ in 0..8 {
        let Some(d) = dir else { break };
        let candidate = d.join("fflocated");
        if candidate.is_file() {
            found = Some(candidate);
            break;
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
    let Some(daemon_bin) = found else {
        // No daemon in this build tree: the step that wants this check builds
        // `fflocated` first, so say why this run cannot and leave the job green.
        println!(
            "daemon_pixels_mac: no fflocated above {} (build it with `cargo build -p fflocated`); skipping",
            exe.display()
        );
        return Ok(());
    };
    struct DaemonGuard(Option<std::process::Child>);
    impl Drop for DaemonGuard {
        fn drop(&mut self) {
            if let Some(mut child) = self.0.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let mut daemon = DaemonGuard(Some(
        std::process::Command::new(&daemon_bin)
            .arg("--foreground")
            .arg("--backend")
            .arg("fake")
            .arg("--root")
            .arg(&root)
            .arg("--state-dir")
            .arg(fixture.path().join("state"))
            .arg("--socket")
            .arg(&socket)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?,
    ));

    // The GUI's own client, so the request is the one the app sends.
    let client = ff_gui::DaemonClient::with_socket(socket.clone());
    let search = |query: &str| -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let started = std::time::Instant::now();
        let mut last = String::new();
        while started.elapsed() < std::time::Duration::from_secs(30) {
            match client.search(query, 20) {
                Ok(outcome) if outcome.paths.len() == 3 => return Ok(outcome.paths),
                Ok(outcome) => last = format!("{} hit(s)", outcome.paths.len()),
                Err(e) => last = e.to_string(),
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        Err(format!("the daemon never returned the 3 {query:?} rows: {last}").into())
    };
    let alpha = search("alpha-pixels")?;
    let beta = search("beta-pixels")?;
    println!("pixels: daemon {} answered 3 hits for \"alpha-pixels\" and 3 for \"beta-pixels\"", daemon_bin.display());
    println!("pixels: alpha rows {alpha:?}");
    println!("pixels: beta rows {beta:?}");

    // --- The app's results list, in a real window ---
    let app = App::init()?;
    let win = app.create_window()?;
    win.set_title("ff-gui daemon pixels test");
    let list = app.create_treeview(&[("", true, true), ("Name", false, true), ("Path", false, true)])?;
    let painted = list.clone().into_scrolled()?;
    let column = app.create_box(Orientation::Vertical, 1)?;
    column.append(&painted);
    win.set_child(&column);
    win.present();
    let view = win.raw_handle();
    let cells = |paths: &[String]| -> Vec<Vec<CellValue>> {
        paths
            .iter()
            .map(|p| {
                let name = p.rsplit('/').next().unwrap_or(p).to_string();
                vec![
                    CellValue::Icon {
                        name: "text-x-generic".to_string(),
                        fallback: "R".to_string(),
                    },
                    CellValue::Text(name),
                    CellValue::Text(p.clone()),
                ]
            })
            .collect()
    };
    let shot = |rows: &[String]| -> Result<rswidgets::backends_macos_adapter::Shot, Box<dyn std::error::Error>> {
        list.set_rows(&cells(rows));
        let captured = rswidgets::backends_macos_adapter::capture(view, Some((900.0, 420.0)))
            .ok_or("the window produced no capture")?;
        Ok(captured)
    };

    let alpha_shot = shot(&alpha)?;
    println!(
        "pixels: captured {}x{} from the alpha rows: content ink {} px",
        alpha_shot.width, alpha_shot.height, alpha_shot.content_ink
    );
    assert!(
        alpha_shot.width >= 800 && alpha_shot.height >= 300,
        "the capture is {}x{}, which is not the window it asked for",
        alpha_shot.width,
        alpha_shot.height
    );
    assert!(
        alpha_shot.content_ink > 100,
        "the daemon's rows painted {} px: nothing reached the bitmap",
        alpha_shot.content_ink
    );
    for (row, path) in alpha.iter().enumerate() {
        let name = path.rsplit('/').next().unwrap_or(path);
        let drawn = list.cell_text(row, 1).unwrap_or_default();
        assert_eq!(
            drawn, name,
            "row {row} holds {drawn:?}, not the daemon hit {name:?}"
        );
    }

    // A different result set has to look different: same window, same layout,
    // different names in the row bands.
    let beta_shot = shot(&beta)?;
    assert_eq!(
        (beta_shot.width, beta_shot.height),
        (alpha_shot.width, alpha_shot.height),
        "the second result set changed the capture's size"
    );
    let changed = alpha_shot
        .rgba
        .iter()
        .zip(beta_shot.rgba.iter())
        .filter(|(a, b)| a != b)
        .count();
    println!(
        "pixels: the beta rows changed {changed} px of the {}x{} capture ({} ink)",
        beta_shot.width, beta_shot.height, beta_shot.content_ink
    );
    assert!(
        beta_shot.content_ink > 100,
        "the second result set painted {} px",
        beta_shot.content_ink
    );
    assert!(
        changed > 500,
        "swapping three rows for three different ones changed only {changed} px: the capture is not the rows"
    );

    // And an empty list is different again, so the ink above is the rows and
    // not the window's chrome.
    let empty_shot = shot(&[])?;
    let blank_changed = alpha_shot
        .rgba
        .iter()
        .zip(empty_shot.rgba.iter())
        .filter(|(a, b)| a != b)
        .count();
    println!(
        "pixels: the empty list differs from the alpha rows by {blank_changed} px ({} ink)",
        empty_shot.content_ink
    );
    assert!(
        blank_changed > 500,
        "an empty list changed only {blank_changed} px from the daemon's rows"
    );

    drop(daemon.0.take());
    println!(
        "pixels: ok — daemon rows rendered and captured, two result sets and an empty list all differ"
    );
    Ok(())
}

#[cfg(not(all(target_os = "macos", feature = "macos", not(feature = "zork"))))]
fn main() {
    // The capture is AppKit's bitmap caching; the GTK backend has no
    // pixel-capture primitive to assert on.
    println!("daemon_pixels_mac: needs the AppKit backend (macOS, `macos` on, `zork` off)");
}
