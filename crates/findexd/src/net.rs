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
use std::sync::Arc;

use findex::proto::{Request, Response};
use findex::server::{handle_request, ServerCtx};
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

/// Serve one client connection of a **system** instance, answering every query as
/// the connecting user.
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
    let reader = BufReader::new(stream.try_clone().expect("clone stream"));
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
        let responses = handle_request(&ctx, &req);
        let mut closed = false;
        for r in &responses {
            if writeln!(writer, "{}", r.to_line()).is_err() {
                closed = true;
                break;
            }
        }
        let _ = writer.flush();
        if closed {
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

/// Serve one client connection until it closes.
pub fn serve_client(ctx: Arc<ServerCtx>, stream: Stream) {
    let reader = BufReader::new(stream.try_clone().expect("clone stream"));
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
        let responses = handle_request(&ctx, &req);
        let mut closed = false;
        for r in &responses {
            if writeln!(writer, "{}", r.to_line()).is_err() {
                closed = true;
                break;
            }
        }
        let _ = writer.flush();
        if closed {
            break;
        }
    }
}

/// One request/response exchange over a fresh connection. Used by the CLI and
/// the tests.
pub fn roundtrip(socket: &Path, req: &Request) -> std::io::Result<Vec<Response>> {
    let stream = findex::transport::connect(socket)?;
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

/// Run the accept loop, spawning a thread per client, until `stop` is set.
pub fn accept_loop(
    listener: Listener,
    ctx: Arc<ServerCtx>,
    stop: Arc<std::sync::atomic::AtomicBool>,
) {
    listener.set_nonblocking(true).expect("set_nonblocking");
    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                // The listener is nonblocking for the stop flag; accepted
                // streams must be blocking for `serve_client`'s read loop.
                // (On Windows they inherit the nonblocking mode, which turns
                // every first idle read into WouldBlock → close → RST.)
                if stream.set_nonblocking(false).is_err() {
                    continue;
                }
                let ctx = ctx.clone();
                std::thread::spawn(move || serve_client(ctx, stream));
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(_) => {
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
    stop: Arc<std::sync::atomic::AtomicBool>,
) {
    listener.set_nonblocking(true).expect("set_nonblocking");
    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let make = make_ctx.clone();
                std::thread::spawn(move || serve_client_system(|c| make(c), stream));
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(_) => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}
