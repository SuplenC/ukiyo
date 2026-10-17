//! The control socket: line-delimited JSON, one response per request,
//! served non-blocking from the event loop.

use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{Interest, LoopHandle, Mode, PostAction};
use ukiyo_core::ipc::{self, MAX_LINE, Response};

use super::State;

/// Create the socket: its directory 0700, the socket 0600. A stale socket
/// is replaced; a live one means another daemon runs here.
pub fn bind(path: &Path) -> Result<UnixListener> {
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .with_context(|| format!("creating {}", dir.display()))?;
        // Ours already? Keep it private.
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            bail!(
                "another ukiyo daemon is already running on {}",
                path.display()
            );
        }
        std::fs::remove_file(path).with_context(|| format!("removing stale {}", path.display()))?;
    }
    let l = UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    l.set_nonblocking(true)?;
    Ok(l)
}

pub fn insert(handle: &LoopHandle<'static, State>, listener: UnixListener) -> Result<()> {
    let h = handle.clone();
    handle
        .insert_source(
            Generic::new(listener, Interest::READ, Mode::Level),
            move |_, l, _state| {
                loop {
                    match l.accept() {
                        Ok((stream, _)) => {
                            if stream.set_nonblocking(true).is_ok() {
                                add_client(&h, stream);
                            }
                        }
                        Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                        Err(e) => {
                            log::warn!("ipc accept: {e}");
                            break;
                        }
                    }
                }
                Ok(PostAction::Continue)
            },
        )
        .map_err(|e| anyhow::anyhow!("ipc: {e}"))?;
    Ok(())
}

fn add_client(handle: &LoopHandle<'static, State>, stream: UnixStream) {
    let mut buf: Vec<u8> = Vec::new();
    let r = handle.insert_source(
        Generic::new(stream, Interest::READ, Mode::Level),
        move |_, s, state| {
            let mut s: &UnixStream = s;
            let mut chunk = [0u8; 4096];
            let eof = loop {
                match s.read(&mut chunk) {
                    Ok(0) => break true,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break false,
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(_) => break true,
                }
            };
            while let Some(i) = buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = buf.drain(..=i).collect();
                let text = String::from_utf8_lossy(&line);
                if text.trim().is_empty() {
                    continue;
                }
                let resp = match ipc::decode_request(&text) {
                    Ok(req) => {
                        log::debug!("ipc: {req:?}");
                        state.handle(req)
                    }
                    Err(e) => Response::err(e),
                };
                reply(s, &resp);
            }
            if buf.len() > MAX_LINE {
                reply(
                    s,
                    &Response::err(format!("request longer than {MAX_LINE} bytes")),
                );
                return Ok(PostAction::Remove);
            }
            // A last line without its newline still counts.
            if eof && !buf.is_empty() {
                let text = String::from_utf8_lossy(&buf).into_owned();
                buf.clear();
                if let Ok(req) = ipc::decode_request(&text) {
                    let resp = state.handle(req);
                    reply(s, &resp);
                }
            }
            state.schedule();
            Ok(if eof {
                PostAction::Remove
            } else {
                PostAction::Continue
            })
        },
    );
    if let Err(e) = r {
        log::warn!("ipc client: {e}");
    }
}

fn reply(s: &UnixStream, resp: &Response) {
    // Responses are small; write them out with a short blocking timeout so
    // a client that never reads can't wedge the daemon.
    let mut w = s;
    let _ = s.set_nonblocking(false);
    let _ = s.set_write_timeout(Some(Duration::from_secs(1)));
    // A client that already hung up (the shell hook) is fine.
    let _ = w.write_all(ipc::encode(resp).as_bytes());
    let _ = s.set_nonblocking(true);
}
