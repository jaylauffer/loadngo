# Archive CAS browser

`archive_cas_browser` is the native, local visual companion to the Archive
CAS command-line tools. It makes the coverage and shape of a preservation
capture understandable without treating the archive as a media library.

It is deliberately read-only: the browser enumerates `manifests/*.json`,
requires each manifest to be canonical, and derives its views from that
metadata. It does **not** create CAS directories, restore files, upload
data, or publish anything to the loadngo Task plane. The one exception is
the Preview action (see below), which reads and verifies a single selected
file's blob bytes to decode and display it in-window -- it never writes
those bytes back out or hands them to an external process.

## Run

`archive_cas_browser --help` (or running it with no arguments beyond the
binary name) prints the full flag reference; this doc doesn't repeat it. Two
ways to point it at a CAS root:

```sh
cargo run -p loadngo-host-desktop --features archive-cas-browser --bin archive_cas_browser -- \
  --cas-root /Volumes/Loadngo\ Archive\ Staging/loadngo-archive-cas
```

```sh
cargo run -p loadngo-host-desktop --features archive-cas-browser --bin archive_cas_browser
```

The second form -- launching with no `--cas-root` at all -- scans attached
storage (`/Volumes` on macOS, `/media`/`/run/media`/`/mnt` on Linux, drive
letters on Windows, plus any extra roots named in `LOADNGO_CAS_SCAN_ROOTS`)
for directories that look like an Archive CAS root (an `objects/` and a
`manifests/` subdirectory), bounded to three directory levels below each
mount point. It prints what it found before opening the window, and opens
every root it finds side by side -- exactly as if each had been passed as
its own `--cas-root`. If it finds none, it explains where it looked and
exits without launching a window rather than opening one with nothing in it;
pass `--cas-root` explicitly, or mount the drive that holds the archive.

To open one particular manifest first:

```sh
cargo run -p loadngo-host-desktop --features archive-cas-browser --bin archive_cas_browser -- \
  --cas-root /path/to/loadngo-archive-cas \
  --manifest /path/to/loadngo-archive-cas/manifests/<archive-id>-<root>.json
```

Add `,pdf-preview` to `--features` to enable PDF preview (see "Preview"
below); it is off by default and independent of every sng-* game, which
never enable it.

## What it shows

- A manifest index with capture state: complete, complete within a declared
  owner-approved scope, or incomplete because an unreadable entry remains.
- A folder-style path explorer derived from manifest paths. Click folders to
  navigate; click files, symlinks, unreadable entries, or exclusions to see
  their recorded metadata.
- A compact visual summary of entry coverage, logical file bytes, unique blob
  bytes, and within-manifest deduplication. These numbers describe manifest
  references, not a claim that every blob has been rehashed in this session.
- The immutable manifest root and the source label needed to relate the view
  back to `archive_cas_verify`.

`R` refreshes only the manifest index, `Home` returns to the manifest root,
`Backspace` goes up one folder, and `Esc` closes the app (or, while a preview
or a pending removal is open, closes that first). The browser warns when a
JSON file was skipped because it was unreadable, invalid, or not in the
canonical Archive CAS form; it leaves that file untouched.

For a proof that manifest-referenced blobs are present and hash correctly, use
`archive_cas_verify`. For an owner-authorized selected-file recovery, use
`archive_cas_restore`; the browser does not perform either operation.

## Preview

Selecting a regular file whose extension the browser recognizes shows a
"Preview (V)" action in the inspector panel. It reads that one file's blob
bytes from the CAS, verifies them against the manifest's recorded hash (the
same check `archive_cas_verify` performs, just for one object instead of
every object), decodes them, and replaces the path explorer panel with the
decoded image -- `Esc` or the "Close" button returns to the explorer.
Nothing is written to disk and nothing is handed to an external viewer.

- **PNG and JPEG** decode unconditionally, via the `image` crate already
  used for texture loading elsewhere in loadngo.
- **PDF** (first page only) requires this binary to be built with
  `--features pdf-preview`, and requires a real pdfium library to be
  available at runtime -- loadngo does not bundle or download one. Set
  `LOADNGO_PDFIUM_LIBRARY` to the directory holding a prebuilt
  `libpdfium.dylib`/`libpdfium.so`/`pdfium.dll` (see
  [`bblanchon/pdfium-binaries`](https://github.com/bblanchon/pdfium-binaries)),
  or install one where the OS library loader finds it and omit the
  variable. Without either, a `.pdf` entry's Preview action reports the
  missing library instead of failing to build or crashing. This is an
  explicit, temporary stop-gap; see
  [`PDF_RENDERING.md`](PDF_RENDERING.md) for the destination (a native Rust
  PDF renderer) and why pdfium was chosen for the interim.
- Any other extension has no Preview action offered at all.

A file larger than 64 MiB is refused with a clear message rather than
attempted -- decoding happens synchronously on the same thread as every
other action in this browser (sign, remove, refresh), so this is a ceiling
against a multi-second freeze, not a format-specific limit.
