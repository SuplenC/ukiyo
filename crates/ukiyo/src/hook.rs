//! `ukiyo shell-hook <shell>`: snippets that report every finished command
//! (command line, exit status, duration) to the daemon. The daemon decides
//! whether it was a build (`signals.build.*`).

/// zsh: no process is spawned per prompt. `zsh/net/socket` writes the JSON
/// line straight to the daemon's socket; nothing happens if it isn't there.
pub const ZSH: &str = r#"# ukiyo shell hook (zsh). Add to ~/.zshrc:  eval "$(ukiyo shell-hook zsh)"
if zmodload zsh/net/socket 2>/dev/null && zmodload zsh/datetime 2>/dev/null; then
  typeset -g _ukiyo_cmd= _ukiyo_start=

  # JSON-escape $1 into $REPLY.
  _ukiyo_json() {
    local s=$1
    s=${s//\\/\\\\}
    s=${s//\"/\\\"}
    s=${s//$'\n'/\\n}
    s=${s//$'\t'/\\t}
    s=${s//$'\r'/\\r}
    s=${s//[[:cntrl:]]/ }
    REPLY=$s
  }

  _ukiyo_preexec() {
    _ukiyo_cmd=$1
    _ukiyo_start=$EPOCHREALTIME
  }

  _ukiyo_precmd() {
    local st=$?
    [[ -n $_ukiyo_start ]] || return 0
    local dur
    printf -v dur '%.3f' $(( EPOCHREALTIME - _ukiyo_start ))
    _ukiyo_start=
    local sock=${UKIYO_SOCKET:-${XDG_RUNTIME_DIR}/ukiyo/${${WAYLAND_DISPLAY:-wayland-0}:t}.sock}
    [[ -S $sock ]] || return 0
    zsocket $sock 2>/dev/null || return 0
    local fd=$REPLY
    _ukiyo_json $_ukiyo_cmd
    print -r -u $fd -- "{\"cmd\":\"command\",\"command\":\"$REPLY\",\"status\":$st,\"duration\":$dur}" 2>/dev/null
    exec {fd}>&-
    return 0
  }

  autoload -Uz add-zsh-hook
  add-zsh-hook preexec _ukiyo_preexec
  # First, so $? is still the command's status.
  precmd_functions=(_ukiyo_precmd ${precmd_functions:#_ukiyo_precmd})
fi
"#;

/// bash: spawns `ukiyo command` in the background after each command
/// (bash has no socket builtin), only when the daemon's socket exists.
pub const BASH: &str = r#"# ukiyo shell hook (bash). Add to ~/.bashrc:  eval "$(ukiyo shell-hook bash)"
_ukiyo_start=
_ukiyo_preexec() {
  [[ -n $COMP_LINE || -n $_ukiyo_start ]] && return
  [[ $BASH_COMMAND == _ukiyo_precmd* ]] && return
  _ukiyo_start=${EPOCHREALTIME:-$SECONDS}
}
_ukiyo_precmd() {
  local st=$?
  [[ -n $_ukiyo_start ]] || return 0
  local now=${EPOCHREALTIME:-$SECONDS} start=$_ukiyo_start
  _ukiyo_start=
  local sock=${UKIYO_SOCKET:-${XDG_RUNTIME_DIR}/ukiyo/${WAYLAND_DISPLAY##*/}.sock}
  [[ -S $sock ]] || return 0
  local cmd
  cmd=$(HISTTIMEFORMAT= builtin history 1)
  cmd=${cmd#*[0-9]  }
  local dur
  dur=$(awk "BEGIN{printf \"%.3f\", ${now/,/.} - ${start/,/.}}")
  (ukiyo command --status "$st" --duration "$dur" -- "$cmd" >/dev/null 2>&1 &)
  return 0
}
trap '_ukiyo_preexec' DEBUG
PROMPT_COMMAND="_ukiyo_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
"#;

pub fn snippet(shell: &str) -> Option<&'static str> {
    match shell {
        "zsh" => Some(ZSH),
        "bash" => Some(BASH),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::process::Command;

    fn have_zsh() -> bool {
        Command::new("zsh")
            .args(["-fc", "zmodload zsh/net/socket"])
            .status()
            .is_ok_and(|s| s.success())
    }

    /// The zsh escaping turns any command line into a JSON string that
    /// decodes back to it (control characters other than \n \t \r become
    /// spaces).
    #[test]
    fn zsh_json_escaping() {
        if !have_zsh() {
            eprintln!("no zsh with zsh/net/socket; skipped");
            return;
        }
        let cases = [
            "cargo build",
            r#"echo "quoted" and \backslash\ and \"both\""#,
            "printf 'a\\tb'\nsecond line\twith tab",
            "ünïcödé — 浮世 'single'",
            "bell\u{7}here",
        ];
        for c in cases {
            let script = format!("{ZSH}\n_ukiyo_json \"$1\"; print -rn -- \"\\\"$REPLY\\\"\"");
            let out = Command::new("zsh")
                .args(["-fc", &script, "zsh", c])
                .output()
                .unwrap();
            let text = String::from_utf8(out.stdout).unwrap();
            let back: String =
                serde_json::from_str(&text).unwrap_or_else(|e| panic!("{c:?} -> {text:?}: {e}"));
            assert_eq!(back, c.replace('\u{7}', " "), "{c:?}");
        }
    }

    /// The whole hook: preexec + precmd write one request line to the
    /// socket, which the daemon's decoder accepts.
    #[test]
    fn zsh_hook_writes_a_request() {
        if !have_zsh() {
            eprintln!("no zsh with zsh/net/socket; skipped");
            return;
        }
        let dir = std::env::temp_dir().join(format!("ukiyo-hook-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("s.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let script = format!(
            "{ZSH}\n_ukiyo_preexec 'make -j8 \"all\"'\nfalse\n_ukiyo_precmd\n_ukiyo_precmd\n"
        );
        let status = Command::new("zsh")
            .env("UKIYO_SOCKET", &sock)
            .args(["-fc", &script])
            .status()
            .unwrap();
        assert!(status.success());
        let (mut conn, _) = listener.accept().unwrap();
        let mut line = String::new();
        conn.read_to_string(&mut line).unwrap();
        let req = ukiyo_core::ipc::decode_request(&line).unwrap();
        match req {
            ukiyo_core::ipc::Request::Command {
                command,
                status,
                duration,
            } => {
                assert_eq!(command, "make -j8 \"all\"");
                assert_eq!(status, 1);
                assert!((0.0..5.0).contains(&duration));
            }
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn zsh_hook_is_silent_without_a_daemon() {
        if !have_zsh() {
            return;
        }
        let script = format!("{ZSH}\n_ukiyo_preexec 'ls'\n_ukiyo_precmd\n");
        let out = Command::new("zsh")
            .env("UKIYO_SOCKET", "/nonexistent/ukiyo.sock")
            .args(["-fc", &script])
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(out.stdout.is_empty() && out.stderr.is_empty());
    }

    #[test]
    fn known_shells() {
        assert!(snippet("zsh").is_some() && snippet("bash").is_some());
        assert!(snippet("fish").is_none());
    }
}
