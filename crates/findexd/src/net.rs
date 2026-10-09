//! Unix-socket transport for the NDJSON protocol (§11, §15).
//!
//! * Socket at `$XDG_RUNTIME_DIR/findex.sock` (0600), never `/tmp`.
//! * Stale-socket recovery: if the path exists but nobody is listening, unlink
//!   it and bind afresh.
//! * The pidfile is `flock`ed so two daemons cannot race for the same socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use findex::proto::{Request, Response};
use findex::server::{handle_request, ServerCtx};

/// Bind the listener, recovering a stale socket if present.
pub fn bind(socket: &Path) -> std::io::Result<UnixListener> {
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if socket.exists() {
        // Is anyone home?
        match UnixStream::connect(socket) {
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
    let listener = UnixListener::bind(socket)?;
    // 0600: only the owner may talk to the daemon.
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Try to connect to an existing daemon.
pub fn connect(socket: &Path) -> std::io::Result<UnixStream> {
    UnixStream::connect(socket)
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
        let mut f = &file;
        f.set_len(0)?;
        use std::io::Seek;
        f.seek(std::io::SeekFrom::Start(0))?;
        write!(f, "{}", std::process::id())?;
        f.sync_all()?;
        Ok(PidFile { path: path.to_path_buf(), file })
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

fn process_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Serve one client connection until it closes.
pub fn serve_client(ctx: Arc<ServerCtx>, stream: UnixStream) {
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
                let resp = Response::Error { message: format!("bad request: {e}") };
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
    let stream = UnixStream::connect(socket)?;
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
            let done = matches!(r, Response::Done { .. } | Response::Error { .. } | Response::Pong { .. } | Response::Synced { .. } | Response::Stats { .. } | Response::Rescanning { .. });
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
    listener: UnixListener,
    ctx: Arc<ServerCtx>,
    stop: Arc<std::sync::atomic::AtomicBool>,
) {
    listener
        .set_nonblocking(true)
        .expect("set_nonblocking");
    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
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
