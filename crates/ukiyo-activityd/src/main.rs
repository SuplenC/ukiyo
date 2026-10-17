//! `ukiyo-activityd`: a tiny privileged helper that tells ukiyo how fast
//! you type, and nothing else.
//!
//! It reads keyboards under `/dev/input` (it needs the `input` group; run
//! it from the shipped systemd unit), counts key presses, and publishes
//! once every 250 ms the presses per second over the last 2 s, rounded to
//! 0.5, as `{"rate":3.5}` lines on a Unix socket. Key codes are never
//! stored or sent, and the coarse fixed cadence hides keystroke timing.
//! Only root and the user of the active seat0 session may connect.
//! See `docs/SECURITY.md`.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

#[cfg(target_os = "linux")]
mod linux;
mod logic;

use std::process::ExitCode;

const USAGE: &str = "usage: ukiyo-activityd [--socket PATH] [--seat-file PATH]

Publishes an aggregate typing rate for ukiyo on a Unix socket.
  --socket PATH     where to listen (default /run/ukiyo-activity/socket;
                    ignored under systemd socket activation)
  --seat-file PATH  systemd's seat state (default /run/systemd/seats/seat0)";

pub struct Args {
    pub socket: String,
    pub seat_file: String,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut a = Args {
        socket: "/run/ukiyo-activity/socket".into(),
        seat_file: "/run/systemd/seats/seat0".into(),
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--socket" => a.socket = it.next().ok_or("--socket needs a path")?.clone(),
            "--seat-file" => a.seat_file = it.next().ok_or("--seat-file needs a path")?.clone(),
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok(a)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&args) {
        Ok(a) => a,
        Err(e) => {
            if e.is_empty() {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            eprintln!("ukiyo-activityd: {e}\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    run(args)
}

#[cfg(target_os = "linux")]
fn run(args: Args) -> ExitCode {
    match linux::run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ukiyo-activityd: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn run(_: Args) -> ExitCode {
    eprintln!("ukiyo-activityd: Linux only");
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let a = parse_args(&s(&[])).unwrap();
        assert_eq!(a.socket, "/run/ukiyo-activity/socket");
        let a = parse_args(&s(&["--socket", "/tmp/x", "--seat-file", "/tmp/s"])).unwrap();
        assert_eq!(
            (a.socket.as_str(), a.seat_file.as_str()),
            ("/tmp/x", "/tmp/s")
        );
        assert!(parse_args(&s(&["--socket"])).is_err());
        assert!(parse_args(&s(&["--bogus"])).is_err());
    }
}
