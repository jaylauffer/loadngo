# NetBSD Display Bring-Up

Date: 2026-04-30

## Target

Host: `root@10.10.10.3`

Observed baseline:

- NetBSD `11.0_RC3` on `evbarm` / `aarch64`
- Rust and Cargo are installed from pkgsrc
- base Xorg exists under `/usr/X11R7/bin/Xorg`
- wscons display devices exist as `/dev/ttyE*`
- DRM devices exist under `/dev/dri`
- outbound routing/DNS was not available during bring-up, so the target build
  used vendored crate sources and Cargo `--offline`

## Direction

Use the `loadngo-host-desktop` boundary for NetBSD display work, but do not try
to build the full compositor first.

Use `loadngo-proactor` as the runtime core. NetBSD should use the existing
kqueue-backed `KqueuePort` for timers, wakeups, input readiness, and eventual
frame invalidation. Avoid fixed sleep loops in the host path.

Bring-up order:

1. Prove direct framebuffer output through `wsdisplay`.
2. Keep the proof running through `loadngo-proactor` timers instead of direct
   sleeps.
3. Route a tiny loadngo paint scene into that framebuffer path.
4. Add keyboard/mouse input through `wskbd` / `wsmouse` or `wsmux` readiness
   events registered with the proactor.
5. Decide whether the long-term compositor path should sit on:
   - wsdisplay framebuffer
   - DRM/KMS
   - X11/winit
   - Wayland server libraries

The first milestone intentionally uses `wsdisplay` because it is part of the
NetBSD base system and avoids making X11 or Wayland server setup the blocker.

## Probe

The initial probe binary is:

```bash
cargo run --offline -p loadngo-host-desktop --bin netbsd_wsdisplay_probe -- --probe-only
```

To paint a temporary test pattern:

```bash
cargo run --offline -p loadngo-host-desktop --bin netbsd_wsdisplay_probe -- --seconds 4
```

The probe opens `/dev/ttyE0`, switches to mapped framebuffer mode, writes a
deterministic RGB/checker test pattern, uses a `loadngo-proactor` timer to hold
the frame briefly, and restores emulation text mode before exit.

Do not call `msync(MS_SYNC)` on the wsdisplay framebuffer mapping. On the
NetBSD 11 RC test host it blocked the paint proof and made the machine stop
answering SSH until the process was terminated. Direct framebuffer stores plus
the proactor-held display interval were sufficient for the initial test.

Validated on `10.10.10.3`:

```bash
cargo check --offline -p loadngo-host-desktop --bin netbsd_wsdisplay_probe
cargo test --offline -p loadngo-host-desktop netbsd_ioctl_numbers_match_headers -- --nocapture
cargo run --offline -p loadngo-host-desktop --bin netbsd_wsdisplay_probe -- --probe-only
cargo run --offline -p loadngo-host-desktop --bin netbsd_wsdisplay_probe -- --seconds 2
```

The probe reported `/dev/ttyE0` as `1920x1080`, stride `7680`, `32` bpp.
The two-second paint test completed and restored `/dev/ttyE0` to emulation mode.

## Desktop Shell

The first functional desktop binary is:

```bash
cargo build --offline -p loadngo-host-desktop --bin netbsd_wsdesktop
./target/debug/netbsd_wsdesktop
```

For bounded testing over SSH or during bring-up:

```bash
./target/debug/netbsd_wsdesktop --seconds 30
```

Repo-local manual page:

```bash
mandoc -Tutf8 docs/man/loadngo-desktop.1
```

The shell currently provides:

- direct wsdisplay framebuffer presentation
- a native-format RAM back buffer; desktop primitives render off-screen before
  one completed-frame copy to wsdisplay
- a cursor overlay damage path; pointer motion restores the previous cursor
  rectangle from the back buffer and redraws only the cursor region at up to
  60 Hz
- proactor-driven damage presentation through `loadngo-proactor` / `KqueuePort`
- wscons mouse and keyboard readiness through the same proactor
- a first interactive Terminal app backed by a persistent `/bin/sh` process;
  command lines are edited in the desktop and submitted to the shell on Enter,
  while shell stdout/stderr are read through proactor readiness
- USB `wskbd` key-down fallback for the Terminal line editor, so targets that
  do not emit `WSCONS_EVENT_ASCII` still accept printable keys, Shift
  punctuation, Backspace, Tab, and Enter
- a launcher, draggable app window, status bar, pointer cursor, and quit action
- keyboard fallbacks outside the Terminal app: `Tab` / `Enter` cycle apps,
  `Q` / `Esc` quits

Default devices:

```text
display:  /dev/ttyE0
mouse:    /dev/wsmouse
keyboard: /dev/wskbd
```

Useful options:

```bash
./target/debug/netbsd_wsdesktop --device /dev/ttyE0 --mouse /dev/wsmouse --keyboard /dev/wskbd
./target/debug/netbsd_wsdesktop --fps 1
./target/debug/netbsd_wsdesktop --cursor-hz 60
./target/debug/netbsd_wsdesktop --no-input --seconds 10
./target/debug/netbsd_wsdesktop --continuous --fps 1 --seconds 10
```

PQ-authenticated deployment discipline:

- create a challenge payload that names `root@10.10.10.3`, the touched files,
  the build/test command, and the intended restart action
- issue a `loadngo-pq-auth` token with audience `netbsd-rpi3b` and scope
  `netbsd-deploy`
- verify that token against the same challenge and trusted public key before
  changing the live target
- use `--quiet` for auth issue/verify and keep build output in target-local log
  files unless failure detail is needed

This keeps routine bring-up transcripts to concise signed receipts instead of
scrollback-heavy command noise.

Continuity contract:

- Default mode renders once, then presents only after input or shell state
  changes.
- `--fps` is a maximum present rate, not an animation target.
- `--cursor-hz` is a separate cursor damage cap. Keep it at 60 for smooth
  mouse tracking unless the target display or CPU cannot keep up.
- `--continuous` is a diagnostic mode for repaint testing only.
- Avoid animated status counters as default behavior on wsdisplay until damage
  tracking is in place.
- The Terminal app is intentionally line-oriented for this milestone. It is
  useful for shell commands and persistent shell state such as `cd`, but it is
  not yet a full tty/pty emulator for curses programs or job-control workflows.

The wsdisplay mmap can behave like slow device memory. The desktop must not
redraw at raw mouse event rate, and it must not repaint merely because time has
passed. The back buffer prevents users from seeing individual background,
window, and cursor drawing passes; the final copy may still tear until damage
rectangles or hardware page flipping exist.
Cursor motion is the exception: it does not copy the whole framebuffer. The
cursor path repairs the previous cursor rectangle from the back buffer and
paints the new cursor directly on the mapped front buffer, throttled by the
proactor timer.

## Architecture Notes

The NetBSD code should stay behind `host-desktop` platform boundaries:

- `ui-core` remains platform independent
- `host-core` remains the frame/input/render contract
- NetBSD-specific `wsdisplay`/`wskbd`/`wsmouse` APIs stay in the host backend
- host timing, wakeup, and readiness policy should flow through
  `loadngo-proactor`
- future renderer work should consume `loadngo-renderer` commands rather than
  binding application code directly to wscons
