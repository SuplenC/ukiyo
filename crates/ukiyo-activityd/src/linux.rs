//! The Linux side: evdev keyboards, inotify for hotplug, the socket, one
//! `poll` loop.

use std::ffi::CString;
use std::io::{ErrorKind, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::{Duration, Instant};

use crate::Args;
use crate::logic::{self, Window};

const INPUT_DIR: &str = "/dev/input";
/// `KEY_MAX / 8 + 1`.
const KEY_BYTES: usize = 0x2ff / 8 + 1;

fn eviocgbit(ev: u32, len: usize) -> libc::c_ulong {
    // _IOC(_IOC_READ, 'E', 0x20 + ev, len)
    ((2 as libc::c_ulong) << 30)
        | ((len as libc::c_ulong) << 16)
        | ((b'E' as libc::c_ulong) << 8)
        | (0x20 + ev) as libc::c_ulong
}

struct Device {
    path: String,
    fd: OwnedFd,
}

/// Open `path` if it's a keyboard.
fn open_keyboard(path: &str) -> Option<Device> {
    let c = CString::new(path).ok()?;
    // SAFETY: plain open(2) of a NUL-terminated path.
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return None;
    }
    // SAFETY: we own the fresh descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut bits = [0u8; KEY_BYTES];
    // SAFETY: EVIOCGBIT writes at most `len` bytes into `bits`.
    let r = unsafe {
        libc::ioctl(
            fd.as_raw_fd(),
            eviocgbit(u32::from(logic::EV_KEY), KEY_BYTES) as _,
            bits.as_mut_ptr(),
        )
    };
    (r >= 0 && logic::is_keyboard(&bits)).then(|| Device {
        path: path.to_string(),
        fd,
    })
}

fn scan(devices: &mut Vec<Device>) {
    let Ok(rd) = std::fs::read_dir(INPUT_DIR) else {
        return;
    };
    let mut paths: Vec<String> = rd
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("event"))
        .map(|e| e.path().to_string_lossy().into_owned())
        .collect();
    paths.sort();
    for p in paths {
        if devices.iter().any(|d| d.path == p) {
            continue;
        }
        if let Some(d) = open_keyboard(&p) {
            eprintln!("ukiyo-activityd: keyboard {p}");
            devices.push(d);
        }
    }
}

/// Read everything pending; the number of presses, or None if the device
/// is gone.
fn drain(d: &Device) -> Option<u32> {
    let mut buf = [0u8; logic::EVENT_SIZE * 64];
    let mut presses = 0;
    loop {
        // SAFETY: reading into a local buffer of the given size.
        let n = unsafe { libc::read(d.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            presses += logic::count_presses(&buf[..n as usize]);
            continue;
        }
        if n == 0 {
            return None;
        }
        let e = std::io::Error::last_os_error();
        return match e.kind() {
            ErrorKind::WouldBlock => Some(presses),
            ErrorKind::Interrupted => continue,
            _ => None,
        };
    }
}

fn inotify() -> Option<OwnedFd> {
    // SAFETY: plain syscalls; the fd is owned on success.
    unsafe {
        let fd = libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC);
        if fd < 0 {
            return None;
        }
        let dir = CString::new(INPUT_DIR).ok()?;
        // IN_ATTRIB: udev sets the group/ACL after the node appears.
        libc::inotify_add_watch(fd, dir.as_ptr(), libc::IN_CREATE | libc::IN_ATTRIB);
        Some(OwnedFd::from_raw_fd(fd))
    }
}

fn drain_fd(fd: RawFd) {
    let mut buf = [0u8; 4096];
    // SAFETY: reading into a local buffer.
    while unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
}

fn peer_uid(s: &UnixStream) -> Option<u32> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: SO_PEERCRED fills a ucred of the given length.
    let r = unsafe {
        libc::getsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    (r == 0).then_some(cred.uid)
}

/// The listening socket: from systemd (`LISTEN_FDS`) or bound here.
fn listener(path: &str) -> Result<UnixListener, String> {
    let ours = std::env::var("LISTEN_PID")
        .ok()
        .and_then(|p| p.parse::<u32>().ok())
        == Some(std::process::id());
    if ours && std::env::var("LISTEN_FDS").ok().as_deref() == Some("1") {
        // SAFETY: systemd passes the socket as fd 3.
        let l = unsafe { UnixListener::from_raw_fd(3) };
        l.set_nonblocking(true).map_err(|e| e.to_string())?;
        return Ok(l);
    }
    if let Some(dir) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    if std::path::Path::new(path).exists() {
        if UnixStream::connect(path).is_ok() {
            return Err(format!("{path}: another instance is running"));
        }
        let _ = std::fs::remove_file(path);
    }
    let l = UnixListener::bind(path).map_err(|e| format!("{path}: {e}"))?;
    // Anyone may connect; who gets an answer is checked per peer.
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666));
    l.set_nonblocking(true).map_err(|e| e.to_string())?;
    Ok(l)
}

struct Client {
    stream: UnixStream,
    uid: u32,
}

pub fn run(args: &Args) -> Result<(), String> {
    let listener = listener(&args.socket)?;
    let notify = inotify();
    let mut devices = Vec::new();
    scan(&mut devices);
    if devices.is_empty() {
        eprintln!("ukiyo-activityd: no readable keyboard yet (is it in the `input` group?)");
    }
    let mut clients: Vec<Client> = Vec::new();
    let mut window = Window::default();
    let mut presses: u32 = 0;
    let tick = Duration::from_millis(logic::TICK_MS);
    let mut next = Instant::now() + tick;
    let active_uid = || {
        std::fs::read_to_string(&args.seat_file)
            .ok()
            .and_then(|t| logic::parse_active_uid(&t))
    };

    loop {
        let mut fds: Vec<libc::pollfd> = Vec::with_capacity(devices.len() + 2);
        let pfd = |fd: RawFd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        fds.push(pfd(listener.as_raw_fd()));
        fds.push(pfd(notify.as_ref().map_or(-1, |n| n.as_raw_fd())));
        fds.extend(devices.iter().map(|d| pfd(d.fd.as_raw_fd())));
        let wait = next.saturating_duration_since(Instant::now());
        // SAFETY: `fds` is a valid array of pollfd for the call.
        let n = unsafe {
            libc::poll(
                fds.as_mut_ptr(),
                fds.len() as libc::nfds_t,
                wait.as_millis() as libc::c_int,
            )
        };
        if n < 0 && std::io::Error::last_os_error().kind() != ErrorKind::Interrupted {
            return Err(format!("poll: {}", std::io::Error::last_os_error()));
        }
        if n > 0 {
            // Devices first (indices shift when one goes).
            let mut gone = Vec::new();
            for (i, d) in devices.iter().enumerate() {
                if fds[i + 2].revents != 0 {
                    match drain(d) {
                        Some(p) => presses = presses.saturating_add(p),
                        None => gone.push(i),
                    }
                }
            }
            for i in gone.into_iter().rev() {
                eprintln!("ukiyo-activityd: {} gone", devices[i].path);
                devices.remove(i);
            }
            if fds[1].revents != 0
                && let Some(nfd) = &notify
            {
                drain_fd(nfd.as_raw_fd());
                scan(&mut devices);
            }
            if fds[0].revents != 0 {
                while let Ok((s, _)) = listener.accept() {
                    let uid = peer_uid(&s);
                    match uid {
                        Some(uid) if logic::allowed(uid, active_uid()) => {
                            let _ = s.set_nonblocking(true);
                            clients.push(Client { stream: s, uid });
                        }
                        _ => drop(s),
                    }
                }
            }
        }
        if Instant::now() >= next {
            next += tick;
            // Fell far behind (suspend): don't burst.
            if next < Instant::now() {
                next = Instant::now() + tick;
            }
            let rate = window.tick(std::mem::take(&mut presses));
            if !clients.is_empty() {
                let active = active_uid();
                clients.retain_mut(|c| {
                    let r = logic::visible_rate(c.uid, active, rate);
                    match c.stream.write_all(logic::line(r).as_bytes()) {
                        Ok(()) => true,
                        // A reader that's behind loses this line, not its connection.
                        Err(e) => e.kind() == ErrorKind::WouldBlock,
                    }
                });
            }
        }
    }
}
