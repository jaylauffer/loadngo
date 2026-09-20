# PDF rendering in loadngo

Recorded 2026-09-20 from a design discussion with Jay, prompted by wanting
to preview archived PDFs from `archive_cas_browser` (see
[`ARCHIVE_CAS_BROWSER.md`](ARCHIVE_CAS_BROWSER.md)) without shelling out to
an OS viewer. The stop-gap below (pdfium via FFI) is built, behind the
`pdf-preview` feature; the native renderer described after it is intent and
plan only, not built yet.

## Intent

loadngo's destination is a **native, pure-Rust PDF renderer**, built and
owned the same way as its GLES/Metal/DX12 backends or `loadngo-weights`: no
shelling out to a system tool, no permanent runtime dependency on a
non-Rust library. This is a real, multi-part engineering effort -- a PDF
page is drawn by its own small stack-based operator language, not just
"decode bytes and blit" -- and the plan below scopes it so it stays
buildable instead of open-ended.

**Until the native renderer exists, PDF preview is served by an interim,
explicitly temporary dependency on a mature C/C++ library via FFI.** The
stop-gap is not the destination architecture; see "Retirement" below.

## Stop-gap: pdfium via FFI

[pdfium](https://pdfium.googlesource.com/pdfium/) (Chrome's PDF engine),
via the [`pdfium-render`](https://github.com/ajrcarey/pdfium-render) Rust
bindings, is the recommended stop-gap over the other mature C/C++ PDF
libraries:

| | pdfium | mupdf | poppler |
|---|---|---|---|
| License | Apache-2.0 / BSD-3 mix | AGPL (commercial license needed for most closed uses) | GPL |
| Prebuilt binaries for our platform matrix | Yes -- [`pdfium-binaries`](https://github.com/bblanchon/pdfium-binaries) publishes macOS, iOS, Linux, Android, and Windows builds | No general prebuilt set; expects building the C++ tree per target | Linux-first; weak macOS/iOS/Android/Windows story |
| Rust bindings | `pdfium-render`, actively maintained | `mupdf-rs`, smaller community | none well-maintained |
| Real-world hardening | Ships in Chrome; sees enormous fuzzing/exposure to malformed and hostile PDFs | Mature, smaller install base | Mature, smaller install base |

loadngo's platform matrix is macOS/iOS, Android, Linux, and Windows as
equal-priority targets (see root `AGENTS.md`), which is exactly the matrix
`pdfium-binaries` already publishes -- the deciding factor over mupdf's
otherwise-comparable maturity is not having to own cross-compiling a large
C++ tree per platform ourselves, on top of avoiding AGPL entirely.

Scope of the stop-gap, as built:

- Page-to-raster only, and only the first page: renders to an RGBA bitmap
  loadngo uploads as a texture through the existing renderer path (the same
  path PNG/JPEG preview uses via the `image` crate).
- Behind the `pdf-preview` Cargo feature on `loadngo-host-desktop`, off by
  default; no sng-* game enables it, and enabling it adds no dependency to
  any game's own build. `archive_cas_browser`'s own Preview action is the
  only caller today.
- No PDF *editing*, form-filling, or text extraction beyond what a preview
  needs.

### Required library: pdfium from bblanchon/pdfium-binaries

**loadngo does not bundle or download the pdfium library itself — you must
install one by hand before PDF preview can render anything.** The `pdfium-render`
Rust crate is only bindings; it has no PDF engine of its own.

The required prebuilt library comes from
**[`bblanchon/pdfium-binaries`](https://github.com/bblanchon/pdfium-binaries)**
(not Google's own pdfium repo, which doesn't publish binaries) — a
community project that packages Google's pdfium as Developer-ID-signed,
Apache-2.0/BSD-licensed downloads for every platform loadngo targets.

**Install:**

1. Download the archive for your platform from that repo's
   [Releases page](https://github.com/bblanchon/pdfium-binaries/releases)
   (or `gh release download <tag> --repo bblanchon/pdfium-binaries --pattern
   'pdfium-<platform>.tgz'`) — e.g. `pdfium-mac-arm64.tgz` for Apple
   Silicon, `pdfium-linux-x64.tgz` for x86_64 Linux, `pdfium-win-x64.zip`
   for 64-bit Windows.
2. Extract it somewhere outside any git checkout — e.g.
   `~/.loadngo/vendor/pdfium-<platform>/` (a local machine artifact, the
   same convention as the PQ signing keys under `~/.loadngo/keys`; never
   commit it to a repo).
3. Point `LOADNGO_PDFIUM_LIBRARY` at the **directory** containing the
   platform library file (`lib/libpdfium.dylib` on macOS,
   `lib/libpdfium.so` on Linux, `bin/pdfium.dll` on Windows — check the
   archive's exact layout, it varies slightly by platform) before running
   `archive_cas_browser` built with `--features pdf-preview`. Without a
   library at that path (or found on the OS's normal library search path,
   the fallback if the variable is unset), the Preview action reports the
   missing library clearly instead of crashing.

**Already done on the Mac mini**: `pdfium-mac-arm64` (`chromium/8057`) is
downloaded and extracted to `~/.loadngo/vendor/pdfium-mac-arm64/lib`.
Run with `LOADNGO_PDFIUM_LIBRARY=~/.loadngo/vendor/pdfium-mac-arm64/lib` to
use it — nothing further to install there.

**Verified end to end on 2026-09-20**, not just compiled: rendered a real
multi-paragraph PDF (`/System/Library/ProductDocuments/ProductGuides/ENERGY
STAR.pdf`) through this exact code path and visually confirmed correct
text, layout, and colors (no channel-swap or corruption) in the output
bitmap. One real bug this caught: `PdfRenderConfig::scale_page_to_display_size`
auto-rotates a landscape page 90 degrees (documented behavior, tuned for
fitting a wide page onto a portrait display) -- exactly wrong for a
preview, which should keep a page's own orientation. Fixed by using
`set_target_width` + `set_maximum_height` instead, which fits within the
same bounding box without rotating.

## Retirement

The stop-gap is retired once the native renderer reaches parity for this
specific use case -- rendering a preview inside `archive_cas_browser` (and
any later "preview a well-known file format" surface) -- not before, and
not on a calendar date. Track native-renderer progress against the
milestone below; only drop the pdfium dependency once that milestone
renders the same preview correctly without it.

## Native renderer, milestone 1 (buildable, not open-ended)

Scoped to PDFs loadngo's own tooling would produce, or a small hand-picked
real-world sample -- not "any PDF found in the wild." Real-world PDF
fidelity (the reason mupdf/poppler/pdfium are each huge codebases) is
explicitly deferred past milestone 1:

- **Fonts:** embedded TrueType/OpenType (CFF) only. loadngo already
  depends on [`fontdue`](https://github.com/mooman219/fontdue) (pure Rust)
  for glyph rasterization on `host-desktop` today -- that solves "turn this
  font's outlines into pixels." It does **not** solve PDF's own
  byte-code-to-glyph mapping (`/Encoding`, `Differences` arrays, embedded
  CMaps, `Identity-H` for CID fonts), which is bespoke glue this milestone
  still has to write. Base-14 standard fonts (Helvetica, Times, ... with no
  embedded font data, common in real-world PDFs) are out of scope for
  milestone 1; a PDF that relies on them falls back to the pdfium stop-gap.
- **Compression:** FlateDecode only, via the `flate2` dependency already in
  the workspace.
- **Graphics:** path fill/stroke with nonzero and even-odd winding rules,
  affine transforms, clipping. No transparency groups or soft masks yet.
- **Color/images:** DeviceRGB and DeviceGray; DCTDecode (JPEG) images via
  the existing `image` crate path used for texture loading elsewhere.
- **Explicitly out of scope for milestone 1:** encryption, CMYK/ICC color,
  JBIG2/CCITTFax (fax/scan compression), Type1 and Type3 fonts,
  forms/annotations (AcroForm/XFA), base-14 font substitution.

A PDF outside this milestone's support is not a bug to fix in the native
renderer -- it's routed to the pdfium stop-gap until a later milestone
explicitly extends coverage.

## Non-goals

- **DOCX preview is explicitly out of scope**, for either the stop-gap or
  the native renderer. OOXML is a word-processor layout format (flowed
  text, styles, embedded objects), a different order of complexity from a
  page-description format like PDF, and isn't part of this effort.
- This doc does not cover PDF *creation* (loadngo has no need to author
  PDFs today).

## Where this is used

`archive_cas_browser`'s "Preview" action (see
[`ARCHIVE_CAS_BROWSER.md`](ARCHIVE_CAS_BROWSER.md)) is the first and, so
far, only consumer: it reads and verifies the selected file's blob bytes
against the manifest's recorded hash (the same check `archive_cas_verify`
performs, for one object), decodes them (pdfium today; the native renderer
once it covers the case), and renders into the browser's existing
texture/paint pipeline -- never handing the bytes to an external viewer or
writing them back to disk.
