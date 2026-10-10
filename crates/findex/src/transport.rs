//! IPC transport (§11): unix-domain sockets where they exist, TCP loopback on
//! Windows (whose std has no unix sockets).
//!
//! The socket *address* stays a `PathBuf` on every platform so config, CLI
//! flags, `FINDEX_SOCKET`, and tests need no platform branches: on unix it
//! is a filesystem path, on Windows it carries `127.0.0.1:PORT` text (see
//! [`crate::config_paths::socket_path`]). Loopback-only binding is the access
//! control where no filesystem permissions exist.

use std::io;
use std::path::Path;

#[cfg(unix)]
pub type Stream = std::os::unix::net::UnixStream;
#[cfg(windows)]
pub type Stream = std::net::TcpStream;

#[cfg(unix)]
pub type Listener = std::os::unix::net::UnixListener;
#[cfg(windows)]
pub type Listener = std::net::TcpListener;

/// Parse a Windows socket address (`127.0.0.1:PORT` text in a `PathBuf`).
#[cfg(windows)]
pub fn parse_addr(path: &Path) -> io::Result<std::net::SocketAddr> {
    path.to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket address is not UTF-8"))?
        .parse::<std::net::SocketAddr>()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("bad socket address: {e}")))
}

/// Connect to the daemon at `socket`.
pub fn connect(socket: &Path) -> io::Result<Stream> {
    #[cfg(unix)]
    {
        Stream::connect(socket)
    }
    #[cfg(windows)]
    {
        Stream::connect(parse_addr(socket)?)
    }
}
