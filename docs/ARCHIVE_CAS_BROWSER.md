# Archive CAS browser

`archive_cas_browser` is the native, local visual companion to the Archive
CAS command-line tools. It makes the coverage and shape of a preservation
capture understandable without treating the archive as a media library.

It is deliberately read-only: the browser enumerates `manifests/*.json`,
requires each manifest to be canonical, and derives its views from that
metadata. It does **not** create CAS directories, open blob objects, preview
file contents, restore files, upload data, or publish anything to the loadngo
Task plane.

## Run

```sh
cargo run -p loadngo-host-desktop --features archive-cas-browser --bin archive_cas_browser -- \
  --cas-root /Volumes/Loadngo\ Archive\ Staging/loadngo-archive-cas
```

To open one particular manifest first:

```sh
cargo run -p loadngo-host-desktop --features archive-cas-browser --bin archive_cas_browser -- \
  --cas-root /path/to/loadngo-archive-cas \
  --manifest /path/to/loadngo-archive-cas/manifests/<archive-id>-<root>.json
```

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
`Backspace` goes up one folder, and `Esc` closes the app. The browser warns
when a JSON file was skipped because it was unreadable, invalid, or not in the
canonical Archive CAS form; it leaves that file untouched.

For a proof that manifest-referenced blobs are present and hash correctly, use
`archive_cas_verify`. For an owner-authorized selected-file recovery, use
`archive_cas_restore`; the browser does not perform either operation.
