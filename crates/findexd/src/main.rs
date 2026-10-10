//! `findexd` — the findex daemon binary.
//!
//! Usage:
//!
//! ```text
//! findexd [--socket PATH] [--state-dir DIR] [--root DIR]... [--backend fanotify|inotify|fake|auto] [--foreground]
//! ```
//!
//! With no `--root`, `$HOME` is indexed. The daemon binds its socket, installs
//! watches, crawls in the background, and serves NDJSON requests until
//! `SIGTERM`/`SIGINT`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use findexd::config::{BackendChoice, Config};
use findexd::daemon::Daemon;
use findexd::net;

fn main() {
    if let Err(e) = run() {
        eprintln!("findexd: {e:#}");
        std::process::exit(1);
    }
}

fn parse_args() -> Result<Config, String> {
    let mut cfg = Config::defaults();
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut it = std::env::args().skip(1);
    let mut foreground = false;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--socket" => {
                let p = it.next().ok_or("--socket needs a path")?;
                cfg = cfg.with_socket(PathBuf::from(p));
            }
            "--state-dir" => {
                let p = it.next().ok_or("--state-dir needs a path")?;
                cfg = cfg.with_state_dir(PathBuf::from(p));
            }
            "--root" => {
                let p = it.next().ok_or("--root needs a path")?;
                roots.push(PathBuf::from(p));
            }
            "--backend" => {
                let b = it.next().ok_or("--backend needs a value")?;
                cfg.backend = BackendChoice::parse(&b)
                    .ok_or_else(|| format!("unknown backend: {b} (want fanotify|inotify|fake|auto)"))?;
            }
            "--foreground" | "-f" => foreground = true,
            "--help" | "-h" => {
                println!(
                    "findexd [--socket PATH] [--state-dir DIR] [--root DIR]... [--backend fanotify|inotify|fake|auto] [--foreground]"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if !roots.is_empty() {
        cfg.roots = roots;
    }
    std::env::set_var("FINDEX_FOREGROUND", if foreground { "1" } else { "0" });
    Ok(cfg)
}

fn run() -> anyhow::Result<()> {
    let cfg = parse_args().map_err(|e| anyhow::anyhow!(e))?;
    let socket = cfg.socket_path.clone();
    let state_dir = cfg.state_dir.clone();

    // Stale-lock recovery: one daemon per socket.
    let _pidfile = match net::PidFile::acquire(&cfg.pidfile) {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            // Retry once after a short pause: the holder may have just exited.
            std::thread::sleep(std::time::Duration::from_millis(100));
            net::PidFile::acquire(&cfg.pidfile)?
        }
        Err(e) => return Err(e.into()),
    };

    let listener = match net::bind(&socket) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            eprintln!("findexd: {e}");
            std::process::exit(0); // auto-spawn contract: exit quietly if up
        }
        Err(e) => return Err(e.into()),
    };

    let mut daemon = Daemon::build(cfg)?;
    daemon.start()?;
    let ctx = Arc::new(daemon.server_ctx());

    // Signal handling: SIGTERM/SIGINT set the stop flag.
    let stop = Arc::new(AtomicBool::new(false));
    install_signal_handlers(stop.clone());

    // Accept loop on its own thread; the main thread runs the applier.
    {
        let stop = stop.clone();
        std::thread::spawn(move || net::accept_loop(listener, ctx, stop));
    }

    // Run the applier until stopped.
    while !stop.load(Ordering::Relaxed) {
        let idle = daemon.tick();
        if idle {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    daemon.stop();
    let _ = std::fs::remove_file(&socket);
    let _ = state_dir;
    Ok(())
}

fn install_signal_handlers(stop: Arc<AtomicBool>) {
    use std::sync::OnceLock;
    static STOP: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    let _ = STOP.set(stop);
    extern "C" fn handler(_sig: libc::c_int) {
        if let Some(s) = STOP.get() {
            s.store(true, Ordering::Relaxed);
        }
    }
    let h = handler as extern "C" fn(libc::c_int) as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGTERM, h);
        libc::signal(libc::SIGINT, h);
    }
}
