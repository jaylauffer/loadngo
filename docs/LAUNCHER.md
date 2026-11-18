# Launcher

`loadngo-launcher` is one window that lists what can be started on a machine
and starts it: loadngo's own tools, the games built on it, a page some
program serves, a link (Jay, 2026-11-16: "The loadngo framework needs it's
own launcher for all these things that can be installed as a mac app"). It is
one binary, `launcher`, on `loadngo-host-desktop`, the same source on macOS,
Linux and Windows.

```
cargo run -p loadngo-launcher --release
launcher --start "Code Editor"
scripts/install-launcher.sh --open        # from a checkout: build it and install it
scripts/install-launcher.sh --release     # install the newest published build
```

`launcher --help` lists every option.

## The catalog

What the window lists is a JSON file, `catalog.json`, in the app data
folder:

| System | Folder |
|---|---|
| macOS | `~/Library/Application Support/loadngo-launcher` |
| Linux | `~/.local/share/loadngo-launcher` |
| Windows | `%APPDATA%\loadngo-launcher` |

**Edit list** opens the file in the system's editor for it (and makes an
empty one first if there is none). **Reload** (Cmd-R, Ctrl-R) reads it
again. A file that cannot be read changes nothing that is listed or running:
the reason shows above the list, naming the entry.

```json
{
  "entries": [
    {
      "name": "Code Editor",
      "group": "loadngo",
      "about": "Opens the folder and tabs you had last time",
      "build": { "program": "cargo", "args": ["build", "--release", "-p", "loadngo-code-editor"],
                 "dir": "~/src/loadngo" },
      "run": { "program": "~/src/loadngo/target/release/code_editor" }
    },
    {
      "name": "Docs site",
      "group": "Pages",
      "run": { "program": "npm", "args": ["run", "dev", "--", "--port", "5173", "--strictPort"],
               "dir": "~/src/docs-site" },
      "open": "http://localhost:5173/",
      "ready": "Local:"
    },
    { "name": "Project board", "open": "https://example.org/board" }
  ]
}
```

| Field | | |
|---|---|---|
| `name` | required | Shown in the list; no two entries share one. |
| `group` | optional | The heading it is listed under. Entries keep the file's order. |
| `about` | optional | One line under the name. |
| `build` | optional | A command that must finish, successfully, before `run` starts. It runs on every start; a build that is up to date costs a moment. |
| `run` | optional | The program. |
| `open` | optional | A web address, file or folder, opened with the system's own opener. |
| `ready` | optional | Text in `run`'s output that means `open` can be shown. Without it the page opens as soon as the program has started. |

A command (`build`, `run`) is `program`, and optionally `args`, `dir` (the
folder it starts in) and `env` (variables added to its environment). `~` and
`~/…` stand for the home folder in `program`, in each argument, in `dir` and
in `open`. An entry needs `run`, `open` or both; an unknown field is refused,
so a misspelling does not pass silently.

The parts an entry has decide what starting it means:

| Parts | Kind | Starting it | When the launcher quits |
|---|---|---|---|
| `run` | A program with its own window | Builds, then starts it | It is left running |
| `run` and `open` | A program that serves a page | Builds, starts it, waits for `ready`, opens the page | It is stopped |
| `open` | A link | Opens it | — |

## The window

Each entry has a dot (grey: not running; amber: building, starting or
stopping; green: running; red: it failed), its name, and one line: what it
is, or what it is doing. While it builds, that line is the build's latest
output. The pane at the bottom says what the selected entry runs and shows
the last lines it printed; after a failure, the reason and the end of its
output.

- **Start** builds and starts. **Open** shows a link, or a running service's
  page again.
- **Stop** asks everything the entry started to exit, and ends it outright
  after three seconds. A program in its own window may hold unsaved work, so
  its Stop asks once more ("Stop it?"); a press anywhere else, or Escape,
  takes the question back.
- Up and Down move the selection; Enter starts or opens the selected entry.
  Enter never stops anything.
- `--start NAME` (more than once if wanted) starts entries as soon as the
  list has been read, as their buttons would.

## How it runs things

`launcher/src/process.rs`. Each start gets a thread of its own that blocks on
the program's output or on its exit; nothing polls. News goes to the window
as queued events and one wake of the host (`frame_waker`): a burst of output
costs one wake, and the window asks for no frame of its own
(`FrameDemand::Idle` always).

- **Its own process group**, so stopping an entry stops what it started too
  (`npm` and the `node` under it). Unix: `SIGTERM` to the group, once more a
  quarter of a second later, and `SIGKILL` after three seconds. The second
  request is for a process that a member of the group was in the middle of
  starting when the first arrived: the signal misses it, and it would hold
  the group's output open until the kill. Windows: `taskkill /T /F` on the
  process.
- **Programs are found as a terminal would find them.** An app started from
  the Dock or a desktop menu has only the system's bare `PATH`, without
  `cargo`, `npm` and the rest. On Unix the launcher asks the user's shell
  once (`$SHELL -l -i -c`, in a session of its own, eight seconds at most)
  for its `PATH` and gives that to what it starts. An entry's own `env.PATH`
  wins. If the shell does not answer, the launcher's own `PATH` is used. On
  Windows a program is named as it is on disk (`npm.cmd`).
- **A build step and a service** write down a pipe; their lines show as they
  come. Because a program writing into a pipe nobody reads fails, these are
  the launcher's to stop: when its window closes, on Quit, and (Unix) at
  `exit` and on `SIGTERM`, `SIGINT` and `SIGHUP`, so a server does not outlive
  the launcher. Not covered: the launcher being killed outright (`SIGKILL`, a
  crash), after which a server keeps running and its next start fails with
  its port in use.
- **A program with its own window** writes to a file,
  `output/<name>.log` beside the catalog (replaced at each start), so it
  runs on untroubled after the launcher has gone. If it ends with a failure
  the end of that file is shown.
- Colour codes and carriage-return progress are taken out of shown lines; a
  line is cut at 400 characters; 200 lines are kept per entry.

`LAUNCHER_STATE_DIR` puts the catalog and `output/` in another folder, for
test runs.

## Installing it

`scripts/install-launcher.sh` installs the launcher for the current user, on
macOS as an app and on Linux as an entry in the desktop's menu. What it
installs is one of three things: a published build (`--release`, below), a
binary it is given (`--binary PATH`), or, with neither, the release binary it
builds in its own checkout. The catalog is not part of what is installed and
is never touched. `--help` lists the options and how to undo an install.

### Releases: how a machine gets the launcher

A machine does not need a checkout or Rust, and nothing is copied to it by
hand. The launcher reaches it the way the code does:

```
dev -> CI -> main -> "Launcher release" workflow -> the release -> install-launcher.sh --release
```

- `.github/workflows/launcher-release.yml` is run by hand on `main`, the
  commit CI passed. It builds the launcher for Linux (aarch64 and x86-64),
  macOS (arm64) and Windows (x86-64), and publishes them together as the
  release `launcher-v<version>`, the version being `launcher/Cargo.toml`'s. A
  version that is already out is refused, so a change is released by raising
  the version first.
- Each release holds a `.tar.gz` per system (a `.zip` for Windows: unpack it
  and run `launcher.exe`), `install-launcher.sh` itself, and `SHA256SUMS`.
- On the machine, `install-launcher.sh --release` finds the newest release
  whose tag begins `launcher-v` (or takes a tag), fetches the build for that
  system and processor, checks it against the release's `SHA256SUMS`, and
  installs it. The same command updates an installed launcher. With no
  checkout, the script comes from the release:

```
curl -fsSL https://github.com/jaylauffer/loadngo/releases/download/launcher-v0.1.0/install-launcher.sh | sh -s -- --release launcher-v0.1.0
```

The Linux builds are made on Ubuntu 24.04 and need glibc 2.39 or newer
(Debian 13, Raspberry Pi OS of that base).

### On a Linux desktop

The binary goes to `~/.local/bin/loadngo-launcher`, its icon to
`~/.local/share/icons/hicolor/<size>/apps/loadngo-launcher.png` (48 to 256,
made from `launcher/assets/icon-1024.png`), and
`~/.local/share/applications/loadngo-launcher.desktop` puts "loadngo
Launcher" in the desktop's menu. The entry is named for the window's app id
(`loadngo-launcher`), which is how a Wayland desktop gives the open window
the same icon.

`--pin` keeps it in sight once its window is closed (Jay on dolores,
2026-11-18, having closed it: "Now I have nothing that can be seen?"): the
entry is also put on the desktop, and on a Raspberry Pi desktop its name is
added to the top bar's launchers (`launchers=` in
`~/.config/wf-panel-pi/wf-panel-pi.ini`, started from the system's list when
the user has none; the file as it was is kept beside it as
`wf-panel-pi.ini.before-loadngo-launcher`). Without `--pin` the menu entry
(under Accessories there) is the only way in.

### As a Mac app

The script makes `/Applications/loadngo Launcher.app` (`~/Applications` if that cannot be
written): the binary, `launcher/assets/icon-1024.png` as its icon (drawn by
`launcher/assets/draw-icon.py`), bundle id `com.loadngo.launcher`, signed ad
hoc. Spotlight, Launchpad and the Dock then find it as "loadngo Launcher".
The app is a copy: run the script again after changing the launcher. The
catalog is not part of the app and is not touched.

What an entry starts runs as a child of the app, so the first time one of
them reaches somewhere macOS guards (a removable drive, the camera), macOS
asks on the launcher's behalf.

## Opening things: `open_in_system`

The launcher added one primitive to `loadngo-host-desktop`:
`open_in_system(target)` opens an address, file or folder the way a
double-click would (`/usr/bin/open`, `xdg-open`, `ShellExecuteW`). It blocks
until the system has taken the request, so it is called from an offloaded
job.

## Checked

2026-11-16, macOS (M4 Pro), from the app bundle started through
LaunchServices (`open -a`) with a scratch catalog:

- `npm --version` as a build step ran with the bare environment a Dock launch
  has (11.19.1 shown): the shell's `PATH` reached it.
- A service (`python3 -m http.server`) started, its `ready` text was seen,
  its page was fetched, and the opener was called for it (in this run with a
  target that does not exist, so nothing opened: the refusal showed above
  the list).
- A build step's lines, then its program "Running in its own window".
- A program that exits 101 showed red with the reason; its last lines were
  in `output/`.
- `SIGTERM` to the launcher: the service was gone within 0.3 s; the program
  in its own window was still running (and was then stopped by hand).
- In use the same evening, from `/Applications`: a Vite dev server started
  with Start, its `Local:` line seen, its page opened in the browser, no
  refusal.
- Idle, with that server running under it, over 30 s (`top -l 2 -s 30`):
  0.0% CPU and no CPU time added, 22 context switches, 3 idle wake-ups,
  9 threads, 28 MB.

Unit tests (`cargo test -p loadngo-launcher`, 26): the catalog's reading and
refusals; the window through synthetic input (start, stop twice for a
program, a service's page on ready and never while stopping, failures,
reload keeping what runs, keys, scrolling); and, on Unix, real child
processes (a failed build starts nothing, stop ends a service's children,
stopping a build starts nothing after it, what survives the first request to
stop is asked again, what ignores it is killed, quitting stops services and
leaves programs). Three of them were checked against the code broken on
purpose (no second request; quitting stops everything; a stopped build
going on to its program): each failed.

## Not done

- Windows has no install script: there the binary is run as it is, and it
  has not been run by hand; its stop (`taskkill`) and `ShellExecuteW` are
  type-checked only.
- A release is checked by its SHA-256 only, which guards the download, not
  who published it: the signed-release question is open in
  `docs/RELEASE_VERSIONING_AND_INTEGRITY.md`.
- The Dock's Quit (the `exit` hook) was not exercised, only the signal path
  that shares its code.
- After the launcher is killed outright, a service it started is not found
  again (see above).
- No search, no icons per entry, one catalog file per machine.
