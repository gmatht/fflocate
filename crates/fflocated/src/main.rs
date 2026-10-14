//! `fflocated` — the fflocate daemon binary.
//!
//! Usage:
//!
//! ```text
//! fflocated [--socket PATH] [--state-dir DIR] [--root DIR]... [--backend fanotify|inotify|fake|auto] [--shared-store] [--foreground]
//! ```
//!
//! With no `--root` (and no `$FFLOCATE_ROOT`), every readable native filesystem
//! is indexed on Linux (§4); elsewhere `$HOME` is. The daemon binds its
//! socket, installs watches, crawls in the background, and serves NDJSON
//! requests until `SIGTERM`/`SIGINT`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[cfg(unix)]
use fflocate::server::ServerCtx;
use fflocated::config::{BackendChoice, Config};
use fflocated::daemon::Daemon;
use fflocated::net;

fn main() {
    if let Err(e) = run() {
        eprintln!("fflocated: {e:#}");
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
                    "fflocated [--socket PATH] [--state-dir DIR] [--root DIR]... \
                     [--backend fanotify|inotify|fake|auto] [--shared-store] [--foreground]\n\
                     fflocated --system [--system-group GID|NAME] [--root DIR]... \
                     # system instance: /run/fflocated.sock, per-entry ownership"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if !roots.is_empty() {
        cfg.roots = roots;
    } else {
        // No `--root` flags: `$FFLOCATE_ROOT` (already applied by
        // `Config::defaults`) still wins when set; otherwise discover this
        // boot's filesystems. Discovery runs here — not in `defaults()` — so
        // an explicit configuration never pays for mountinfo it ignores.
        let fflocate_root_unset = match std::env::var_os("FFLOCATE_ROOT") {
            None => true,
            Some(v) => v.is_empty(),
        };
        if fflocate_root_unset {
            cfg.roots = Config::default_roots();
        }
    }
    std::env::set_var("FFLOCATE_FOREGROUND", if foreground { "1" } else { "0" });
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
    // Resolve the system gid once, so both the initial bind and any
    // supervisor re-bind own the socket identically.
    #[cfg(unix)]
    let system_gid: Option<u32> = if cfg.system {
        Some(if cfg.system_group != 0 {
            cfg.system_group
        } else {
            Config::fflocate_group_gid().ok_or_else(|| {
                anyhow::anyhow!(
                    "--system needs the `fflocate` group (or --system-group GID); \
                     create it with: groupadd --system fflocate"
                )
            })?
        })
    } else {
        None
    };
    #[cfg(unix)]
    let listener = if cfg.system {
        // A socket we cannot properly own is worse than no daemon: refuse
        // without the group rather than silently widening the mode.
        match net::bind_system(&socket, system_gid) {
            Ok(l) => l,
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                eprintln!("fflocated: {e}");
                std::process::exit(0);
            }
            Err(e) => return Err(e.into()),
        }
    } else {
        match net::bind(&socket) {
            Ok(l) => l,
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                eprintln!("fflocated: {e}");
                std::process::exit(0); // auto-spawn contract: exit quietly if up
            }
            Err(e) => return Err(e.into()),
        }
    };
    #[cfg(not(unix))]
    let listener = match net::bind(&socket) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            eprintln!("fflocated: {e}");
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
    #[cfg(unix)]
    let system = daemon.is_system();

    // Signal handling: SIGTERM/SIGINT set the stop flag.
    let stop = Arc::new(AtomicBool::new(false));
    install_signal_handlers(stop.clone());

    // The accept loop runs on its own thread, supervised from the applier loop
    // below *and* the watcher thread: if it ever dies, the daemon is still
    // alive (pidfile held, index ticking) but serves nothing — the deaf-daemon
    // signature (§15). The supervisor re-binds and respawns it, bumping
    // `health.accept_restarts`. Either surviving thread revives the listener,
    // which is the point: a healthy applier with a wedged watcher still
    // recovers service, and vice versa. `--system` is refused on non-unix
    // above, so only unix can take the system branch (whose socket code has no
    // Windows meaning).
    #[cfg(unix)]
    let supervisor = if system {
        // A system instance answers every connection as the user the kernel
        // says is on the other end (§3).
        let shared = daemon.shared.clone();
        let sync = daemon.sync.clone();
        let backend = daemon.backend_name();
        let control = daemon.control_sender();
        let make: Arc<
            dyn Fn(Option<fflocate::ownership::Caller>) -> Arc<ServerCtx> + Send + Sync,
        > = Arc::new(move |caller| {
            Arc::new(ServerCtx {
                shared: shared.clone(),
                sync: sync.clone(),
                backend: backend.clone(),
                control: Some(control.clone()),
                caller,
            })
        });
        net::AcceptSupervisor::system(
            socket.clone(),
            listener,
            system_gid,
            make,
            daemon.shared.clone(),
            stop.clone(),
        )
    } else {
        net::AcceptSupervisor::personal(socket.clone(), listener, ctx, stop.clone())
    };
    #[cfg(not(unix))]
    let supervisor = {
        debug_assert!(
            !daemon.is_system(),
            "--system must have been refused above"
        );
        net::AcceptSupervisor::personal(socket.clone(), listener, ctx, stop.clone())
    };

    // Shared with the watcher thread (see `Daemon::set_accept_supervisor`):
    // the applier loop ticks it below, the watcher loop ticks it ~20x/s, and
    // whichever thread is still alive revives a dead listener. The lock is
    // held for the whole tick, but a tick never blocks indefinitely: the
    // common case is one `is_finished()` check, a reap joins an already-dead
    // thread (immediate), and a re-bind is fast syscalls plus a spawn — with
    // the 1 s backoff spanning ticks, never sleeping under the lock. A tick
    // can delay the other thread by milliseconds, never deadlock it.
    let accept_supervisor: net::SharedSupervisor =
        Arc::new(Mutex::new(supervisor));
    daemon.set_accept_supervisor(accept_supervisor.clone());

    // Run the applier until stopped; the supervisor rides along on every tick.
    loop {
        accept_supervisor.lock().unwrap().tick();
        if stop.load(Ordering::Relaxed) {
            break;
        }
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
