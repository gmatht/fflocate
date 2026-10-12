//! `findexd` — the findex daemon binary.
//!
//! Usage:
//!
//! ```text
//! findexd [--socket PATH] [--state-dir DIR] [--root DIR]... [--backend fanotify|inotify|fake|auto] [--shared-store] [--foreground]
//! ```
//!
//! With no `--root` (and no `$FINDEX_ROOT`), every readable native filesystem
//! is indexed on Linux (§4); elsewhere `$HOME` is. The daemon binds its
//! socket, installs watches, crawls in the background, and serves NDJSON
//! requests until `SIGTERM`/`SIGINT`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[cfg(unix)]
use findex::server::ServerCtx;
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
                cfg.backend = BackendChoice::parse(&b).ok_or_else(|| {
                    format!("unknown backend: {b} (want fanotify|inotify|fake|auto)")
                })?;
            }
            "--system" => {
                cfg = cfg.with_system(true);
            }
            "--system-group" => {
                let g = it.next().ok_or("--system-group needs a gid or name")?;
                cfg.system_group = g
                    .parse::<u32>()
                    .or_else(|_| {
                        // Resolve the name via /etc/group.
                        let txt = std::fs::read_to_string("/etc/group").map_err(|_| "bad gid")?;
                        txt.lines()
                            .find_map(|l| {
                                let f: Vec<&str> = l.split(':').collect();
                                (f.len() > 2 && f[0] == g).then(|| f[2].parse::<u32>().ok())?
                            })
                            .ok_or("unknown group")
                    })
                    .map_err(|e: &str| format!("--system-group: {e}"))?;
            }
            "--shared-store" => {
                cfg = cfg.with_shared_store(true);
            }
            "--foreground" | "-f" => foreground = true,
            "--help" | "-h" => {
                println!(
                    "findexd [--socket PATH] [--state-dir DIR] [--root DIR]... \
                     [--backend fanotify|inotify|fake|auto] [--shared-store] [--foreground]\n\
                     findexd --system [--system-group GID|NAME] [--root DIR]... \
                     # system instance: /run/findexd.sock, per-entry ownership"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if !roots.is_empty() {
        cfg.roots = roots;
    } else {
        // No `--root` flags: `$FINDEX_ROOT` (already applied by
        // `Config::defaults`) still wins when set; otherwise discover this
        // boot's filesystems. Discovery runs here — not in `defaults()` — so
        // an explicit configuration never pays for mountinfo it ignores.
        let findex_root_unset = match std::env::var_os("FINDEX_ROOT") {
            None => true,
            Some(v) => v.is_empty(),
        };
        if findex_root_unset {
            cfg.roots = Config::default_roots();
        }
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

    // System instances are unix-only (group-owned sockets, SO_PEERCRED):
    // refuse anywhere else rather than serve every query as nobody.
    #[cfg(not(unix))]
    if cfg.system {
        return Err(anyhow::anyhow!("--system is only supported on unix"));
    }
    #[cfg(unix)]
    let listener = if cfg.system {
        // System instance: resolve the group *before* binding, and refuse to run
        // without it — a socket we cannot properly own is worse than no daemon.
        let gid = if cfg.system_group != 0 {
            cfg.system_group
        } else {
            Config::findex_group_gid().ok_or_else(|| {
                anyhow::anyhow!(
                    "--system needs the `findex` group (or --system-group GID); \
                     create it with: groupadd --system findex"
                )
            })?
        };
        match net::bind_system(&socket, Some(gid)) {
            Ok(l) => l,
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                eprintln!("findexd: {e}");
                std::process::exit(0);
            }
            Err(e) => return Err(e.into()),
        }
    } else {
        match net::bind(&socket) {
            Ok(l) => l,
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                eprintln!("findexd: {e}");
                std::process::exit(0); // auto-spawn contract: exit quietly if up
            }
            Err(e) => return Err(e.into()),
        }
    };
    #[cfg(not(unix))]
    let listener = match net::bind(&socket) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            eprintln!("findexd: {e}");
            std::process::exit(0); // auto-spawn contract: exit quietly if up
        }
        Err(e) => return Err(e.into()),
    };

    let mut daemon = Daemon::build(cfg)?;
    // System mode captures ownership as it crawls; this is the `lstat` per entry
    // the personal mode deliberately avoids (§3).
    if daemon.is_system() {
        daemon.enable_ownership_tracking();
    }
    daemon.start()?;
    let ctx = Arc::new(daemon.server_ctx());
    let system = daemon.is_system();

    // Signal handling: SIGTERM/SIGINT set the stop flag.
    let stop = Arc::new(AtomicBool::new(false));
    install_signal_handlers(stop.clone());

    // Accept loop on its own thread; the main thread runs the applier.
    // `--system` is refused on non-unix above, so only unix can take the
    // system branch (whose socket code has no Windows meaning).
    #[cfg(unix)]
    {
        let stop = stop.clone();
        if system {
            // A system instance answers every connection as the user the kernel
            // says is on the other end (§3).
            let shared = daemon.shared.clone();
            let sync = daemon.sync.clone();
            let backend = daemon.backend_name();
            let control = daemon.control_sender();
            let make: Arc<
                dyn Fn(Option<findex::ownership::Caller>) -> Arc<ServerCtx> + Send + Sync,
            > = Arc::new(move |caller| {
                Arc::new(ServerCtx {
                    shared: shared.clone(),
                    sync: sync.clone(),
                    backend: backend.clone(),
                    control: Some(control.clone()),
                    caller,
                })
            });
            std::thread::spawn(move || net::accept_loop_system(listener, make, stop));
        } else {
            std::thread::spawn(move || net::accept_loop(listener, ctx, stop));
        }
    }
    #[cfg(not(unix))]
    {
        let stop = stop.clone();
        debug_assert!(!system, "--system must have been refused above");
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
    #[cfg(unix)]
    {
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
    // No libc signals on Windows: Ctrl-C terminates the process without a
    // graceful drain (the journal replays on next start, §9). The stop flag
    // stays for a future console handler.
    #[cfg(windows)]
    {
        let _ = stop;
    }
}
