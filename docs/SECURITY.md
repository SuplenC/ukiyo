# Security and privacy

ukiyo reacts to what you do: typing, music, the cursor, builds. Each of
those is sensitive, so each input is reduced to the least the wallpaper
needs, as early as possible. This page says what is read, by whom, and what
an attacker would get.

## The daemon (`ukiyo daemon`)

Runs as you, unprivileged. It needs no group membership and no
capabilities.

- **Control socket**: `$XDG_RUNTIME_DIR/ukiyo/<display>.sock`. The directory
  is created `0700` and the socket `0600`, both inside your private runtime
  directory, so only your user can talk to it. Anyone who can already run
  code as you can also drive the wallpaper; nothing more is exposed.
- **Typing, default source `idle`**: from the compositor's
  ext-idle-notify-v1 protocol with a ~0.4 s timeout. The daemon learns
  only "there was input" and "there was no input for 0.4 s". No key codes,
  no key events, no `input` group. Input while the cursor stands still is
  counted as typing. That is an approximation (it can't tell 3 from 8 keys
  a second), and the trade is deliberate: an unprivileged default that
  can't be turned into a keylogger.
- **Audio**: only the monitor of a playback sink (what you hear), via
  `pw-record` with `stream.capture.sink=true` or `parec -d
  @DEFAULT_MONITOR@`. `signals.audio.device` names a sink, and its monitor is
  used. A microphone is never opened. Capture runs only while the
  wallpaper is drawing; paused, still or locked, the capture process is
  killed. Samples are reduced to five numbers (level, three bands, beat) in
  memory and never stored.
- **Cursor**: Hyprland's `cursorpos`, queried at most once per drawn frame,
  or `wl_pointer` events over the wallpaper itself.
- **Shell hook**: sends the command line, exit status and duration of each
  command to your own socket. The daemon keeps nothing; it decides whether
  the command was a build and fires a pulse. Commands are not logged
  (except at `RUST_LOG=debug`).
- **Lock frames**: `still-<output>.png` in your runtime directory (`0700`),
  showing only the wallpaper.

## The helper (`ukiyo-activityd`), optional

For an exact typing rate, something has to read keyboards, which needs the
`input` group. Putting your user in `input` would let **every** program you
run read every keystroke system-wide, including passwords typed into other
sessions and the lock screen. ukiyo never asks for that. Instead a small
separate helper holds the privilege, and it is built so that what it
publishes can't be used to reconstruct text.

What it does:

- Opens keyboards (devices with letter keys) read-only. It never grabs
  them and never writes to them.
- For each event it checks "key press?" (`EV_KEY`, value 1) and adds one to
  a counter. The key code is never read into a variable, stored or sent.
  Repeats and releases are ignored.
- Every 250 ms, on a fixed clock, it closes a bucket. It publishes
  presses per second over the last 2 s (8 buckets), rounded to 0.5, as
  `{"rate":3.5}`.

Why aggregate, quantised and coarse: the timing between keystrokes is a
known side channel. Inter-key intervals are enough to guess typed words and
narrow down passwords. A per-keystroke event, or a rate updated on every
key, would leak those intervals. A 2 s sliding count, rounded and sampled
on a fixed 250 ms clock regardless of input, says roughly "how busy the
keyboard was lately", which is all a wallpaper needs. Message timing says
nothing either: a line goes out every tick whether or not anything changed.

Who can connect: the socket is world-connectable (`0666`). On each
connection the helper reads the peer's uid with `SO_PEERCRED` and keeps it
only for root or the user of the active session on seat0 (`ACTIVE_UID=`
in `/run/systemd/seats/seat0`). While that client's session is not the
active one (you switched users, or a greeter is up), it gets `0`. Another
user on the machine can't watch your typing rate.

How it runs (`dist/systemd/ukiyo-activityd.{socket,service}`):
socket-activated, `DynamicUser=yes` (a throwaway uid) with
`SupplementaryGroups=input` as the only extra privilege. Also
`DevicePolicy=closed` + `DeviceAllow=char-input r`, no capabilities,
`NoNewPrivileges`, `ProtectSystem=strict`, `ProtectHome`, `PrivateNetwork`,
`RestrictAddressFamilies=AF_UNIX`, `SystemCallFilter=@system-service`,
`MemoryDenyWriteExecute`, and a 16 MiB memory cap.

If the helper is compromised, the attacker holds `input` group access as
a throwaway user in a sandbox with no network, no home directory, a
read-only system and no way to gain privileges. That is the same power
as being in the `input` group, contained to one tiny process. The helper
is ~300 lines with no dependencies beyond libc, so that surface is small
and easy to audit. If you don't want even that, don't install it: the
`idle` source needs no privileges at all, and `signals.typing.source =
"idle"` never tries the helper.

## Reporting

Please report vulnerabilities privately through GitHub security advisories
on the repository rather than in public issues.
