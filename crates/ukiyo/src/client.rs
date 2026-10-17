//! Talking to a running daemon.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use ukiyo_core::ipc::{self, Request, Response};

#[derive(Debug)]
pub enum ClientError {
    /// No daemon on the socket (or no socket path at all).
    NotRunning(String),
    Other(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::NotRunning(m) | ClientError::Other(m) => f.write_str(m),
        }
    }
}

pub fn socket() -> Result<PathBuf, ClientError> {
    ipc::socket_path().ok_or_else(|| ClientError::NotRunning("XDG_RUNTIME_DIR isn't set".into()))
}

/// Send one request and wait for its response.
pub fn send(req: &Request) -> Result<Response, ClientError> {
    let path = socket()?;
    let mut s = UnixStream::connect(&path).map_err(|e| {
        use std::io::ErrorKind::*;
        match e.kind() {
            NotFound | ConnectionRefused => ClientError::NotRunning(format!(
                "the ukiyo daemon isn't running (no {})",
                path.display()
            )),
            _ => ClientError::Other(format!("{}: {e}", path.display())),
        }
    })?;
    // Scene switches load textures; give them time.
    let timeout = match req {
        Request::Set { .. } | Request::Snapshot { .. } => 120,
        _ => 15,
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(timeout)));
    let _ = s.set_write_timeout(Some(Duration::from_secs(5)));
    s.write_all(ipc::encode(req).as_bytes())
        .map_err(|e| ClientError::Other(format!("sending to the daemon: {e}")))?;
    let mut line = String::new();
    BufReader::new(&s)
        .read_line(&mut line)
        .map_err(|e| ClientError::Other(format!("reading the daemon's answer: {e}")))?;
    if line.is_empty() {
        return Err(ClientError::Other(
            "the daemon closed the connection".into(),
        ));
    }
    ipc::decode_response(&line).map_err(ClientError::Other)
}

/// Is a daemon answering on our socket?
pub fn running() -> bool {
    socket().is_ok_and(|p| UnixStream::connect(p).is_ok())
}
