//! IPC transport for the NDJSON protocol (§11, §15): unix sockets on unix,
//! TCP loopback on Windows (see `findex::transport`).
//!
//! * Socket at `$XDG_RUNTIME_DIR/findex.sock` (0600), never `/tmp`.
//! * Stale-socket recovery: if the path exists but nobody is listening, unlink
//!   it and bind afresh (unix); on Windows a failed bind with nobody
//!   listening is a stale TCP state and surfaces as an error.
//! * The pidfile is `flock`ed so two daemons cannot race for the same socket
//!   (unix); on Windows the TCP bind itself is the exclusion.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use findex::applier::Shared;
use findex::proto::{Request, Response};
use findex::server::{handle_expr_streaming, handle_fuzzy_streaming, handle_query_streaming, handle_request, ServerCtx};
use findex::transport::{Listener, Stream};

/// Bind the listener, recovering a stale socket if present.
pub fn bind(socket: &Path) -> std::io::Result<Listener> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Some(parent) = socket.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if socket.exists() {
            // Is anyone home?
            match findex::transport::connect(socket) {
                Ok(_) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AddrInUse,
                        format!("another findexd is listening on {socket:?}"),
                    ));
                }
                Err(_) => {
                    // Stale socket: safe to remove.
                    let _ = std::fs::remove_file(socket);
                }
            }
        }
        let listener = Listener::bind(socket)?;
        // 0600: only the owner may talk to the daemon.
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
        Ok(listener)
    }
    #[cfg(windows)]
    {
        // Probe-then-bind, mirroring the unix stale-socket check: if someone
        // answers, the address is taken; otherwise bind (a bind failure with
        // nobody home is stale TCP state and is reported as-is).
        match findex::transport::connect(socket) {
            Ok(_) => Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                format!("another findexd is listening on {socket:?}"),
            )),
            Err(_) => Listener::bind(findex::transport::parse_addr(socket)?),
        }
    }
}

/// Bind the listener for a **system** instance (§3, §15).
///
/// Two differences from [`bind`]:
///
/// * the socket is `0660` and owned by the `findex` group, so only group members
///   may connect at all — a first line of defence before the per-query ownership
///   check, on the principle that an interface nobody can reach is an interface
///   nobody can exploit;
/// * the caller is established per connection by [`peer_credentials`], never from
///   anything the client says about itself.
///
/// `group` is the gid to own the socket. If it does not exist the daemon refuses
/// to start rather than silently falling back to a wider mode — a misconfigured
/// *system* instance must fail loudly.
///
/// Unix-only: group-owned sockets, `chown` and `SO_PEERCRED` have no Windows
/// meaning, so `--system` is refused there (see `main.rs`).
#[cfg(unix)]
pub fn bind_system(socket: &Path, group: Option<u32>) -> std::io::Result<Listener> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if socket.exists() {
        match Stream::connect(socket) {
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    format!("another findexd is listening on {socket:?}"),
                ));
            }
            Err(_) => {
                let _ = std::fs::remove_file(socket);
            }
        }
    }
    let listener = Listener::bind(socket)?;
    // 0660 + the findex group.
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o660))?;
    if let Some(gid) = group {
        use std::os::unix::ffi::OsStrExt;
        let Ok(c) = std::ffi::CString::new(socket.as_os_str().as_bytes()) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "socket path has NUL",
            ));
        };
        // chown requires privilege; a non-root daemon trying to run --system is a
        // configuration error, so surface it rather than pretend.
        let rc = unsafe { libc::chown(c.as_ptr(), u32::MAX, gid) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(listener)
}

/// The credentials of the process on the other end of `stream`, from the
/// kernel (Linux `SO_PEERCRED`, macOS `LOCAL_PEERCRED`).
///
/// This is the *only* trustworthy source of caller identity: it is filled in by
/// the kernel from the connected socket, so a client cannot claim to be someone
/// else by sending a field in a request. Everything the system daemon does about
/// permissions is downstream of this.
#[cfg(unix)]
pub fn peer_credentials(stream: &Stream) -> std::io::Result<findex::ownership::Caller> {
    use std::os::unix::io::AsRawFd;
    let fd = stream.as_raw_fd();
    #[cfg(target_os = "linux")]
    {
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                &mut cred as *mut _ as *mut libc::c_void,
                &mut len,
            )
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(findex::ownership::Caller::new(cred.uid, cred.gid))
    }
    #[cfg(target_os = "macos")]
    {
        // macOS has no SO_PEERCRED: peer credentials arrive as an `xucred`
        // (uid + advisory groups, no single primary gid — `cr_groups[0]` is
        // the conventional stand-in, matching the kernel's `cr_gid` macro).
        let mut cred: libc::xucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::xucred>() as libc::socklen_t;
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_LOCAL,
                libc::LOCAL_PEERCRED,
                &mut cred as *mut _ as *mut libc::c_void,
                &mut len,
            )
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let gid = if cred.cr_ngroups > 0 {
            cred.cr_groups[0]
        } else {
            0
        };
        Ok(findex::ownership::Caller::new(cred.cr_uid, gid))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = fd;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "peer credentials are only supported on Linux and macOS",
        ))
    }
}

/// Constructor for per-connection contexts in a system instance: the kernel's
/// answer for who is on the other end, mapped to what they may see.
#[cfg(unix)]
pub type SystemCtxFactory =
    Arc<dyn Fn(Option<findex::ownership::Caller>) -> Arc<ServerCtx> + Send + Sync>;

/// Shareable handle to an [`AcceptSupervisor`]. The applier loop and the
/// watcher thread each hold one, so *either* surviving thread can `tick` the
/// supervisor and revive a dead accept loop (§15 deaf-daemon watchdog).
pub type SharedSupervisor = Arc<Mutex<AcceptSupervisor>>;

/// Supervises the accept loop, respawning it if it dies (§15 deaf-daemon
/// watchdog).
///
/// The accept thread is fire-and-forget today: if it panics or returns early
/// (a `set_nonblocking` failure, a dropped listener), the process stays up —
/// pidfile held, applier ticking, watcher polling — but serves nothing. The
/// client already fails fast on that signature (`spawn::deaf_daemon_message`);
/// this closes the loop on the daemon side: as long as *any* thread that
/// calls [`AcceptSupervisor::tick`] is alive (the applier thread in
/// `findexd::main`), a dead accept thread is re-bound and respawned, and
/// `health.accept_restarts` records that it happened.
///
/// Re-bind reuses [`bind`]/[`bind_system`], so stale-socket recovery applies:
/// nobody listening means the old socket file is unlinked and bound afresh.
/// A bind failure (e.g. someone else took the address) is logged and retried
/// with a 1 s backoff — never a spin. Shutdown (`stop` set) never respawns.
///
/// Note on scope: the supervisor is ticked by the applier thread, which is
/// the right owner — if the applier itself is gone, serving is broken
/// regardless (it owns all index mutation), so a resurrected listener would
/// be theater. Conversely a single wedged *work item* (a minutes-long
/// `diff_subtree`) delays supervision until the drain returns: the deaf
/// window is bounded by the longest single item, not unbounded.

/// One supervised accept thread: its handle plus the generation flag it
/// polls. A fresh flag per generation means a replaced thread can always be
/// told to stop — by shutdown, or (in tests) to simulate a death — without
/// touching the daemon-wide shutdown flag that gates respawns.
struct AcceptThread {
    handle: JoinHandle<()>,
    /// The generation flag the loop polls. Written by `kill_current_for_test`
    /// (test builds); in production the loop exits on its own death and this
    /// is only read at join time — hence the allow.
    #[allow(dead_code)]
    stop: Arc<AtomicBool>,
}

pub struct AcceptSupervisor {
    socket: PathBuf,
    system: bool,
    system_gid: Option<u32>,
    ctx: Option<Arc<ServerCtx>>,
    #[cfg(unix)]
    make_ctx: Option<SystemCtxFactory>,
    shared: Arc<Shared>,
    /// Daemon shutdown: while set, `tick` stops the current thread but never
    /// respawns. The accept loop also polls a per-generation flag, so one
    /// generation can be stopped — shutdown, or a simulated death in tests —
    /// without tripping this daemon-wide gate.
    stop: Arc<AtomicBool>,
    /// The current generation, if any. `None` before the first spawn and
    /// whenever the thread is known dead-but-not-yet-respawned.
    current: Option<AcceptThread>,
    /// True once the first accept thread has been spawned: later spawns are
    /// restarts and bump `accept_restarts`.
    started: bool,
    last_attempt_ms: u64,
}

impl AcceptSupervisor {
    /// Supervise a personal (per-user) accept loop, taking ownership of the
    /// already-bound listener from startup (`main` binds before the daemon
    /// builds, so the auto-spawn "already running" contract holds). The
    /// initial thread is *not* a restart: `accept_restarts` stays 0 until a
    /// death is actually repaired.
    pub fn personal(
        socket: PathBuf,
        listener: Listener,
        ctx: Arc<ServerCtx>,
        stop: Arc<AtomicBool>,
    ) -> Self {
        let shared = ctx.shared.clone();
        let mut sup = AcceptSupervisor {
            socket,
            system: false,
            system_gid: None,
            ctx: Some(ctx),
            #[cfg(unix)]
            make_ctx: None,
            shared,
            stop,
            current: None,
            started: false,
            last_attempt_ms: 0,
        };
        sup.adopt(listener);
        sup
    }

    /// Supervise a system accept loop (unix only), taking ownership of the
    /// already-bound listener (see [`AcceptSupervisor::personal`]).
    #[cfg(unix)]
    pub fn system(
        socket: PathBuf,
        listener: Listener,
        system_gid: Option<u32>,
        make_ctx: SystemCtxFactory,
        shared: Arc<Shared>,
        stop: Arc<AtomicBool>,
    ) -> Self {
        let mut sup = AcceptSupervisor {
            socket,
            system: true,
            system_gid,
            ctx: None,
            make_ctx: Some(make_ctx),
            shared,
            stop,
            current: None,
            started: false,
            last_attempt_ms: 0,
        };
        sup.adopt_system(listener);
        sup
    }

    /// Spawn the accept thread around an already-bound listener. Used once at
    /// startup; every later spawn goes through `spawn_once` (re-bind) and
    /// counts as a restart. A spawn failure is logged loudly, sets the
    /// re-bind backoff, and is left for `tick` to retry — a dead listener
    /// with no thread must never be silent, nor a tight respawn spin.
    fn adopt(&mut self, listener: Listener) {
        debug_assert!(!self.system);
        let ctx = self.ctx.clone().expect("personal supervisor needs ctx");
        let daemon_stop = self.stop.clone();
        let gen_stop = Arc::new(AtomicBool::new(false));
        match std::thread::Builder::new()
            .name("findexd-accept".into())
            .spawn({
                let gen_stop = gen_stop.clone();
                move || accept_loop(listener, ctx, daemon_stop, gen_stop)
            }) {
            Ok(h) => {
                self.current = Some(AcceptThread {
                    handle: h,
                    stop: gen_stop,
                });
                // A successful adopt deliberately leaves `last_attempt_ms`
                // alone (it is zero): the *first* death must repair
                // immediately, with no 1 s backoff debt from startup. Only
                // actual respawn attempts (in `tick`) arm the backoff.
                self.started = true;
            }
            Err(e) => {
                eprintln!("findexd: accept supervisor: initial spawn failed: {e}; will retry");
                self.last_attempt_ms = findex::applier::now_ms();
            }
        }
    }

    #[cfg(unix)]
    fn adopt_system(&mut self, listener: Listener) {
        debug_assert!(self.system);
        let make = self.make_ctx.clone().expect("system supervisor needs make_ctx");
        let daemon_stop = self.stop.clone();
        let gen_stop = Arc::new(AtomicBool::new(false));
        match std::thread::Builder::new()
            .name("findexd-accept".into())
            .spawn({
                let gen_stop = gen_stop.clone();
                move || accept_loop_system(listener, make, daemon_stop, gen_stop)
            }) {
            Ok(h) => {
                self.current = Some(AcceptThread {
                    handle: h,
                    stop: gen_stop,
                });
                // As in `adopt`: success leaves the re-bind backoff disarmed
                // so the first death repairs immediately; only a failed
                // attempt arms it.
                self.started = true;
            }
            Err(e) => {
                eprintln!("findexd: accept supervisor: initial spawn failed: {e}; will retry");
                self.last_attempt_ms = findex::applier::now_ms();
            }
        }
    }

    /// Is there a live accept thread right now?
    pub fn is_alive(&self) -> bool {
        match &self.current {
            Some(t) => !t.handle.is_finished(),
            None => false,
        }
    }

    /// Stop the current accept generation without tripping daemon shutdown.
    /// Test-only hook: simulates the accept thread dying (the `set_nonblocking`
    /// failure / dropped-listener case) so a test can prove the supervisor
    /// re-binds and respawns. Never touches the daemon-wide `stop` flag, so
    /// the next `tick` treats this as a death to repair, not a shutdown.
    #[cfg(test)]
    pub fn kill_current_for_test(&mut self) {
        if let Some(t) = &self.current {
            t.stop.store(true, Ordering::Relaxed);
        }
    }

    /// Check the accept thread; respawn it if it died. Returns true when it
    /// spawned (or re-spawned) a thread on this call. Call it from the applier
    /// tick — the thread most likely to still be alive when the listener dies.
    ///
    /// [`SharedSupervisor`] lets the *watcher* tick it too: either surviving
    /// thread revives the listener, which is the point — a deaf daemon with a
    /// healthy applier *or* a healthy watcher still recovers.
    pub fn tick(&mut self) -> bool {
        if self.stop.load(Ordering::Relaxed) {
            return false;
        }
        let dead = match &self.current {
            None => true,
            Some(t) => t.handle.is_finished(),
        };
        if !dead {
            return false;
        }
        // Reap the old thread. The join result is the *only* record of why
        // the generation died — a panic payload, or a clean return after
        // `set_nonblocking` failed / the listener errored / shutdown — so
        // log it loudly before respawning. Without this a recurring death
        // (bad fd, poisoned listener) is invisible except for the restart
        // counter climbing.
        if let Some(t) = self.current.take() {
            match t.handle.join() {
                Ok(()) => {
                    if t.stop.load(Ordering::Relaxed) {
                        eprintln!(
                            "findexd: accept loop exited on generation stop \
                             (retired, not crashed); respawning"
                        );
                    } else {
                        eprintln!(
                            "findexd: accept loop returned cleanly (listener \
                             setup failure or accept error); respawning"
                        );
                    }
                }
                Err(e) => {
                    eprintln!(
                        "findexd: accept loop panicked; respawning: {}",
                        panic_payload(&e)
                    );
                }
            }
        }
        // Back off re-binds: a listener that dies in a tight loop (bad fd,
        // permanent bind failure) must not become a respawn spin.
        let now = findex::applier::now_ms();
        if self.started && now.saturating_sub(self.last_attempt_ms) < 1_000 {
            return false;
        }
        self.last_attempt_ms = now;
        self.spawn_once()
    }

    fn spawn_once(&mut self) -> bool {
        let listener = if self.system {
            #[cfg(unix)]
            {
                bind_system(&self.socket, self.system_gid)
            }
            #[cfg(not(unix))]
            {
                eprintln!("findexd: system mode is unix-only; accept loop not started");
                return false;
            }
        } else {
            bind(&self.socket)
        };
        let listener = match listener {
            Ok(l) => l,
            Err(e) => {
                eprintln!("findexd: accept supervisor: re-bind failed: {e}; retrying");
                return false;
            }
        };
        let daemon_stop = self.stop.clone();
        let gen_stop = Arc::new(AtomicBool::new(false));
        let spawned = if self.system {
            #[cfg(unix)]
            {
                let make = self.make_ctx.clone().expect("system supervisor needs make_ctx");
                match std::thread::Builder::new()
                    .name("findexd-accept".into())
                    .spawn({
                        let gen_stop = gen_stop.clone();
                        move || accept_loop_system(listener, make, daemon_stop, gen_stop)
                    }) {
                    Ok(h) => {
                        self.current = Some(AcceptThread {
                            handle: h,
                            stop: gen_stop,
                        });
                        true
                    }
                    Err(e) => {
                        eprintln!("findexd: accept supervisor: spawn failed: {e}; retrying");
                        false
                    }
                }
            }
            #[cfg(not(unix))]
            {
                false
            }
        } else {
            let ctx = self.ctx.clone().expect("personal supervisor needs ctx");
            match std::thread::Builder::new()
                .name("findexd-accept".into())
                .spawn({
                    let gen_stop = gen_stop.clone();
                    move || accept_loop(listener, ctx, daemon_stop, gen_stop)
                }) {
                Ok(h) => {
                    self.current = Some(AcceptThread {
                        handle: h,
                        stop: gen_stop,
                    });
                    true
                }
                Err(e) => {
                    eprintln!("findexd: accept supervisor: spawn failed: {e}; retrying");
                    false
                }
            }
        };
        if !spawned {
            return false;
        }
        if self.started {
            // One lock for the bump and the read: the count and the log line
            // can never disagree, and no stats reader slips between them.
            let n = {
                let mut h = self.shared.health.write().unwrap();
                h.accept_restarts += 1;
                h.accept_restarts
            };
            eprintln!("findexd: accept loop died; respawned (restarts={n})");
        }
        self.started = true;
        true
    }
}

/// Best-effort rendering of a panicked thread's payload for the daemon log.
/// Covers the three shapes a payload takes in practice: `&str` (plain
/// `panic!("...")`), `String` (formatted), and anything else (opaque address).
fn panic_payload(e: &Box<dyn std::any::Any + Send + 'static>) -> String {
    if let Some(s) = e.downcast_ref::<&str>() {
        return (*s).to_string();
    }
    if let Some(s) = e.downcast_ref::<String>() {
        return s.clone();
    }
    "<non-string panic payload>".to_string()
}

impl Drop for AcceptSupervisor {    fn drop(&mut self) {
        // Reap the current generation at daemon shutdown so no accept thread
        // is left detached at exit. Only while the daemon is stopping: the
        // accept loop polls the same flag every ~10 ms, so this join is
        // prompt. Dropping a *live* supervisor (tests, harnesses) never
        // blocks — the thread is left running.
        if self.stop.load(Ordering::Relaxed) {
            if let Some(t) = self.current.take() {
                let _ = t.handle.join();
            }
        }
    }
}

/// Serve one client connection of a **system** instance, answering every query as
/// the connecting user.
///
/// Never panics on a failed stream clone (a client that vanished mid-accept):
/// the handler returns and the accept loop keeps serving.
#[cfg(unix)]
pub fn serve_client_system(
    make_ctx: impl Fn(Option<findex::ownership::Caller>) -> Arc<ServerCtx>,
    stream: Stream,
) {
    // Identity first: if we cannot establish who is asking, we serve the context
    // with *no* caller, which in system mode means "show nothing" rather than
    // "show everything".
    let caller = peer_credentials(&stream).ok();
    let ctx = make_ctx(caller);
    let Ok(clone) = stream.try_clone() else {
        return;
    };
    let reader = BufReader::new(clone);
    let mut writer = stream;
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let req = match Request::from_line(&line) {
            Ok(r) => r,
            Err(e) => {
                let resp = Response::Error {
                    message: format!("bad request: {e}"),
                };
                let _ = writeln!(writer, "{}", resp.to_line());
                continue;
            }
        };
        if respond_streaming(&ctx, &req, &mut writer) {
            break;
        }
    }
}

/// Try to connect to an existing daemon.
pub fn connect(socket: &Path) -> std::io::Result<Stream> {
    findex::transport::connect(socket)
}

/// A pidfile guarded by an exclusive `flock`.
pub struct PidFile {
    path: PathBuf,
    /// Kept open so the `flock` survives for the process lifetime.
    #[allow(dead_code)]
    file: std::fs::File,
}

impl PidFile {
    /// Acquire the pidfile lock, or fail if held. Reclaims a stale pidfile
    /// whose owner is gone (§15 stale-lock recovery).
    pub fn acquire(path: &Path) -> std::io::Result<PidFile> {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)?;
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc != 0 {
                let e = std::io::Error::last_os_error();
                if e.raw_os_error() == Some(libc::EWOULDBLOCK) {
                    // Someone holds it. Check whether the owner is still alive.
                    let mut s = String::new();
                    use std::io::Read;
                    let mut f = file.try_clone()?;
                    let _ = f.read_to_string(&mut s);
                    if let Ok(pid) = s.trim().parse::<i32>() {
                        if process_alive(pid) {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::AddrInUse,
                                format!("findexd already running as pid {pid}"),
                            ));
                        }
                    }
                    // Stale lock: the holder is gone; the flock will be released by
                    // the kernel when its process died, so retry once.
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        "pidfile lock held; retry",
                    ));
                }
                return Err(e);
            }
            Self::write_pid(&file)?;
            Ok(PidFile {
                path: path.to_path_buf(),
                file,
            })
        }
        // No `flock` on Windows: the TCP bind is the mutual exclusion (the OS
        // releases it on crash, so no stale-lock class exists). The pidfile
        // still records the owner for operators.
        #[cfg(windows)]
        {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)?;
            Self::write_pid(&file)?;
            Ok(PidFile {
                path: path.to_path_buf(),
                file,
            })
        }
    }

    fn write_pid(file: &std::fs::File) -> std::io::Result<()> {
        let mut f = file;
        f.set_len(0)?;
        use std::io::Seek;
        f.seek(std::io::SeekFrom::Start(0))?;
        write!(f, "{}", std::process::id())?;
        f.sync_all()?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        // `self.file` closes here, releasing the flock.
    }
}

#[cfg(unix)]
fn process_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    unsafe { libc::kill(pid, 0) == 0 }
}


/// Is this a query-shaped request (one whose handler streams Hits)?
fn is_query(req: &Request) -> bool {
    matches!(req, Request::Query { .. })
}

/// Serve one parsed request, streaming Hits as found. Query handlers emit
/// Meta first, then Hits with a flush per record (a page is ~200 small lines
/// on loopback; per-record flush is what makes first paint immediate), then
/// Done with the exact total. Non-query ops go through the batched path.
/// Returns true when the connection closed mid-reply.
fn respond_streaming(ctx: &ServerCtx, req: &Request, writer: &mut Stream) -> bool {
    use std::io::Write as _;
    // Destructure the query fields once; every query shape streams.
    let query_args = match req {
        Request::Query {
            q,
            limit,
            count,
            expression,
            glob,
            glob_includedotfiles,
            glob_noescape,
            suffix,
            path,
            relative_root,
            case_sensitive,
            fuzzy,
            max_distance,
            ..
        } => Some((
            q.clone(),
            *limit,
            *count,
            expression.clone(),
            glob.clone(),
            glob_includedotfiles,
            glob_noescape,
            suffix.clone(),
            path.clone(),
            relative_root.clone(),
            *case_sensitive,
            *fuzzy,
            *max_distance,
        )),
        _ => None,
    };
    if let Some((
        q,
        limit,
        count,
        expression,
        glob,
        _glob_includedotfiles,
        _glob_noescape,
        suffix,
        path,
        relative_root,
        case_sensitive,
        fuzzy,
        max_distance,
    )) = query_args
    {
        // Rebuild the expression object the expr handler parses, mirroring
        // `handle_request`'s assembly exactly (same keys, same shape).
        // `false` from here aborts the in-flight scan: every streaming
        // handler stops at the first refusal and skips its `Done` (a partial
        // count would lie), so a client that disconnected mid-scan costs one
        // cheap write error, not a full scan to a dead socket.
        let mut closed = false;
        let mut emit = |r: Response| {
            if closed {
                return false;
            }
            if writeln!(writer, "{}", r.to_line()).is_err() {
                closed = true;
                return false;
            }
            // Flush per record: Hits leave the kernel immediately instead of
            // waiting for Done. Meta unblocks first paint; each Hit extends
            // the visible list while the total is still being counted.
            let _ = writer.flush();
            true
        };
        // std::time::Instant for t0 (server handlers take std Instant).
        let t0 = std::time::Instant::now();
        // The handler's bool is authoritative: `false` means the scan
        // aborted on a refused emit, which is exactly when the socket is
        // dead. `closed` usually agrees already (the closure sets it on the
        // failing write); OR-ing in the negated return covers the
        // Meta-refused case, where nothing was ever written but the client
        // is equally gone. (Handlers return `true` on success — hence the
        // negation: a completed scan leaves `closed` as the closure set it.)
        closed = if expression.is_some() || glob.is_some() || suffix.is_some() || path.is_some() {
            let mut obj = serde_json::Map::new();
            if let Some(e) = expression {
                obj.insert("expression".into(), e);
            }
            if let Some(g) = glob {
                obj.insert("glob".into(), g);
            }
            if let Some(s) = suffix {
                obj.insert("suffix".into(), s);
            }
            if let Some(p) = path {
                obj.insert("path".into(), p);
            }
            if let Some(rr) = relative_root {
                obj.insert("relative_root".into(), serde_json::Value::String(rr));
            }
            if let Some(cs) = case_sensitive {
                obj.insert("case_sensitive".into(), serde_json::Value::Bool(cs));
            }
            !handle_expr_streaming(
                ctx,
                &serde_json::Value::Object(obj),
                limit,
                count,
                t0,
                &mut emit,
            ) || closed
        } else if fuzzy {
            !handle_fuzzy_streaming(ctx, &q, max_distance, limit, count, t0, &mut emit) || closed
        } else {
            !handle_query_streaming(
                ctx,
                &q,
                limit,
                count,
                case_sensitive == Some(true),
                t0,
                &mut emit
            ) || closed
        };
        return closed;
    }
    let _ = is_query;
    let responses = handle_request(ctx, req);
    let mut closed = false;
    for r in &responses {
        if writeln!(writer, "{}", r.to_line()).is_err() {
            closed = true;
            break;
        }
    }
    let _ = writer.flush();
    closed
}

/// Serve one client connection until it closes.
///
/// Never panics on a failed stream clone (a client that vanished mid-accept):
/// the handler returns and the accept loop keeps serving.
pub fn serve_client(ctx: Arc<ServerCtx>, stream: Stream) {
    let Ok(clone) = stream.try_clone() else {
        return;
    };
    let reader = BufReader::new(clone);
    let mut writer = stream;
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let req = match Request::from_line(&line) {
            Ok(r) => r,
            Err(e) => {
                let resp = Response::Error {
                    message: format!("bad request: {e}"),
                };
                let _ = writeln!(writer, "{}", resp.to_line());
                continue;
            }
        };
        if respond_streaming(&ctx, &req, &mut writer) {
            break;
        }
    }
}

/// One request/response exchange over a fresh connection. Used by the CLI and
/// the tests.
pub fn roundtrip(socket: &Path, req: &Request) -> std::io::Result<Vec<Response>> {    let stream = findex::transport::connect(socket)?;
    let mut writer = stream.try_clone()?;
    writeln!(writer, "{}", serde_json::to_string(req).unwrap())?;
    writer.flush()?;
    let reader = BufReader::new(stream);
    let mut out = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(r) = serde_json::from_str::<Response>(&line) {
            let done = matches!(
                r,
                Response::Done { .. }
                    | Response::Error { .. }
                    | Response::Pong { .. }
                    | Response::Synced { .. }
                    | Response::Stats { .. }
                    | Response::Rescanning { .. }
            );
            out.push(r);
            if done {
                break;
            }
        }
    }
    Ok(out)
}

/// Run the accept loop, spawning a thread per client, until stopped.
///
/// Two stop conditions, deliberately separate: `daemon_stop` is the process
/// shutdown flag, while `gen_stop` ends *this generation only* — the
/// supervisor sets it (or tests do) to retire one accept thread without
/// tripping the daemon-wide gate that would suppress respawns.
///
/// Never panics on listener setup: a `set_nonblocking` failure is logged and
/// the thread returns (the supervisor in `findexd::main` re-binds and
/// respawns, bumping `health.accept_restarts`). Per-client spawn failures are
/// contained the same way — a dead accept thread must never be the end of
/// serving.
pub fn accept_loop(
    listener: Listener,
    ctx: Arc<ServerCtx>,
    daemon_stop: Arc<std::sync::atomic::AtomicBool>,
    gen_stop: Arc<std::sync::atomic::AtomicBool>,
) {
    if let Err(e) = listener.set_nonblocking(true) {
        eprintln!("findexd: accept loop cannot set nonblocking: {e}; exiting (supervisor will respawn)");
        return;
    }
    let stopped = || {
        daemon_stop.load(std::sync::atomic::Ordering::Relaxed)
            || gen_stop.load(std::sync::atomic::Ordering::Relaxed)
    };
    while !stopped() {
        match listener.accept() {
            Ok((stream, _)) => {
                // The listener is nonblocking for the stop flag; accepted
                // streams must be blocking for `serve_client`'s read loop.
                // (On Windows they inherit the nonblocking mode, which turns
                // every first idle read into WouldBlock → close → RST.)
                if stream.set_nonblocking(false).is_err() {
                    // A persistent failure here drops every connection while
                    // the loop lives — deaf while alive. Log loudly but at
                    // most every ~10 s: per-failure logging on a hot loop
                    // is a disk DoS when the fd table is wedged; silence
                    // loses the diagnosis entirely.
                    static LAST_LOG_MS: std::sync::atomic::AtomicU64 =
                        std::sync::atomic::AtomicU64::new(0);
                    let now = findex::applier::now_ms();
                    if now.saturating_sub(
                        LAST_LOG_MS.load(std::sync::atomic::Ordering::Relaxed),
                    ) >= 10_000
                    {
                        LAST_LOG_MS
                            .store(now, std::sync::atomic::Ordering::Relaxed);
                        eprintln!("findexd: accepted stream would not go blocking; dropping connections (further occurrences suppressed 10 s)");
                    }
                    continue;
                }
                let ctx = ctx.clone();
                std::thread::spawn(move || serve_client(ctx, stream));
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => {
                // A recurring error here (fd exhaustion, revoked socket) is
                // otherwise invisible: the loop survives but serves nothing
                // between sleeps. Log loudly so the supervisor's restart
                // counter has a matching diagnosis in the log.
                eprintln!("findexd: accept error (retrying in 50 ms): {e}");
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}

/// As [`accept_loop`], for a **system** instance: each connection is served as
/// the user the kernel says is on the other end.
#[cfg(unix)]
pub fn accept_loop_system(
    listener: Listener,
    make_ctx: Arc<dyn Fn(Option<findex::ownership::Caller>) -> Arc<ServerCtx> + Send + Sync>,
    daemon_stop: Arc<std::sync::atomic::AtomicBool>,
    gen_stop: Arc<std::sync::atomic::AtomicBool>,
) {
    if let Err(e) = listener.set_nonblocking(true) {
        eprintln!("findexd: accept loop (system) cannot set nonblocking: {e}; exiting (supervisor will respawn)");
        return;
    }
    let stopped = || {
        daemon_stop.load(std::sync::atomic::Ordering::Relaxed)
            || gen_stop.load(std::sync::atomic::Ordering::Relaxed)
    };
    while !stopped() {
        match listener.accept() {
            Ok((stream, _)) => {
                let make = make_ctx.clone();
                std::thread::spawn(move || serve_client_system(|c| make(c), stream));
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => {
                // As in `accept_loop`: a surviving-but-erroring loop is
                // otherwise silent. Log so the restart counter has a
                // matching diagnosis.
                eprintln!("findexd: accept error (system, retrying in 50 ms): {e}");
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}

#[cfg(test)]
mod supervisor_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn panic_payload_renders_all_shapes() {
        // The three payload shapes a panicked accept thread can leave
        // behind: plain `panic!("...")`, formatted, and opaque.
        let s: Box<dyn std::any::Any + Send> = Box::new("plain &str");
        assert_eq!(panic_payload(&s), "plain &str");
        let s: Box<dyn std::any::Any + Send> = Box::new("owned String".to_string());
        assert_eq!(panic_payload(&s), "owned String");
        let s: Box<dyn std::any::Any + Send> = Box::new(42u32);
        assert_eq!(panic_payload(&s), "<non-string panic payload>");
    }

    fn test_ctx() -> Arc<ServerCtx> {
        let mut ix = findex::index::Index::new();
        ix.insert(
            findex::index::ROOT_ID,
            b"socktest",
            findex::index::EntryFlags::EMPTY
                .with_dir(true)
                .with_root_dir(true),
        );
        let shared = Shared::new(
            ix,
            vec![findex::roots::Root::new(0, PathBuf::from("/tmp/socktest-root"))],
        );
        Arc::new(ServerCtx {
            shared,
            sync: findex::sync::SyncRegistry::new(),
            backend: "fake".into(),
            control: None,
            caller: None,
        })
    }

    /// The supervisor repairs a dead accept loop: kill the adopted
    /// generation, tick, and the daemon serves again with
    /// `accept_restarts == 1`. This is the deaf-daemon signature (§15)
    /// exercised live: pidfile-equivalent state held, index ticking, but no
    /// listener — until the supervisor re-binds.
    #[cfg(unix)]
    #[test]
    fn supervisor_respawns_a_dead_accept_loop() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("findex.sock");
        let listener = bind(&sock).expect("initial bind");
        let stop = Arc::new(AtomicBool::new(false));
        let ctx = test_ctx();
        let mut sup =
            AcceptSupervisor::personal(sock.clone(), listener, ctx.clone(), stop.clone());
        assert!(sup.is_alive(), "adopted accept thread must be live");
        assert_eq!(
            ctx.shared.health.read().unwrap().accept_restarts,
            0,
            "adopting is not a restart"
        );

        // Baseline: the adopted generation serves.
        let pong = roundtrip(&sock, &Request::Hello).expect("baseline hello");
        assert!(
            matches!(pong.first(), Some(Response::Pong { .. })),
            "got {pong:?}"
        );

        // Simulate the accept thread dying (dropped listener /
        // set_nonblocking failure in production).
        sup.kill_current_for_test();

        // Poll: the old generation needs ~one poll interval to notice its
        // flag and exit, then the next tick joins it and re-binds.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut respawned = false;
        while Instant::now() < deadline {
            if sup.tick() {
                respawned = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(respawned, "supervisor must respawn the dead accept loop");
        assert!(sup.is_alive());
        assert_eq!(
            ctx.shared.health.read().unwrap().accept_restarts,
            1,
            "exactly one repair must be counted"
        );

        // The respawned generation serves again on the same socket path.
        let pong2 = roundtrip(&sock, &Request::Hello).expect("post-respawn hello");
        assert!(
            matches!(pong2.first(), Some(Response::Pong { .. })),
            "got {pong2:?}"
        );

        // Shutdown never respawns.
        stop.store(true, Ordering::Relaxed);
        assert!(!sup.tick());
    }
}
