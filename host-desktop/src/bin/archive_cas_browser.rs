//! A local, metadata-only browser for loadngo Archive CAS manifests.
//!
//! The browser deliberately reads only canonical manifest JSON: it does not
//! upload data or write anything besides the manifest edits its own actions
//! make. The one exception is the Preview action (see the `preview`
//! module), which reads and verifies a single selected file's blob bytes
//! to decode and display it in-window -- it never writes those bytes back
//! out or hands them to an external process.

use anyhow::{anyhow, bail, Context, Result};
use data::archive_cas::{ArchiveCasStorage, ArchiveEntry, ArchiveManifest, ArchiveObject};
use data::cas::CasHash;
use data::cli::{discover, ArgDoc, Usage};
use loadngo_host_core::{DecodedImage, FrameDemand, HostKey, InputSnapshot, WindowDescriptor};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use ui_core::{
    Color, HorizontalAlign, LabelModel, PanelModel, Point, Rect, TextOverflow, TextStyle,
    VerticalAlign,
};

const WINDOW_WIDTH: i32 = 1_560;
const WINDOW_HEIGHT: i32 = 980;
const OUTER_GUTTER: f32 = 18.0;
const HEADER_HEIGHT: f32 = 118.0;
const PANEL_GAP: f32 = 14.0;
const PANEL_INSET: f32 = 14.0;
const ROW_HEIGHT: f32 = 38.0;
const TITLE_FONT: u16 = 23;
const SECTION_FONT: u16 = 17;
const BODY_FONT: u16 = 15;
const CAPTION_FONT: u16 = 13;

const BACKGROUND: Color = Color::rgba(0x0d, 0x12, 0x1b, 0xff);
const PANEL_BACKGROUND: Color = Color::rgba(0x19, 0x22, 0x31, 0xf7);
const PANEL_BORDER: Color = Color::rgba(0x5f, 0x76, 0x96, 0xff);
const TEXT: Color = Color::rgba(0xec, 0xf1, 0xfb, 0xff);
const MUTED: Color = Color::rgba(0xb4, 0xc2, 0xd7, 0xff);
const ACCENT: Color = Color::rgba(0x68, 0xc9, 0xee, 0xff);
const SELECTED: Color = Color::rgba(0x2a, 0x65, 0x86, 0xff);
const COMPLETE: Color = Color::rgba(0x74, 0xd2, 0x9a, 0xff);
const CAUTION: Color = Color::rgba(0xf2, 0xbc, 0x5c, 0xff);
const DANGER: Color = Color::rgba(0xf0, 0x87, 0x87, 0xff);
const HOVER_FILL: Color = Color::rgba(0x3a, 0x86, 0xae, 0xff);
const HOVER_BORDER: Color = Color::rgba(0x9a, 0xe4, 0xff, 0xff);
const ROW_HOVER_BORDER: Color = Color::rgba(0x4a, 0x6a, 0x88, 0xff);

fn main() {
    if let Err(error) = run() {
        eprintln!("archive_cas_browser: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse()?;
    let catalog = ArchiveCatalog::read(&args.cas_roots)?;
    let selected_archive = catalog.initial_selection(args.manifest.as_deref());
    let signing = args.signing()?;
    let devices = args
        .cas_roots
        .iter()
        .map(|root| (root.clone(), device_info::lookup(root)))
        .collect();
    loadngo_host_desktop::launch(window_descriptor(), None, async move {
        BrowserApp::new(args.cas_roots, devices, catalog, selected_archive, signing)
            .run()
            .await;
    });
    Ok(())
}

/// Loaded key material and the identity to sign as, kept for the process
/// lifetime once the (optional) `--public-key`/`--private-key` args are
/// read. Without it the "Sign this manifest" action is simply absent -- the
/// browser never invents or reuses a key of its own.
struct SigningContext {
    signer_identity: String,
    public_key: loadngo_pq_crypto::PublicKey,
    private_key: loadngo_pq_crypto::PrivateKey,
}

impl std::fmt::Debug for SigningContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SigningContext")
            .field("signer_identity", &self.signer_identity)
            .field("public_key", &self.public_key)
            .finish_non_exhaustive()
    }
}

/// Best-effort physical device/volume info for a CAS root, shown as a
/// header above that root's archives so it's clear which drive each one
/// actually lives on. Never fails the caller -- a lookup that can't
/// identify the device just falls back to showing the mount path alone.
#[derive(Debug, Clone, Default)]
struct DeviceInfo {
    /// e.g. "SSD 960 EVO 1TB, USB (external)" -- empty if nothing could be
    /// determined beyond the mount path itself.
    label: String,
    mount_point: String,
    free_bytes: Option<u64>,
    total_bytes: Option<u64>,
}

#[cfg(target_os = "macos")]
mod device_info {
    use super::DeviceInfo;
    use std::path::Path;
    use std::process::Command;

    pub fn lookup(cas_root: &Path) -> DeviceInfo {
        let mount_point =
            mount_point_for(cas_root).unwrap_or_else(|| cas_root.display().to_string());
        let (total_bytes, free_bytes) = df_totals(&mount_point);
        let mut info = DeviceInfo {
            mount_point,
            total_bytes,
            free_bytes,
            label: String::new(),
        };

        let Some(volume_plist) = diskutil_info_plist(&info.mount_point) else {
            return info;
        };
        let volume_name = plist_string(&volume_plist, "VolumeName");
        let bus = plist_string(&volume_plist, "BusProtocol");
        let external = plist_bool(&volume_plist, "RemovableMediaOrExternalDevice").unwrap_or(false);
        let device_id = plist_string(&volume_plist, "DeviceIdentifier");

        // The physical media's model name lives on the parent whole-disk
        // entry (e.g. "disk7"), not the volume slice ("disk7s1") that
        // `diskutil info <mount point>` itself reports.
        let media_name = device_id
            .as_deref()
            .and_then(parent_disk_identifier)
            .and_then(|parent| diskutil_info_plist(&parent))
            .and_then(|parent_plist| plist_string(&parent_plist, "MediaName"))
            .filter(|name| !name.trim().is_empty());

        let mut parts = Vec::new();
        if let Some(media) = media_name {
            parts.push(media);
        } else if let Some(volume) = volume_name {
            parts.push(volume);
        }
        if let Some(bus) = bus {
            parts.push(if external {
                format!("{bus}, external")
            } else {
                bus
            });
        }
        info.label = parts.join(", ");
        info
    }

    fn mount_point_for(path: &Path) -> Option<String> {
        let output = Command::new("df").arg("-P").arg(path).output().ok()?;
        if !output.status.success() {
            return None;
        }
        parse_df_mount_point(&String::from_utf8_lossy(&output.stdout))
    }

    /// `df -P` reports six fixed columns -- Filesystem, 512-blocks, Used,
    /// Available, Capacity, Mounted-on -- but a volume name (and so its
    /// mount point) can itself contain spaces ("Zhoenus II"), so `.last()`
    /// on a naive whitespace split silently truncates it to "II". Only the
    /// mount point can contain whitespace; everything before it is a single
    /// token, so join whatever's left after the first five fields.
    fn parse_df_mount_point(df_output: &str) -> Option<String> {
        let last_line = df_output.lines().nth(1)?;
        let fields: Vec<&str> = last_line.split_whitespace().collect();
        if fields.len() < 6 {
            return None;
        }
        Some(fields[5..].join(" "))
    }

    fn df_totals(mount_point: &str) -> (Option<u64>, Option<u64>) {
        let Ok(output) = Command::new("df").arg("-Pk").arg(mount_point).output() else {
            return (None, None);
        };
        if !output.status.success() {
            return (None, None);
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let Some(fields) = text
            .lines()
            .nth(1)
            .map(|line| line.split_whitespace().collect::<Vec<_>>())
        else {
            return (None, None);
        };
        // Filesystem, 1024-blocks, Used, Available, Capacity, Mounted-on
        let total = fields.get(1).and_then(|value| value.parse::<u64>().ok());
        let free = fields.get(3).and_then(|value| value.parse::<u64>().ok());
        (total.map(|kib| kib * 1024), free.map(|kib| kib * 1024))
    }

    fn parent_disk_identifier(device_id: &str) -> Option<String> {
        // "disk7s1" -> "disk7"; a whole-disk id with no slice has nothing
        // to strip and is already its own parent. `rfind`, not `find`: the
        // slice separator is the *last* 's', not the one already in "disk".
        let cut = device_id.rfind('s').filter(|&index| {
            index > 0
                && device_id[index + 1..].chars().all(|c| c.is_ascii_digit())
                && index + 1 < device_id.len()
        });
        Some(match cut {
            Some(index) => device_id[..index].to_string(),
            None => device_id.to_string(),
        })
    }

    /// A deliberately narrow plist reader: `diskutil info -plist` emits a
    /// flat top-level dict of scalar values for the keys this module reads,
    /// so a line-by-line `<key>NAME</key>` -> next-line-value scan is
    /// sufficient. It is not a general plist parser and does not need to be
    /// one -- this is a best-effort, informational label, never load-bearing.
    fn diskutil_info_plist(id_or_path: &str) -> Option<Vec<String>> {
        let output = Command::new("diskutil")
            .args(["info", "-plist", id_or_path])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        Some(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::to_string)
                .collect(),
        )
    }

    fn plist_string(lines: &[String], key: &str) -> Option<String> {
        plist_value_line(lines, key).and_then(|line| {
            let line = line.trim();
            line.strip_prefix("<string>")
                .and_then(|rest| rest.strip_suffix("</string>"))
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
    }

    fn plist_bool(lines: &[String], key: &str) -> Option<bool> {
        let line = plist_value_line(lines, key)?.trim().to_string();
        if line == "<true/>" {
            Some(true)
        } else if line == "<false/>" {
            Some(false)
        } else {
            None
        }
    }

    fn plist_value_line<'a>(lines: &'a [String], key: &str) -> Option<&'a str> {
        let needle = format!("<key>{key}</key>");
        let index = lines.iter().position(|line| line.trim() == needle)?;
        lines.get(index + 1).map(String::as_str)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn lines(text: &str) -> Vec<String> {
            text.lines().map(str::to_string).collect()
        }

        #[test]
        fn parses_string_bool_and_missing_keys_from_a_real_diskutil_sample() {
            let plist = lines(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
	<key>BusProtocol</key>
	<string>USB</string>
	<key>DeviceIdentifier</key>
	<string>disk7s1</string>
	<key>RemovableMediaOrExternalDevice</key>
	<true/>
	<key>Bootable</key>
	<false/>
	<key>MediaName</key>
	<string></string>
	<key>VolumeName</key>
	<string>Zhoenus II</string>
</dict>
</plist>"#,
            );
            assert_eq!(plist_string(&plist, "BusProtocol"), Some("USB".to_string()));
            assert_eq!(
                plist_string(&plist, "VolumeName"),
                Some("Zhoenus II".to_string())
            );
            // An empty <string></string> is treated as absent, same as a
            // missing key -- callers fall back past it either way.
            assert_eq!(plist_string(&plist, "MediaName"), None);
            assert_eq!(
                plist_bool(&plist, "RemovableMediaOrExternalDevice"),
                Some(true)
            );
            assert_eq!(plist_bool(&plist, "Bootable"), Some(false));
            assert_eq!(plist_string(&plist, "NoSuchKey"), None);
        }

        #[test]
        fn parent_disk_identifier_strips_a_slice_suffix_only() {
            assert_eq!(parent_disk_identifier("disk7s1"), Some("disk7".to_string()));
            assert_eq!(
                parent_disk_identifier("disk10s1"),
                Some("disk10".to_string())
            );
            // A whole-disk identifier has no slice to strip.
            assert_eq!(parent_disk_identifier("disk7"), Some("disk7".to_string()));
        }

        #[test]
        fn parse_df_mount_point_keeps_a_space_in_the_volume_name() {
            let output = "Filesystem   512-blocks      Used  Available Capacity  Mounted on\n\
                           /dev/disk7s1 1953115488 102451488 1850259416     6%    /Volumes/Zhoenus II\n";
            assert_eq!(
                parse_df_mount_point(output),
                Some("/Volumes/Zhoenus II".to_string())
            );
        }

        #[test]
        fn parse_df_mount_point_handles_a_plain_root_mount() {
            let output = "Filesystem    512-blocks      Used Available Capacity  Mounted on\n\
                           /dev/disk3s1s1 976490568 12345678 900000000     2%    /\n";
            assert_eq!(parse_df_mount_point(output), Some("/".to_string()));
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod device_info {
    use super::DeviceInfo;
    use std::path::Path;

    // No per-OS device/volume model lookup here yet -- this tool's real
    // operator today is the Mac mini. Still fully functional without it:
    // every CAS root just shows its mount path with no device label.
    pub fn lookup(cas_root: &Path) -> DeviceInfo {
        DeviceInfo {
            mount_point: cas_root.display().to_string(),
            ..Default::default()
        }
    }
}

/// Decodes a well-known file format's verified bytes into an RGBA bitmap
/// for the Preview action, without ever writing them back to disk or
/// handing them to an external viewer. PNG/JPEG go through the `image`
/// crate (already a workspace dependency used for texture loading
/// elsewhere). PDF goes through the `pdf` submodule (pdfium via FFI, an
/// explicit temporary stop-gap -- see `docs/PDF_RENDERING.md`) only when
/// this binary is built with `--features pdf-preview`; without that
/// feature, a `.pdf` entry simply has no Preview action offered.
mod preview {
    use super::DecodedImage;
    use anyhow::{Context, Result};

    /// The largest side, in pixels, a preview is ever decoded or rendered
    /// at. Bounds every preview's memory and texture-upload cost
    /// regardless of the source file's own resolution or PDF page size.
    const MAX_PREVIEW_DIMENSION: u32 = 1600;

    /// Refuses to preview a file larger than this. Loading and decoding
    /// happens synchronously on the UI thread, matching every other action
    /// in this browser (sign, remove, refresh) -- this is a hard ceiling
    /// against a multi-second freeze on an accidentally huge file, not a
    /// format-specific limit.
    pub const MAX_PREVIEW_SOURCE_BYTES: u64 = 64 * 1024 * 1024;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum PreviewKind {
        Raster,
        #[cfg(feature = "pdf-preview")]
        Pdf,
    }

    /// The preview kind for a manifest-relative path, by extension, or
    /// `None` if this build offers no preview for it.
    pub fn kind_for_path(path: &str) -> Option<PreviewKind> {
        let extension = std::path::Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str())?
            .to_ascii_lowercase();
        match extension.as_str() {
            "png" | "jpg" | "jpeg" => Some(PreviewKind::Raster),
            #[cfg(feature = "pdf-preview")]
            "pdf" => Some(PreviewKind::Pdf),
            _ => None,
        }
    }

    pub fn decode(kind: PreviewKind, bytes: &[u8]) -> Result<DecodedImage> {
        match kind {
            PreviewKind::Raster => decode_raster(bytes),
            #[cfg(feature = "pdf-preview")]
            PreviewKind::Pdf => pdf::render_first_page(bytes, MAX_PREVIEW_DIMENSION),
        }
    }

    fn decode_raster(bytes: &[u8]) -> Result<DecodedImage> {
        let decoded = image::load_from_memory(bytes).context("not a readable PNG/JPEG image")?;
        let (width, height) =
            downscaled_dimensions(decoded.width(), decoded.height(), MAX_PREVIEW_DIMENSION);
        let decoded = if (width, height) != (decoded.width(), decoded.height()) {
            decoded.resize(width, height, image::imageops::FilterType::Triangle)
        } else {
            decoded
        };
        let rgba = decoded.to_rgba8();
        Ok(DecodedImage::new(
            rgba.width(),
            rgba.height(),
            rgba.into_raw(),
        ))
    }

    fn downscaled_dimensions(width: u32, height: u32, max_dimension: u32) -> (u32, u32) {
        if width <= max_dimension && height <= max_dimension {
            return (width.max(1), height.max(1));
        }
        let scale = (max_dimension as f32 / width.max(height) as f32).min(1.0);
        (
            ((width as f32 * scale).round() as u32).max(1),
            ((height as f32 * scale).round() as u32).max(1),
        )
    }

    /// Native PDF page rendering via pdfium (Google's PDF engine), through
    /// FFI. This is a deliberate, temporary stop-gap: loadngo's destination
    /// is its own native Rust PDF renderer. See `docs/PDF_RENDERING.md` for
    /// the full plan, the license/prebuilt-binary reasoning for choosing
    /// pdfium over mupdf/poppler, and the native renderer's milestone 1
    /// scope. loadngo does not bundle or download pdfium itself.
    #[cfg(feature = "pdf-preview")]
    mod pdf {
        use super::DecodedImage;
        use anyhow::{anyhow, Context, Result};
        use pdfium_render::prelude::*;
        use std::cell::RefCell;
        use std::rc::Rc;

        thread_local! {
            // `pdfium-render`'s own binding functions are backed by a
            // process-wide singleton and return an error if called a
            // second time, even to reload the same library -- so this
            // binds once per process (lazily, on the first preview) and
            // reuses the result for every later one, success or failure.
            static PDFIUM: RefCell<Option<Result<Rc<Pdfium>, String>>> =
                const { RefCell::new(None) };
        }

        fn shared_pdfium() -> Result<Rc<Pdfium>, String> {
            PDFIUM.with(|cell| {
                cell.borrow_mut()
                    .get_or_insert_with(|| {
                        bind().map(Rc::new).map_err(|error| format!("{error:#}"))
                    })
                    .clone()
            })
        }

        /// Where to find the pdfium dynamic library: an explicit directory
        /// via `LOADNGO_PDFIUM_LIBRARY`, or the system library search path
        /// as a fallback.
        fn bind() -> Result<Pdfium> {
            let bindings = if let Ok(dir) = std::env::var("LOADNGO_PDFIUM_LIBRARY") {
                Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(&dir))
                    .with_context(|| {
                        format!(
                            "failed to load the pdfium library from LOADNGO_PDFIUM_LIBRARY={dir}"
                        )
                    })?
            } else {
                Pdfium::bind_to_system_library().map_err(|error| {
                    anyhow!(
                        "no pdfium library found: set LOADNGO_PDFIUM_LIBRARY to the directory \
                         holding a prebuilt pdfium (see docs/PDF_RENDERING.md), or install one \
                         where the OS library loader can find it ({error})"
                    )
                })?
            };
            Ok(Pdfium::new(bindings))
        }

        pub fn render_first_page(bytes: &[u8], max_dimension: u32) -> Result<DecodedImage> {
            let pdfium = shared_pdfium().map_err(|error| anyhow!(error))?;
            let document = pdfium
                .load_pdf_from_byte_slice(bytes, None)
                .context("not a readable PDF (or it is password-protected)")?;
            let page = document
                .pages()
                .get(0)
                .context("PDF has no pages to preview")?;
            // Deliberately not `scale_page_to_display_size`: it
            // auto-rotates a landscape page 90 degrees to favor a portrait
            // display, which is exactly wrong for a preview -- a wide page
            // should stay wide. `set_target_width` plus `set_maximum_height`
            // fits the page within `max_dimension` on both sides while
            // preserving its own orientation.
            let config = PdfRenderConfig::new()
                .set_target_width(max_dimension as Pixels)
                .set_maximum_height(max_dimension as Pixels);
            let bitmap = page
                .render_with_config(&config)
                .context("failed to render the PDF's first page")?;
            let width =
                u32::try_from(bitmap.width()).context("PDF page rendered at an invalid width")?;
            let height =
                u32::try_from(bitmap.height()).context("PDF page rendered at an invalid height")?;
            Ok(DecodedImage::new(width, height, bitmap.as_rgba_bytes()))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::Cursor;

        fn synthetic_png(width: u32, height: u32) -> Vec<u8> {
            let mut pixels = image::RgbaImage::new(width, height);
            for (x, y, pixel) in pixels.enumerate_pixels_mut() {
                *pixel = image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255]);
            }
            let mut bytes = Vec::new();
            image::DynamicImage::ImageRgba8(pixels)
                .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
                .unwrap();
            bytes
        }

        #[test]
        fn kind_for_path_recognizes_raster_extensions_case_insensitively() {
            assert_eq!(kind_for_path("photo.PNG"), Some(PreviewKind::Raster));
            assert_eq!(kind_for_path("photo.jpg"), Some(PreviewKind::Raster));
            assert_eq!(kind_for_path("photo.JPEG"), Some(PreviewKind::Raster));
            assert_eq!(kind_for_path("no/extension/at/all"), None);
            assert_eq!(kind_for_path("readme.md"), None);
        }

        #[test]
        fn decode_raster_reads_a_real_png_at_its_own_size_when_under_the_cap() {
            let bytes = synthetic_png(32, 16);
            let decoded = decode_raster(&bytes).unwrap();
            assert_eq!((decoded.width, decoded.height), (32, 16));
            decoded.validate_rgba8().unwrap();
        }

        #[test]
        fn decode_raster_downscales_an_oversized_image_and_keeps_aspect_ratio() {
            let bytes = synthetic_png(MAX_PREVIEW_DIMENSION * 2, MAX_PREVIEW_DIMENSION);
            let decoded = decode_raster(&bytes).unwrap();
            assert_eq!(decoded.width, MAX_PREVIEW_DIMENSION);
            assert_eq!(decoded.height, MAX_PREVIEW_DIMENSION / 2);
            decoded.validate_rgba8().unwrap();
        }

        #[test]
        fn decode_raster_rejects_bytes_that_are_not_an_image() {
            assert!(decode_raster(b"not a png").is_err());
        }

        #[test]
        fn downscaled_dimensions_leaves_a_small_image_untouched() {
            assert_eq!(downscaled_dimensions(800, 600, 1600), (800, 600));
        }

        #[test]
        fn downscaled_dimensions_scales_the_longer_side_down_to_the_cap() {
            assert_eq!(downscaled_dimensions(3200, 1600, 1600), (1600, 800));
            assert_eq!(downscaled_dimensions(1600, 3200, 1600), (800, 1600));
        }

        /// A hand-assembled, minimal single-page PDF (no external
        /// dependency on any file on disk): one Catalog, one Pages tree,
        /// one blank 200x100pt Page, with a byte-exact xref table computed
        /// as it's written rather than hardcoded.
        #[cfg(feature = "pdf-preview")]
        fn minimal_one_page_pdf() -> Vec<u8> {
            let objects = [
                "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
                "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
                "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << >> >>\nendobj\n",
            ];
            let mut buffer = b"%PDF-1.4\n".to_vec();
            let mut offsets = Vec::new();
            for object in objects {
                offsets.push(buffer.len());
                buffer.extend_from_slice(object.as_bytes());
            }
            let xref_offset = buffer.len();
            buffer.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
            buffer.extend_from_slice(b"0000000000 65535 f \n");
            for offset in &offsets {
                buffer.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
            }
            buffer.extend_from_slice(
                format!(
                    "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
                    objects.len() + 1,
                    xref_offset
                )
                .as_bytes(),
            );
            buffer
        }

        /// Exercises the real pdfium library end to end, not just this
        /// module's own glue code. Ignored by default: it depends on
        /// `LOADNGO_PDFIUM_LIBRARY` pointing at a real prebuilt pdfium on
        /// whatever machine runs it, which `cargo test` cannot provide --
        /// see docs/PDF_RENDERING.md for where to get one. Run explicitly
        /// with `cargo test --features pdf-preview -- --ignored
        /// render_first_page_produces_a_bitmap_from_a_real_pdfium_library`.
        #[cfg(feature = "pdf-preview")]
        #[test]
        #[ignore]
        fn render_first_page_produces_a_bitmap_from_a_real_pdfium_library() {
            let bytes = minimal_one_page_pdf();
            let decoded =
                pdf::render_first_page(&bytes, 400).expect("pdfium render should succeed");
            decoded.validate_rgba8().unwrap();
            // A 200x100pt page is wider than tall; the render should keep
            // that shape.
            assert!(
                decoded.width > decoded.height,
                "expected a landscape render, got {}x{}",
                decoded.width,
                decoded.height
            );
        }
    }
}

fn window_descriptor() -> WindowDescriptor {
    WindowDescriptor {
        title: "loadngo Archive CAS browser".to_string(),
        width: Some(WINDOW_WIDTH),
        height: Some(WINDOW_HEIGHT),
        high_dpi: true,
        linux_wm_class: Some("loadngo-archive-cas-browser"),
    }
}

#[derive(Debug)]
struct Args {
    cas_roots: Vec<PathBuf>,
    manifest: Option<PathBuf>,
    public_key: Option<PathBuf>,
    private_key: Option<PathBuf>,
    signer_identity: Option<String>,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut cas_roots = Vec::new();
        let mut manifest = None;
        let mut public_key = None;
        let mut private_key = None;
        let mut signer_identity = None;
        // Launching with no --cas-root at all is a legitimate way to start
        // this browser -- it scans attached storage instead of failing --
        // so an empty argv must not be mistaken for a request to see the
        // docs. --help/-h still always works.
        let args = data::cli::read_args(&usage(), false);
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                // Repeatable: one browser window can show every CAS root at
                // once, each labeled with the physical device it lives on,
                // instead of a separate process per root.
                "--cas-root" => cas_roots.push(
                    args.next()
                        .map(PathBuf::from)
                        .ok_or_else(|| anyhow!("--cas-root requires a value"))?,
                ),
                "--manifest" => manifest = args.next().map(PathBuf::from),
                "--public-key" => public_key = args.next().map(PathBuf::from),
                "--private-key" => private_key = args.next().map(PathBuf::from),
                "--signer-identity" => signer_identity = args.next(),
                other => bail!("unknown argument: {other}\n{}", usage().hint()),
            }
        }
        if cas_roots.is_empty() {
            cas_roots = discover_cas_root_or_bail()?;
        }
        // Two `--cas-root` values that resolve to the same real directory
        // (a literal repeat, a relative vs. absolute spelling, or a
        // symlink alias) must show as one device, not two identical
        // banners -- see `data::cli::discover`'s doc comment on the
        // `Macintosh HD -> /` symlink for the discovery-side version of
        // this same bug.
        let cas_roots = data::cli::dedup_by_canonical_path(cas_roots);
        Ok(Self {
            cas_roots,
            manifest,
            public_key,
            private_key,
            signer_identity,
        })
    }

    /// Builds the signing context if key material was supplied. Requires all
    /// three of `--public-key`/`--private-key`/`--signer-identity` together,
    /// or none of them -- a partial set is almost certainly a mistake, not
    /// an intentional "sign disabled" choice.
    fn signing(&self) -> Result<Option<SigningContext>> {
        match (&self.public_key, &self.private_key, &self.signer_identity) {
            (None, None, None) => Ok(None),
            (Some(public_key_path), Some(private_key_path), Some(signer_identity)) => {
                let public_key = data::archive_cas_sign::read_public_key(public_key_path)
                    .context("failed to read --public-key")?;
                let private_key = data::archive_cas_sign::read_private_key(private_key_path)
                    .context("failed to read --private-key")?;
                Ok(Some(SigningContext {
                    signer_identity: signer_identity.clone(),
                    public_key,
                    private_key,
                }))
            }
            _ => bail!(
                "--public-key, --private-key, and --signer-identity must be given together, or not at all"
            ),
        }
    }
}

fn usage() -> Usage {
    const ARGS: &[ArgDoc] = &[
        ArgDoc::repeated(
            "--cas-root",
            "<archive-directory>",
            "an Archive CAS root to show, labeled with the device/volume it's mounted from; repeat to show several roots in one window. Omit entirely to scan attached storage instead",
        ),
        ArgDoc::optional(
            "--manifest",
            "<archive-manifest.json>",
            "manifest to select on launch; defaults to the newest manifest found",
        ),
        ArgDoc::optional(
            "--public-key",
            "<hex-file>",
            "Dilithium2 public key; with --private-key and --signer-identity, adds a \"Sign this manifest\" action in the GUI",
        ),
        ArgDoc::optional("--private-key", "<hex-file>", "Dilithium2 private key matching --public-key"),
        ArgDoc::optional(
            "--signer-identity",
            "<name>",
            "identity to sign as; required together with --public-key and --private-key",
        ),
    ];
    const EXAMPLES: &[&str] = &[
        "cargo run -p loadngo-host-desktop --bin archive_cas_browser --",
        "cargo run -p loadngo-host-desktop --bin archive_cas_browser -- --cas-root /Volumes/Backup/loadngo-archive-cas --cas-root \"/Volumes/Zhoenus II/pudding-cas\"",
    ];
    const NOTES: &[&str] = &[
        "With no --cas-root at all, scans attached storage for existing Archive CAS roots and opens every one it finds.",
        "The key arguments are optional; without them the browser is read/edit-only and a manifest change still needs archive_cas_sign run separately.",
        "Reads only canonical manifest JSON; it never opens, previews, or uploads archive blob payloads.",
    ];
    Usage {
        bin: "archive_cas_browser",
        invocation: "cargo run -p loadngo-host-desktop --bin archive_cas_browser --",
        about: "a local, metadata-only GUI browser for loadngo Archive CAS manifests",
        args: ARGS,
        examples: EXAMPLES,
        notes: NOTES,
    }
}

/// Scans attached storage for existing Archive CAS roots when the caller
/// gave no `--cas-root` at all, printing what it found (or didn't) so a
/// terminal launch explains itself instead of silently guessing. This is
/// deliberately not an interactive picker: the browser already renders one
/// device-labeled banner row per CAS root side by side (see
/// `ArchiveListRow::Device`), so handing it every discovered root reuses
/// that existing view instead of building a second selection UI.
fn discover_cas_root_or_bail() -> Result<Vec<PathBuf>> {
    println!(
        "archive_cas_browser: no --cas-root given; scanning attached storage for an Archive CAS..."
    );
    let mount_points = discover::candidate_mount_points();
    let found = discover::scan_for_cas_roots();
    if found.is_empty() {
        bail!(
            "no Archive CAS root found under {} attached volume(s)\n\n\
             Insert or mount the drive that holds it, then rerun, or pass\n\
             --cas-root <archive-directory> directly. A fresh, empty\n\
             directory also works: archive_cas_ingest creates the CAS\n\
             layout the first time it writes to it.\n\n{}",
            mount_points.len(),
            usage().hint()
        );
    }
    println!("Found {} Archive CAS root(s):", found.len());
    for root in &found {
        println!("  {}", root.display());
    }
    Ok(found)
}

#[derive(Debug, Clone)]
struct ArchiveCatalog {
    archives: Vec<ArchiveRecord>,
    warnings: Vec<String>,
}

impl ArchiveCatalog {
    fn read(cas_roots: &[PathBuf]) -> Result<Self> {
        let mut archives = Vec::new();
        let mut warnings = Vec::new();
        for cas_root in cas_roots {
            let manifests_root = cas_root.join("manifests");
            if !manifests_root.is_dir() {
                bail!(
                    "Archive CAS manifest directory does not exist: {}",
                    manifests_root.display()
                );
            }

            let mut paths = fs::read_dir(&manifests_root)
                .with_context(|| format!("failed to enumerate {}", manifests_root.display()))?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<std::result::Result<Vec<_>, _>>()
                .with_context(|| format!("failed to enumerate {}", manifests_root.display()))?;
            paths.sort();

            for path in paths {
                if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                    continue;
                }
                match ArchiveRecord::read(&path, cas_root.clone()) {
                    Ok(record) => archives.push(record),
                    Err(error) => warnings.push(format!("{}: {error:#}", path.display())),
                }
            }
        }
        // Grouped by CAS root first (in the order given on the command
        // line), then by archive_id, newest-first within that group -- every
        // version of the same archive stays contiguous instead of
        // interleaving with unrelated archives that happen to share a
        // similar timestamp. The archive list is painted as one banner per
        // root, then one heading per archive_id, then its versions, so this
        // sort order is what determines all of that grouping.
        archives.sort_by(|left, right| {
            cas_roots
                .iter()
                .position(|root| *root == left.cas_root)
                .cmp(&cas_roots.iter().position(|root| *root == right.cas_root))
                .then_with(|| left.manifest.archive_id.cmp(&right.manifest.archive_id))
                .then_with(|| {
                    right
                        .manifest
                        .created_at_unix_secs
                        .cmp(&left.manifest.created_at_unix_secs)
                })
        });
        if archives.is_empty() {
            bail!(
                "no readable canonical archive manifests found under: {}",
                cas_roots
                    .iter()
                    .map(|root| root.join("manifests").display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        // A version is superseded when some other manifest's
        // `supersedes_archive_root` names its root -- the append-only chain
        // is the source of truth for "current," not recency of timestamp
        // alone (a stray older file could otherwise look current).
        let superseded_roots: std::collections::BTreeSet<String> = archives
            .iter()
            .filter_map(|archive| archive.manifest.supersedes_archive_root)
            .map(|hash| hash.to_hex())
            .collect();
        for archive in &mut archives {
            archive.is_superseded = superseded_roots.contains(&archive.summary.root);
        }

        Ok(Self { archives, warnings })
    }

    fn initial_selection(&self, requested_manifest: Option<&Path>) -> usize {
        let Some(requested_manifest) = requested_manifest else {
            return 0;
        };
        self.archives
            .iter()
            .position(|archive| {
                archive.manifest_path == requested_manifest
                    || archive.manifest_path.file_name() == requested_manifest.file_name()
            })
            .unwrap_or(0)
    }
}

const DEVICE_ROW_HEIGHT: f32 = 54.0;

/// One row in the left-hand archive list: either a device banner (which CAS
/// root/physical volume the archives beneath it live on) or one archive.
/// Built fresh each frame from `self.catalog`, and shared between painting
/// and click hit-testing so the two can never disagree about row geometry.
#[derive(Debug, Clone)]
enum ArchiveListRow {
    Device(PathBuf),
    Archive(usize),
}

#[derive(Debug, Clone)]
struct ArchiveRecord {
    manifest_path: PathBuf,
    /// Which `--cas-root` this archive came from -- every operation
    /// (removal, sign, refresh) must act against this root, not a single
    /// global one, now that a window can show several at once.
    cas_root: PathBuf,
    manifest: ArchiveManifest,
    summary: ArchiveSummary,
    /// Set by `ArchiveCatalog::read` after loading every manifest: true when
    /// some other manifest in the catalog names this one as its
    /// `supersedes_archive_root`, i.e. this is history, not the current
    /// version of its archive_id.
    is_superseded: bool,
}

impl ArchiveRecord {
    fn read(path: &Path, cas_root: PathBuf) -> Result<Self> {
        let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
        let manifest: ArchiveManifest = serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        let canonical = manifest
            .canonical_bytes()
            .with_context(|| format!("invalid archive manifest {}", path.display()))?;
        if bytes != canonical {
            bail!("manifest is not canonical JSON");
        }
        let summary = summarize_manifest(&manifest)?;
        Ok(Self {
            manifest_path: path.to_path_buf(),
            cas_root,
            is_superseded: false,
            manifest,
            summary,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArchiveSummary {
    root: String,
    directories: u64,
    files: u64,
    symlinks: u64,
    unreadable: u64,
    excluded: u64,
    logical_bytes: u64,
    unique_objects: u64,
    unique_object_bytes: u64,
}

fn summarize_manifest(manifest: &ArchiveManifest) -> Result<ArchiveSummary> {
    let mut summary = ArchiveSummary {
        root: manifest.digest()?.to_hex(),
        directories: 0,
        files: 0,
        symlinks: 0,
        unreadable: 0,
        excluded: 0,
        logical_bytes: 0,
        unique_objects: 0,
        unique_object_bytes: 0,
    };
    let mut objects = BTreeMap::<CasHash, u64>::new();
    for entry in &manifest.entries {
        match entry {
            ArchiveEntry::Directory { .. } => summary.directories += 1,
            ArchiveEntry::File { object, .. } => {
                summary.files += 1;
                summary.logical_bytes =
                    checked_add(summary.logical_bytes, object.size, "logical bytes")?;
                match objects.insert(object.hash, object.size) {
                    Some(previous_size) if previous_size != object.size => {
                        bail!(
                            "manifest assigns inconsistent sizes to object {}",
                            object.hash
                        )
                    }
                    Some(_) => {}
                    None => {
                        summary.unique_objects += 1;
                        summary.unique_object_bytes = checked_add(
                            summary.unique_object_bytes,
                            object.size,
                            "unique object bytes",
                        )?;
                    }
                }
            }
            ArchiveEntry::Symlink { .. } => summary.symlinks += 1,
            ArchiveEntry::Unreadable { .. } => summary.unreadable += 1,
            ArchiveEntry::Excluded { .. } => summary.excluded += 1,
        }
    }
    Ok(summary)
}

fn checked_add(total: u64, value: u64, label: &str) -> Result<u64> {
    total
        .checked_add(value)
        .ok_or_else(|| anyhow!("{label} exceed u64"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowserEntryKind {
    Folder,
    File,
    Symlink,
    Unreadable,
    Excluded,
}

impl BrowserEntryKind {
    fn label(self) -> &'static str {
        match self {
            Self::Folder => "folder",
            Self::File => "file",
            Self::Symlink => "symlink",
            Self::Unreadable => "unreadable",
            Self::Excluded => "excluded",
        }
    }

    fn color(self) -> Color {
        match self {
            Self::Folder => ACCENT,
            Self::File => TEXT,
            Self::Symlink => Color::rgba(0xb7, 0x9d, 0xf7, 0xff),
            Self::Unreadable => DANGER,
            Self::Excluded => CAUTION,
        }
    }

    fn sort_rank(self) -> u8 {
        match self {
            Self::Folder => 0,
            Self::File => 1,
            Self::Symlink => 2,
            Self::Unreadable => 3,
            Self::Excluded => 4,
        }
    }
}

#[derive(Debug, Clone)]
struct BrowserItem {
    name: String,
    path: String,
    kind: BrowserEntryKind,
    entries: u64,
    files: u64,
    logical_bytes: u64,
    direct_detail: Option<String>,
}

fn directory_children(manifest: &ArchiveManifest, prefix: Option<&str>) -> Vec<BrowserItem> {
    let mut children = BTreeMap::<String, BrowserItem>::new();
    for entry in &manifest.entries {
        let remainder = match prefix {
            Some(prefix) => entry
                .path()
                .strip_prefix(prefix)
                .and_then(|remainder| remainder.strip_prefix('/')),
            None => Some(entry.path()),
        };
        let Some(remainder) = remainder else {
            continue;
        };
        let Some(name) = remainder.split('/').next() else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let direct = name.len() == remainder.len();
        let path = match prefix {
            Some(prefix) => format!("{prefix}/{name}"),
            None => name.to_string(),
        };
        let kind = if direct {
            entry_kind(entry)
        } else {
            BrowserEntryKind::Folder
        };
        let item = children
            .entry(name.to_string())
            .or_insert_with(|| BrowserItem {
                name: name.to_string(),
                path,
                kind,
                entries: 0,
                files: 0,
                logical_bytes: 0,
                direct_detail: None,
            });
        if !direct {
            item.kind = BrowserEntryKind::Folder;
        }
        item.entries += 1;
        if let ArchiveEntry::File { object, .. } = entry {
            item.files += 1;
            item.logical_bytes = item.logical_bytes.saturating_add(object.size);
        }
        if direct {
            item.kind = kind;
            item.direct_detail = Some(entry_detail(entry));
        }
    }
    let mut children = children.into_values().collect::<Vec<_>>();
    children.sort_by(|left, right| {
        left.kind
            .sort_rank()
            .cmp(&right.kind.sort_rank())
            .then_with(|| compare_names(&left.name, &right.name))
    });
    children
}

fn compare_names(left: &str, right: &str) -> Ordering {
    left.to_lowercase()
        .cmp(&right.to_lowercase())
        .then_with(|| left.cmp(right))
}

fn entry_kind(entry: &ArchiveEntry) -> BrowserEntryKind {
    match entry {
        ArchiveEntry::Directory { .. } => BrowserEntryKind::Folder,
        ArchiveEntry::File { .. } => BrowserEntryKind::File,
        ArchiveEntry::Symlink { .. } => BrowserEntryKind::Symlink,
        ArchiveEntry::Unreadable { .. } => BrowserEntryKind::Unreadable,
        ArchiveEntry::Excluded { .. } => BrowserEntryKind::Excluded,
    }
}

fn entry_detail(entry: &ArchiveEntry) -> String {
    match entry {
        ArchiveEntry::Directory {
            modified_at_unix_secs,
            ..
        } => format!(
            "directory\nmodified: {}",
            format_timestamp(*modified_at_unix_secs)
        ),
        ArchiveEntry::File {
            object,
            modified_at_unix_secs,
            ..
        } => format!(
            "file\nobject: {}\nsize: {}\nmodified: {}",
            object.hash,
            format_bytes(object.size),
            format_timestamp(*modified_at_unix_secs)
        ),
        ArchiveEntry::Symlink { target, .. } => format!("symlink\ntarget: {target}"),
        ArchiveEntry::Unreadable {
            operation, error, ..
        } => format!("unreadable\noperation: {operation}\nerror: {error}"),
        ArchiveEntry::Excluded { reason, .. } => format!("excluded\nreason: {reason}"),
    }
}

fn parent_prefix(prefix: &str) -> Option<String> {
    prefix
        .rsplit_once('/')
        .map(|(parent, _)| parent.to_string())
}

const CHECKBOX_SIZE: f32 = 16.0;
const CHECKBOX_LEFT_INSET: f32 = 6.0;
/// Distance from the row's left edge to where the label text starts --
/// clears the checkbox plus a gap.
const CHECKBOX_LABEL_OFFSET: f32 = CHECKBOX_LEFT_INSET + CHECKBOX_SIZE + 8.0;

fn row_checkbox_rect(list: Rect, row_y: f32) -> Rect {
    Rect {
        x: list.x + CHECKBOX_LEFT_INSET,
        y: row_y + (ROW_HEIGHT - 2.0 - CHECKBOX_SIZE) / 2.0,
        width: CHECKBOX_SIZE,
        height: CHECKBOX_SIZE,
    }
}

/// A removal awaiting explicit confirmation. Nothing is written to disk
/// until the user confirms -- see [`BrowserApp::confirm_removal`].
#[derive(Debug, Clone)]
struct PendingRemoval {
    /// One or more manifest paths (files, symlinks, or folders) to drop.
    /// Recursive expansion to everything nested under a folder happens at
    /// confirm time, inside `ArchiveManifest::with_entries_removed`.
    paths: Vec<String>,
    label: String,
}

/// The Preview action's outcome for one manifest path -- kept until the
/// selection changes (see `BrowserApp::preview`'s doc comment).
#[derive(Debug, Clone)]
struct PreviewState {
    path: String,
    outcome: std::result::Result<PreviewImage, String>,
}

/// A successfully decoded and texture-uploaded preview image.
#[derive(Debug, Clone)]
struct PreviewImage {
    /// The fixed registered-image key every preview is uploaded under (see
    /// `BrowserApp::open_preview`) -- reusing one key means each new
    /// preview replaces the last one's registered texture instead of the
    /// registry growing by one entry per file ever previewed in a session.
    image_key: String,
    width: f32,
    height: f32,
}

#[derive(Debug)]
struct BrowserApp {
    cas_roots: Vec<PathBuf>,
    /// One entry per `cas_roots`, same order, looked up once at launch.
    devices: Vec<(PathBuf, DeviceInfo)>,
    catalog: ArchiveCatalog,
    selected_archive: usize,
    current_prefix: Option<String>,
    selected_path: Option<String>,
    child_scroll: usize,
    archive_scroll: usize,
    message: Option<String>,
    pending_removal: Option<PendingRemoval>,
    /// Manifest paths checked in the explorer, scoped to the currently
    /// selected archive/manifest -- cleared on `select_archive`/`refresh`,
    /// but deliberately preserved across `navigate_to` so checking items in
    /// one folder, navigating to another, and checking more there builds one
    /// batch removal.
    checked: std::collections::BTreeSet<String>,
    /// Updated every frame from `InputSnapshot`, independent of any click.
    /// `paint()` uses it to highlight whatever the pointer is over -- the
    /// browser only ever polled `mouse_pressed` before, so nothing lit up
    /// on hover the way loadngo's own `ui_core::Button` does elsewhere.
    pointer: Point,
    signing: Option<SigningContext>,
    /// The current folder's direct children, valid for exactly the
    /// `(selected_archive, current_prefix)` pair recorded alongside it.
    /// `directory_children` is a full linear scan of the manifest's entries
    /// (up to ~178k on the largest archive seen so far) to find them, so it
    /// must run once per actual navigation, not once per frame -- a fast
    /// pointer-move storm while `FrameDemand::Idle` wakes on every event
    /// would otherwise re-scan the whole manifest repeatedly while nothing
    /// about the view had even changed. See `ensure_children_cache`.
    children_cache: Vec<BrowserItem>,
    children_cache_key: Option<(usize, Option<String>)>,
    /// The Preview action's outcome for whatever file it was last run
    /// against. Cleared on every navigation/selection change (see
    /// `select_archive`, `navigate_to`, and the explorer click handler in
    /// `handle_input`) so a stale image or error can never linger against a
    /// newly selected file.
    preview: Option<PreviewState>,
}

impl BrowserApp {
    fn new(
        cas_roots: Vec<PathBuf>,
        devices: Vec<(PathBuf, DeviceInfo)>,
        catalog: ArchiveCatalog,
        selected_archive: usize,
        signing: Option<SigningContext>,
    ) -> Self {
        Self {
            cas_roots,
            devices,
            catalog,
            selected_archive,
            current_prefix: None,
            selected_path: None,
            child_scroll: 0,
            archive_scroll: 0,
            message: None,
            pending_removal: None,
            checked: std::collections::BTreeSet::new(),
            pointer: Point { x: -1.0, y: -1.0 },
            signing,
            children_cache: Vec::new(),
            children_cache_key: None,
            preview: None,
        }
    }

    /// Recomputes `children_cache` only when `(selected_archive,
    /// current_prefix)` has actually changed since the last call. Must be
    /// called before anything reads `children_cache`, and again after
    /// `handle_input` in case it navigated -- see the call sites in `run`.
    fn ensure_children_cache(&mut self) {
        let key = (self.selected_archive, self.current_prefix.clone());
        if self.children_cache_key.as_ref() != Some(&key) {
            self.children_cache =
                directory_children(&self.selected().manifest, self.current_prefix.as_deref());
            self.children_cache_key = Some(key);
        }
    }

    async fn run(&mut self) {
        loop {
            let frame = loadngo_host_desktop::capture_frame();
            if frame.input.key_pressed(HostKey::Escape) {
                if self.preview.take().is_some() {
                    // Nothing to announce in `self.message` -- unlike a
                    // cancelled removal, closing a preview has no
                    // consequence worth a status line.
                } else if self.pending_removal.take().is_some() {
                    self.message = Some("Removal cancelled.".to_string());
                } else {
                    break;
                }
            }
            self.ensure_children_cache();
            self.handle_input(&frame.input, frame.surface.width, frame.surface.height);
            self.ensure_children_cache();
            let mut scene = Vec::new();
            self.paint(&mut scene, frame.surface.width, frame.surface.height);
            loadngo_host_desktop::clear(BACKGROUND);
            loadngo_host_desktop::render_widget_paint_ops(&scene);
            // Idle, not a 16ms timer: this UI is entirely static between
            // input events -- no animation, no time-based state, hover is a
            // plain color swap, not a pulse. A fixed timer redraws forever
            // even sitting untouched (this browser's own CPU cost was ~89%
            // of a core at idle before this fix). `FrameDemand::Idle` parks
            // until the next real input event bumps the host's event epoch
            // (mouse move, click, key, scroll, resize all count), matching
            // docs/WIDGET_FRAMEWORK.md's own guidance: "static unchanged UI:
            // Idle".
            loadngo_host_desktop::next_frame(FrameDemand::idle()).await;
        }
    }

    fn selected(&self) -> &ArchiveRecord {
        &self.catalog.archives[self.selected_archive]
    }

    fn selected_item(&self) -> Option<BrowserItem> {
        let path = self.selected_path.as_deref()?;
        self.children_cache
            .iter()
            .find(|item| item.path == path)
            .cloned()
    }

    fn select_archive(&mut self, index: usize) {
        if index < self.catalog.archives.len() && index != self.selected_archive {
            self.selected_archive = index;
            self.current_prefix = None;
            self.selected_path = None;
            self.child_scroll = 0;
            self.pending_removal = None;
            self.checked.clear();
            self.preview = None;
        }
    }

    fn navigate_to(&mut self, path: String) {
        self.current_prefix = Some(path);
        self.selected_path = None;
        self.child_scroll = 0;
        self.pending_removal = None;
        self.preview = None;
    }

    fn refresh(&mut self) {
        let selected_manifest = self
            .catalog
            .archives
            .get(self.selected_archive)
            .map(|archive| archive.manifest_path.clone());
        match ArchiveCatalog::read(&self.cas_roots) {
            Ok(catalog) => {
                let selected_archive = selected_manifest
                    .as_deref()
                    .map(|path| catalog.initial_selection(Some(path)))
                    .unwrap_or(0);
                self.catalog = catalog;
                self.selected_archive = selected_archive;
                self.current_prefix = None;
                self.selected_path = None;
                self.child_scroll = 0;
                self.pending_removal = None;
                self.checked.clear();
                self.preview = None;
                self.message = Some("Manifest index reloaded; blobs were not read.".to_string());
            }
            Err(error) => self.message = Some(format!("Refresh failed: {error:#}")),
        }
    }

    /// The paths a removal would act on right now: everything checked, or
    /// failing that, whatever's selected in the inspector. Empty means there
    /// is nothing to remove yet.
    fn removal_candidates(&self) -> Vec<String> {
        if !self.checked.is_empty() {
            self.checked.iter().cloned().collect()
        } else if let Some(item) = self.selected_item() {
            vec![item.path]
        } else {
            Vec::new()
        }
    }

    /// The kind of the manifest entry at exactly this path, if any. Every
    /// folder shown in the explorer corresponds to a real `Directory` entry
    /// at that exact path (ingest always writes one), so this works for
    /// folders and leaves alike.
    fn manifest_entry_kind(&self, path: &str) -> Option<BrowserEntryKind> {
        self.selected()
            .manifest
            .entries
            .iter()
            .find(|entry| entry.path() == path)
            .map(entry_kind)
    }

    /// The archive object backing a regular-file manifest entry at exactly
    /// this path, if any. `None` for a path that is a folder, symlink,
    /// unreadable, or excluded entry -- none of those have a blob to read.
    fn manifest_file_object(&self, path: &str) -> Option<ArchiveObject> {
        self.selected()
            .manifest
            .entries
            .iter()
            .find_map(|entry| match entry {
                ArchiveEntry::File {
                    path: entry_path,
                    object,
                    ..
                } if entry_path == path => Some(*object),
                _ => None,
            })
    }

    /// Whether the Preview action should be offered for the current
    /// selection at all -- a previewable file kind exists for its
    /// extension, and it is actually the regular-file entry that kind of
    /// path implies (never a folder, symlink, or excluded/unreadable
    /// stand-in that happens to share the extension).
    fn preview_available(&self) -> bool {
        self.selected_item().is_some_and(|item| {
            preview::kind_for_path(&item.path).is_some()
                && self.manifest_file_object(&item.path).is_some()
        })
    }

    /// Reads, verifies, and decodes the selected file's blob, then uploads
    /// it as a texture for `paint_preview` to show. Re-running this against
    /// the same path that is already showing (successfully or not) is a
    /// no-op -- the outcome does not change without a different file being
    /// selected, and there is nothing to gain from re-verifying and
    /// re-decoding the same bytes on every click.
    fn open_preview(&mut self) {
        let Some(item) = self.selected_item() else {
            return;
        };
        if self
            .preview
            .as_ref()
            .is_some_and(|state| state.path == item.path)
        {
            return;
        }
        let Some(kind) = preview::kind_for_path(&item.path) else {
            return;
        };
        let outcome = (|| -> std::result::Result<PreviewImage, String> {
            let object = self.manifest_file_object(&item.path).ok_or_else(|| {
                "selected entry is not a regular file in this manifest".to_string()
            })?;
            if object.size > preview::MAX_PREVIEW_SOURCE_BYTES {
                return Err(format!(
                    "file is {} -- larger than the {} preview limit",
                    format_bytes(object.size),
                    format_bytes(preview::MAX_PREVIEW_SOURCE_BYTES)
                ));
            }
            let store = ArchiveCasStorage::new(&self.selected().cas_root)
                .map_err(|error| format!("{error:#}"))?;
            store
                .verify_object(object)
                .map_err(|error| format!("archive object failed verification: {error:#}"))?;
            let bytes = store
                .read_range(object.hash, 0, object.size as usize)
                .map_err(|error| format!("{error:#}"))?;
            let decoded = preview::decode(kind, &bytes).map_err(|error| format!("{error:#}"))?;
            let image_key = "archive_cas_browser_preview".to_string();
            loadngo_host_desktop::upload_texture_with_image_key(Some(&image_key), &decoded)?;
            Ok(PreviewImage {
                image_key,
                width: decoded.width as f32,
                height: decoded.height as f32,
            })
        })();
        self.preview = Some(PreviewState {
            path: item.path,
            outcome,
        });
    }

    fn close_preview(&mut self) {
        self.preview = None;
    }

    /// Requests removal of the current candidates (checked set, or the
    /// single selected item), pending explicit confirmation. Writes nothing
    /// yet. A folder pulls in everything nested under it, at confirm time,
    /// inside `ArchiveManifest::with_entries_removed`.
    fn request_removal(&mut self) {
        let paths = self.removal_candidates();
        if paths.is_empty() {
            return;
        }
        let label = if let [single] = paths.as_slice() {
            match self.manifest_entry_kind(single) {
                Some(BrowserEntryKind::Folder) => {
                    let prefix = format!("{single}/");
                    let nested = self
                        .selected()
                        .manifest
                        .entries
                        .iter()
                        .filter(|entry| entry.path().starts_with(prefix.as_str()))
                        .count();
                    format!("{single} and everything nested under it ({nested} entries)")
                }
                _ => single.clone(),
            }
        } else {
            let (mut files, mut folders, mut other) = (0, 0, 0);
            for path in &paths {
                match self.manifest_entry_kind(path) {
                    Some(BrowserEntryKind::Folder) => folders += 1,
                    Some(BrowserEntryKind::File) => files += 1,
                    _ => other += 1,
                }
            }
            format!(
                "{} checked items ({files} file(s), {folders} folder(s), {other} other)",
                paths.len()
            )
        };
        self.pending_removal = Some(PendingRemoval { paths, label });
    }

    /// Writes the superseding manifest and delete-log sidecar, then reloads
    /// the catalog onto the new manifest. The old manifest, its signature,
    /// and every blob object are left untouched -- re-signing, history
    /// pruning, and blob GC are separate, deliberate steps run from the
    /// command line afterward.
    fn confirm_removal(&mut self) {
        let Some(pending) = self.pending_removal.take() else {
            return;
        };
        let result = (|| -> Result<PathBuf> {
            let store = ArchiveCasStorage::new(&self.selected().cas_root)?;
            let manifest = &self.selected().manifest;
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .context("system clock is before the Unix epoch")?
                .as_secs();
            let actor = std::env::var("USER")
                .or_else(|_| std::env::var("USERNAME"))
                .unwrap_or_else(|_| "unknown".to_string());
            let (amended, log) = manifest.with_entries_removed(
                &pending.paths,
                "Removed via Archive CAS browser (manual review)",
                actor,
                now,
            )?;
            let (manifest_path, _root) = store.write_manifest(&amended)?;
            store.write_delete_log(&manifest_path, &log)?;
            Ok(manifest_path)
        })();
        match result {
            Ok(manifest_path) => match ArchiveCatalog::read(&self.cas_roots) {
                Ok(catalog) => {
                    self.selected_archive = catalog.initial_selection(Some(&manifest_path));
                    self.catalog = catalog;
                    self.current_prefix = None;
                    self.selected_path = None;
                    self.child_scroll = 0;
                    self.checked.clear();
                    self.message = Some(format!(
                        "Removed {}. Wrote {}. Not yet signed -- run archive_cas_sign, then archive_cas_prune_manifests / archive_cas_gc when ready.",
                        pending.label,
                        manifest_path.display()
                    ));
                }
                Err(error) => {
                    self.message = Some(format!(
                        "Removed {} and wrote {}, but reloading the catalog failed: {error:#}",
                        pending.label,
                        manifest_path.display()
                    ));
                }
            },
            Err(error) => {
                self.message = Some(format!("Removal failed: {error:#}"));
            }
        }
    }

    /// `{cas_root}/manifests/{archive_id}-{root}.signature.json` -- same
    /// naming `archive_cas_sign` writes to, computed without needing an
    /// `ArchiveCasStorage` since it's a plain path join.
    fn signature_path(&self, archive: &ArchiveRecord) -> PathBuf {
        archive.cas_root.join("manifests").join(format!(
            "{}-{}.signature.json",
            archive.manifest.archive_id, archive.summary.root
        ))
    }

    fn selected_is_signed(&self) -> bool {
        self.signature_path(self.selected()).exists()
    }

    /// Signs the currently selected manifest and writes the signature file,
    /// using the key material given at launch. Requires `self.signing` --
    /// callers check that before offering this action at all.
    fn sign_selected_manifest(&mut self) {
        let Some(signing) = self.signing.as_ref() else {
            return;
        };
        let manifest = self.selected().manifest.clone();
        let result = (|| -> Result<PathBuf> {
            let store = ArchiveCasStorage::new(&self.selected().cas_root)?;
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .context("system clock is before the Unix epoch")?
                .as_secs();
            let (out, _signed) = data::archive_cas_sign::sign_manifest_and_write(
                &store,
                &manifest,
                &signing.signer_identity,
                &signing.public_key,
                &signing.private_key,
                now,
            )?;
            Ok(out)
        })();
        match result {
            Ok(path) => {
                self.message = Some(format!(
                    "Signed by {}. Wrote {}.",
                    signing.signer_identity,
                    path.display()
                ));
            }
            Err(error) => {
                self.message = Some(format!("Signing failed: {error:#}"));
            }
        }
    }

    fn handle_input(&mut self, input: &InputSnapshot, width: f32, height: f32) {
        self.pointer = Point {
            x: input.mouse_x,
            y: input.mouse_y,
        };
        if self.preview.is_some() {
            let layout = AppLayout::new(width, height);
            if input.mouse_pressed && layout.up_button.contains(self.pointer) {
                self.close_preview();
            }
            // Escape is handled in `run()`. Every other input is ignored
            // while a preview is showing, same as a pending removal --
            // repurposing `up_button`'s corner as the close hit target (it
            // has no meaning of its own while the explorer panel is
            // replaced by the preview) rather than adding a second
            // always-reserved layout region just for this.
            return;
        }
        if self.pending_removal.is_some() {
            if input.key_pressed(HostKey::Enter) || input.key_pressed(HostKey::Y) {
                self.confirm_removal();
                return;
            }
            let layout = AppLayout::new(width, height);
            if input.mouse_pressed {
                let pointer = self.pointer;
                if layout.action_confirm.contains(pointer) {
                    self.confirm_removal();
                } else if layout.action_cancel.contains(pointer) {
                    self.pending_removal = None;
                    self.message = Some("Removal cancelled.".to_string());
                }
            }
            // Escape is handled in `run()` (it otherwise closes the window).
            // Every other input is ignored while a removal is pending, so a
            // stray click can't change the selection out from under it.
            return;
        }
        if input.key_pressed(HostKey::R) {
            self.refresh();
            return;
        }
        if input.key_pressed(HostKey::S) && self.signing.is_some() && !self.selected_is_signed() {
            self.sign_selected_manifest();
            return;
        }
        if input.key_pressed(HostKey::V) && self.preview_available() {
            self.open_preview();
            return;
        }
        // Shift+Backspace, not plain Delete/Backspace: a Mac keyboard's key
        // labeled "Delete" sends Backspace (forward-delete needs Fn+Delete,
        // which most Mac laptops lack entirely), and Backspace alone already
        // means "navigate up" here. The shift chord keeps both bindings
        // reachable without collision; HostKey::Delete is kept below as a
        // fallback for real forward-delete keys where one exists.
        if (input.key_pressed(HostKey::Backspace) && input.modifiers.shift)
            || input.key_pressed(HostKey::Delete)
        {
            self.request_removal();
            return;
        }
        if input.key_pressed(HostKey::Home) {
            self.current_prefix = None;
            self.selected_path = None;
            self.child_scroll = 0;
        }
        if input.key_pressed(HostKey::Backspace) {
            if let Some(prefix) = self.current_prefix.as_deref() {
                self.current_prefix = parent_prefix(prefix);
                self.selected_path = None;
                self.child_scroll = 0;
            }
        }

        let layout = AppLayout::new(width, height);
        // Re-check, don't blindly trust the cache `run()` primed before this
        // call: Home/Backspace above may have just changed `current_prefix`
        // within this same call, and the click hit-testing below needs the
        // list for *that* folder, not the one before it. A clone, not a
        // borrow, because this function goes on to call other `&mut self`
        // methods (`select_archive`, `navigate_to`, ...) below.
        self.ensure_children_cache();
        let children = self.children_cache.clone();
        let visible_rows = layout.explorer_visible_rows();
        self.clamp_child_scroll(children.len(), visible_rows);
        self.clamp_archive_scroll();
        let pointer = self.pointer;

        if layout.explorer_list.contains(pointer) && input.mouse_wheel_y != 0.0 {
            let step = if input.mouse_wheel_precise {
                (input.mouse_wheel_y.abs() / ROW_HEIGHT).ceil().max(1.0) as usize
            } else {
                input.mouse_wheel_y.abs().ceil().max(1.0) as usize
            };
            if input.mouse_wheel_y < 0.0 {
                self.child_scroll = self.child_scroll.saturating_add(step);
            } else {
                self.child_scroll = self.child_scroll.saturating_sub(step);
            }
            self.clamp_child_scroll(children.len(), visible_rows);
        }
        if layout.archive_list.contains(pointer) && input.mouse_wheel_y != 0.0 {
            let step = if input.mouse_wheel_precise {
                (input.mouse_wheel_y.abs() / ROW_HEIGHT).ceil().max(1.0) as usize
            } else {
                input.mouse_wheel_y.abs().ceil().max(1.0) as usize
            };
            if input.mouse_wheel_y < 0.0 {
                self.archive_scroll = self.archive_scroll.saturating_add(step);
            } else {
                self.archive_scroll = self.archive_scroll.saturating_sub(step);
            }
            self.clamp_archive_scroll();
        }

        if !input.mouse_pressed {
            return;
        }
        if layout.root_button.contains(pointer) {
            self.current_prefix = None;
            self.selected_path = None;
            self.child_scroll = 0;
            return;
        }
        if layout.up_button.contains(pointer) {
            if let Some(prefix) = self.current_prefix.as_deref() {
                self.current_prefix = parent_prefix(prefix);
                self.selected_path = None;
                self.child_scroll = 0;
            }
            return;
        }
        if layout.archive_list.contains(pointer) {
            // Device banners aren't clickable -- only archive rows are, and
            // this walks the exact same row layout `paint_archives` draws,
            // so a click can never land on a row that isn't what it looks
            // like it landed on.
            if let Some((_, ArchiveListRow::Archive(index))) = self
                .visible_archive_rows(&layout)
                .into_iter()
                .find(|(rect, _)| rect.contains(pointer))
            {
                self.select_archive(index);
            }
            return;
        }
        if layout.breadcrumb.contains(pointer) && !self.checked.is_empty() {
            self.checked.clear();
            self.message = Some("Cleared the checked selection.".to_string());
            return;
        }
        if layout.action_button.contains(pointer) {
            self.request_removal();
            return;
        }
        if layout.sign_button.contains(pointer)
            && self.signing.is_some()
            && !self.selected_is_signed()
        {
            self.sign_selected_manifest();
            return;
        }
        if layout.preview_button.contains(pointer) && self.preview_available() {
            self.open_preview();
            return;
        }
        if layout.explorer_list.contains(pointer) {
            let visible_index =
                ((pointer.y - layout.explorer_list.y) / ROW_HEIGHT).floor() as usize;
            let index = self.child_scroll.saturating_add(visible_index);
            if let Some(item) = children.get(index) {
                let row_y = layout.explorer_list.y + visible_index as f32 * ROW_HEIGHT;
                if row_checkbox_rect(layout.explorer_list, row_y).contains(pointer) {
                    // The checkbox is the only way to mark a folder without
                    // opening it, and the only way to build a multi-item
                    // removal -- a plain row click keeps its own meaning
                    // (open a folder / inspect a file) either way.
                    if !self.checked.remove(&item.path) {
                        self.checked.insert(item.path.clone());
                    }
                } else if item.kind == BrowserEntryKind::Folder {
                    self.navigate_to(item.path.clone());
                } else {
                    self.selected_path = Some(item.path.clone());
                    self.preview = None;
                }
            }
        }
    }

    fn clamp_child_scroll(&mut self, child_count: usize, visible_rows: usize) {
        self.child_scroll = self
            .child_scroll
            .min(child_count.saturating_sub(visible_rows.max(1)));
    }

    /// Every row the archive list would show with no scrolling applied: a
    /// device banner ahead of the first archive from each `cas_root`
    /// (`self.catalog.archives` is already grouped this way), then that
    /// root's archives.
    fn archive_list_rows(&self) -> Vec<ArchiveListRow> {
        let mut rows = Vec::new();
        let mut last_root: Option<&PathBuf> = None;
        for (index, archive) in self.catalog.archives.iter().enumerate() {
            if last_root != Some(&archive.cas_root) {
                rows.push(ArchiveListRow::Device(archive.cas_root.clone()));
                last_root = Some(&archive.cas_root);
            }
            rows.push(ArchiveListRow::Archive(index));
        }
        rows
    }

    /// The rows that actually fit in `layout.archive_list` after applying
    /// `self.archive_scroll`, each with its painted rect -- the single
    /// source of truth both `paint_archives` and its click handling walk,
    /// so hit-testing can never drift from what's drawn.
    fn visible_archive_rows(&self, layout: &AppLayout) -> Vec<(Rect, ArchiveListRow)> {
        let mut result = Vec::new();
        let mut y = layout.archive_list.y;
        for row in self
            .archive_list_rows()
            .into_iter()
            .skip(self.archive_scroll)
        {
            if y >= layout.archive_list.bottom() {
                break;
            }
            let height = match row {
                ArchiveListRow::Device(_) => DEVICE_ROW_HEIGHT,
                ArchiveListRow::Archive(_) => ROW_HEIGHT,
            };
            let rect = Rect {
                x: layout.archive_list.x,
                y,
                width: layout.archive_list.width,
                height: (height - 2.0).min((layout.archive_list.bottom() - y).max(0.0)),
            };
            result.push((rect, row));
            y += height;
        }
        result
    }

    fn clamp_archive_scroll(&mut self) {
        let total = self.archive_list_rows().len();
        self.archive_scroll = self.archive_scroll.min(total.saturating_sub(1));
    }

    fn device_info(&self, cas_root: &Path) -> Option<&DeviceInfo> {
        self.devices
            .iter()
            .find(|(root, _)| root == cas_root)
            .map(|(_, info)| info)
    }

    /// The record whose `supersedes_archive_root` names `record`'s own
    /// root, if `record.is_superseded`. `None` for a current version, even
    /// if the search hasn't run -- callers only care about this when
    /// `is_superseded` is already true.
    fn successor_of(&self, record: &ArchiveRecord) -> Option<&ArchiveRecord> {
        if !record.is_superseded {
            return None;
        }
        self.catalog.archives.iter().find(|candidate| {
            candidate
                .manifest
                .supersedes_archive_root
                .is_some_and(|root| root.to_hex() == record.summary.root)
        })
    }

    fn paint(&self, scene: &mut Vec<ui_core::PaintOp>, width: f32, height: f32) {
        let layout = AppLayout::new(width, height);
        let record = self.selected();
        // `run()` calls `ensure_children_cache()` right before `paint`, so
        // this is always fresh for the state actually being painted.
        let children = self.children_cache.as_slice();
        let visible_rows = layout.explorer_visible_rows();
        let scroll = self
            .child_scroll
            .min(children.len().saturating_sub(visible_rows.max(1)));

        paint_panel(scene, layout.header, Color::rgba(0x16, 0x25, 0x35, 0xff));
        paint_text(
            scene,
            "Archive CAS browser",
            Rect {
                x: layout.header.x + PANEL_INSET,
                y: layout.header.y + 8.0,
                width: layout.header.width * 0.42,
                height: 32.0,
            },
            TITLE_FONT,
            TEXT,
            HorizontalAlign::Left,
        );
        paint_text(
            scene,
            "Manifest-level editing — checkbox a row to mark it, click a folder to open it",
            Rect {
                x: layout.header.x + PANEL_INSET,
                y: layout.header.y + 42.0,
                width: layout.header.width * 0.68,
                height: 22.0,
            },
            BODY_FONT,
            MUTED,
            HorizontalAlign::Left,
        );
        let hotkeys = if self.signing.is_some() {
            "R refresh   Home root   Backspace up   Shift+Backspace remove   S sign   V preview   Esc close/cancel"
        } else {
            "R refresh   Home root   Backspace up   Shift+Backspace remove   V preview   Esc close/cancel"
        };
        paint_text(
            scene,
            hotkeys,
            Rect {
                x: layout.header.x + layout.header.width * 0.50,
                y: layout.header.y + 25.0,
                width: layout.header.width * 0.48 - PANEL_INSET,
                height: 24.0,
            },
            CAPTION_FONT,
            ACCENT,
            HorizontalAlign::Right,
        );

        self.paint_archives(scene, &layout);
        match &self.preview {
            Some(preview) => self.paint_preview_pane(scene, &layout, preview),
            None => self.paint_explorer(scene, &layout, children, scroll, visible_rows),
        }
        self.paint_inspector(scene, &layout, record, children);

        if let Some(message) = self.message.as_deref() {
            // Multi-line, not the single-line ellipsis paint_text uses
            // elsewhere: a removal/sign result routinely runs to two or
            // three sentences (paths, next-step commands), and a status
            // line that silently truncates the instruction it just gave you
            // is worse than useless.
            paint_multiline(
                scene,
                message,
                Rect {
                    x: layout.header.x + PANEL_INSET,
                    y: layout.header.bottom() - 58.0,
                    width: layout.header.width - PANEL_INSET * 2.0,
                    height: 54.0,
                },
                CAPTION_FONT,
                CAUTION,
            );
        }
    }

    fn paint_archives(&self, scene: &mut Vec<ui_core::PaintOp>, layout: &AppLayout) {
        paint_panel(scene, layout.archives, PANEL_BACKGROUND);
        paint_text(
            scene,
            "Archive manifests",
            layout.archives_title,
            SECTION_FONT,
            TEXT,
            HorizontalAlign::Left,
        );
        paint_text(
            scene,
            &format!("{} indexed", self.catalog.archives.len()),
            Rect {
                x: layout.archives_title.x,
                y: layout.archives_title.y + 21.0,
                width: layout.archives_title.width,
                height: 16.0,
            },
            CAPTION_FONT,
            MUTED,
            HorizontalAlign::Left,
        );

        for (row, item) in self.visible_archive_rows(layout) {
            match item {
                ArchiveListRow::Device(cas_root) => self.paint_device_row(scene, row, &cas_root),
                ArchiveListRow::Archive(index) => {
                    self.paint_archive_row(scene, row, index);
                }
            }
        }

        if !self.catalog.warnings.is_empty() {
            let warning = format!(
                "{} noncanonical or unreadable manifest{} ignored",
                self.catalog.warnings.len(),
                if self.catalog.warnings.len() == 1 {
                    ""
                } else {
                    "s"
                }
            );
            paint_text(
                scene,
                &warning,
                Rect {
                    x: layout.archives.x + PANEL_INSET,
                    y: layout.archives.bottom() - 25.0,
                    width: layout.archives.width - PANEL_INSET * 2.0,
                    height: 18.0,
                },
                CAPTION_FONT,
                CAUTION,
                HorizontalAlign::Left,
            );
        }
    }

    fn paint_device_row(&self, scene: &mut Vec<ui_core::PaintOp>, row: Rect, cas_root: &Path) {
        scene.push(ui_core::PaintOp::FillRect {
            rect: row,
            color: Color::rgba(0x11, 0x18, 0x24, 0xff),
        });
        let device = self.device_info(cas_root);
        let label = device
            .filter(|info| !info.label.is_empty())
            .map(|info| info.label.as_str())
            .unwrap_or("device unknown");
        paint_text(
            scene,
            label,
            Rect {
                x: row.x + 9.0,
                y: row.y + 3.0,
                width: row.width - 18.0,
                height: 16.0,
            },
            BODY_FONT,
            TEXT,
            HorizontalAlign::Left,
        );
        let mount_point = device.map(|info| info.mount_point.as_str()).unwrap_or("");
        paint_text(
            scene,
            mount_point,
            Rect {
                x: row.x + 9.0,
                y: row.y + 20.0,
                width: row.width - 18.0,
                height: 14.0,
            },
            CAPTION_FONT,
            ACCENT,
            HorizontalAlign::Left,
        );
        let space = match device.and_then(|info| Some((info.free_bytes?, info.total_bytes?))) {
            Some((free, total)) => {
                format!("{} free of {}", format_bytes(free), format_bytes(total))
            }
            None => String::new(),
        };
        paint_text(
            scene,
            &space,
            Rect {
                x: row.x + 9.0,
                y: row.y + 35.0,
                width: row.width - 18.0,
                height: 14.0,
            },
            CAPTION_FONT,
            MUTED,
            HorizontalAlign::Left,
        );
    }

    fn paint_archive_row(&self, scene: &mut Vec<ui_core::PaintOp>, row: Rect, index: usize) {
        let archive = &self.catalog.archives[index];
        let selected = index == self.selected_archive;
        if selected {
            scene.push(ui_core::PaintOp::FillRect {
                rect: row,
                color: SELECTED,
            });
        } else if row.contains(self.pointer) {
            scene.push(ui_core::PaintOp::StrokeRect {
                rect: row,
                color: ROW_HOVER_BORDER,
            });
        }
        paint_text(
            scene,
            &archive.manifest.archive_id,
            Rect {
                x: row.x + 18.0,
                y: row.y + 3.0,
                width: row.width - 27.0,
                height: 17.0,
            },
            BODY_FONT,
            if selected { TEXT } else { ACCENT },
            HorizontalAlign::Left,
        );
        // "Capture complete" is true of every version in a history chain
        // alike -- it says nothing about which one is current. Lead with
        // that instead, since two versions of the same archive otherwise
        // look identical in this list.
        let (version_label, version_color) = if archive.is_superseded {
            ("Superseded", MUTED)
        } else if archive.manifest.supersedes_archive_root.is_some() {
            ("Current (edited)", COMPLETE)
        } else {
            ("Current", COMPLETE)
        };
        paint_text(
            scene,
            &format!(
                "{version_label} • {} files",
                format_number(archive.summary.files)
            ),
            Rect {
                x: row.x + 18.0,
                y: row.y + 20.0,
                width: row.width - 27.0,
                height: 15.0,
            },
            CAPTION_FONT,
            version_color,
            HorizontalAlign::Left,
        );
    }

    fn paint_explorer(
        &self,
        scene: &mut Vec<ui_core::PaintOp>,
        layout: &AppLayout,
        children: &[BrowserItem],
        scroll: usize,
        visible_rows: usize,
    ) {
        paint_panel(scene, layout.explorer, PANEL_BACKGROUND);
        paint_text(
            scene,
            "Path explorer",
            layout.explorer_title,
            SECTION_FONT,
            TEXT,
            HorizontalAlign::Left,
        );
        paint_button(
            scene,
            layout.root_button,
            "Root",
            self.current_prefix.is_none(),
            layout.root_button.contains(self.pointer),
        );
        paint_button(
            scene,
            layout.up_button,
            "Up",
            self.current_prefix.is_some(),
            layout.up_button.contains(self.pointer),
        );
        let breadcrumb = self.current_prefix.as_deref().unwrap_or("/");
        let breadcrumb_text = if self.checked.is_empty() {
            breadcrumb.to_string()
        } else {
            format!(
                "{breadcrumb}   •   {} checked (click here to clear)",
                self.checked.len()
            )
        };
        paint_text(
            scene,
            &breadcrumb_text,
            layout.breadcrumb,
            CAPTION_FONT,
            if self.checked.is_empty() {
                ACCENT
            } else {
                CAUTION
            },
            HorizontalAlign::Left,
        );

        for (visible_index, item) in children.iter().skip(scroll).take(visible_rows).enumerate() {
            let row = Rect {
                x: layout.explorer_list.x,
                y: layout.explorer_list.y + visible_index as f32 * ROW_HEIGHT,
                width: layout.explorer_list.width,
                height: ROW_HEIGHT - 2.0,
            };
            let selected = self.selected_path.as_deref() == Some(item.path.as_str());
            if selected {
                scene.push(ui_core::PaintOp::FillRect {
                    rect: row,
                    color: SELECTED,
                });
            } else if row.contains(self.pointer) {
                scene.push(ui_core::PaintOp::StrokeRect {
                    rect: row,
                    color: ROW_HOVER_BORDER,
                });
            }
            let checkbox = row_checkbox_rect(layout.explorer_list, row.y);
            let checked = self.checked.contains(&item.path);
            scene.push(ui_core::PaintOp::FillRect {
                rect: checkbox,
                color: if checked {
                    ACCENT
                } else {
                    Color::rgba(0x22, 0x2c, 0x3d, 0xff)
                },
            });
            scene.push(ui_core::PaintOp::StrokeRect {
                rect: checkbox,
                color: if checkbox.contains(self.pointer) {
                    HOVER_BORDER
                } else {
                    PANEL_BORDER
                },
            });
            if checked {
                paint_text(
                    scene,
                    "x",
                    checkbox,
                    BODY_FONT,
                    Color::rgba(0x0d, 0x12, 0x1b, 0xff),
                    HorizontalAlign::Center,
                );
            }
            paint_text(
                scene,
                &format!("{}  {}", item.kind.label(), item.name),
                Rect {
                    x: row.x + CHECKBOX_LABEL_OFFSET,
                    y: row.y + 3.0,
                    width: row.width * 0.62 - CHECKBOX_LABEL_OFFSET,
                    height: 18.0,
                },
                BODY_FONT,
                item.kind.color(),
                HorizontalAlign::Left,
            );
            let detail = if item.kind == BrowserEntryKind::Folder {
                format!(
                    "{} entries • {}",
                    format_number(item.entries),
                    format_bytes(item.logical_bytes)
                )
            } else if item.kind == BrowserEntryKind::File {
                format_bytes(item.logical_bytes)
            } else {
                item.kind.label().to_string()
            };
            paint_text(
                scene,
                &detail,
                Rect {
                    x: row.x + row.width * 0.62,
                    y: row.y + 4.0,
                    width: row.width * 0.36 - 8.0,
                    height: 16.0,
                },
                CAPTION_FONT,
                MUTED,
                HorizontalAlign::Right,
            );
            paint_text(
                scene,
                &item.path,
                Rect {
                    x: row.x + CHECKBOX_LABEL_OFFSET,
                    y: row.y + 20.0,
                    width: row.width - CHECKBOX_LABEL_OFFSET - 8.0,
                    height: 14.0,
                },
                CAPTION_FONT,
                MUTED,
                HorizontalAlign::Left,
            );
        }
        if children.is_empty() {
            paint_text(
                scene,
                "This folder has no manifest entries.",
                layout.explorer_list,
                BODY_FONT,
                MUTED,
                HorizontalAlign::Center,
            );
        } else if children.len() > visible_rows {
            paint_text(
                scene,
                &format!(
                    "{}–{} of {}",
                    scroll + 1,
                    (scroll + visible_rows).min(children.len()),
                    children.len()
                ),
                Rect {
                    x: layout.explorer_list.x,
                    y: layout.explorer_list.bottom() - 18.0,
                    width: layout.explorer_list.width - 8.0,
                    height: 16.0,
                },
                CAPTION_FONT,
                MUTED,
                HorizontalAlign::Right,
            );
        }
    }

    /// Replaces the path explorer's panel content with the Preview action's
    /// result for `preview`, while the archives and inspector panels either
    /// side stay exactly as they always are -- previewing one file never
    /// loses your place in the archive list or the selection metadata.
    fn paint_preview_pane(
        &self,
        scene: &mut Vec<ui_core::PaintOp>,
        layout: &AppLayout,
        preview: &PreviewState,
    ) {
        paint_panel(scene, layout.explorer, PANEL_BACKGROUND);
        paint_text(
            scene,
            "Preview",
            layout.explorer_title,
            SECTION_FONT,
            TEXT,
            HorizontalAlign::Left,
        );
        // Reuses `up_button`'s rect as the close hit target (see
        // `handle_input`) -- it has no "navigate up a folder" meaning of
        // its own while this pane has replaced the explorer.
        paint_button(
            scene,
            layout.up_button,
            "Close",
            true,
            layout.up_button.contains(self.pointer),
        );
        paint_text(
            scene,
            &preview.path,
            layout.breadcrumb,
            CAPTION_FONT,
            MUTED,
            HorizontalAlign::Left,
        );
        match &preview.outcome {
            Ok(image) => {
                let fit = fit_within(image.width, image.height, layout.explorer_list);
                scene.push(ui_core::PaintOp::BlitImage {
                    rect: fit,
                    image_key: image.image_key.clone(),
                });
            }
            Err(error) => {
                paint_multiline(
                    scene,
                    &format!("Couldn't preview this file:\n{error}"),
                    layout.explorer_list,
                    BODY_FONT,
                    DANGER,
                );
            }
        }
    }

    fn paint_inspector(
        &self,
        scene: &mut Vec<ui_core::PaintOp>,
        layout: &AppLayout,
        record: &ArchiveRecord,
        children: &[BrowserItem],
    ) {
        paint_panel(scene, layout.inspector, PANEL_BACKGROUND);
        let summary = &record.summary;
        paint_text(
            scene,
            "Archive visualizer",
            layout.inspector_title,
            SECTION_FONT,
            TEXT,
            HorizontalAlign::Left,
        );
        let (status_label, status_color) = archive_status(summary);
        paint_text(
            scene,
            status_label,
            Rect {
                x: layout.inspector_title.x,
                y: layout.inspector_title.y + 22.0,
                width: layout.inspector_title.width,
                height: 16.0,
            },
            CAPTION_FONT,
            status_color,
            HorizontalAlign::Left,
        );

        let mut y = layout.inspector_body.y;
        let text_width = layout.inspector_body.width;
        paint_key_value(
            scene,
            layout.inspector_body.x,
            "Source",
            &record.manifest.source_label,
            y,
            text_width,
            TEXT,
        );
        y += 35.0;
        paint_key_value(
            scene,
            layout.inspector_body.x,
            "Created",
            &format_timestamp(Some(record.manifest.created_at_unix_secs)),
            y,
            text_width,
            MUTED,
        );
        y += 35.0;
        paint_key_value(
            scene,
            layout.inspector_body.x,
            "Manifest root",
            &summary.root,
            y,
            text_width,
            ACCENT,
        );
        y += 35.0;
        let (version_value, version_color) = if let Some(successor) = self.successor_of(record) {
            (
                format!(
                    "Superseded by {}",
                    &successor.summary.root[..successor.summary.root.len().min(12)]
                ),
                CAUTION,
            )
        } else if record.manifest.supersedes_archive_root.is_some() {
            ("Current (edits an earlier capture)".to_string(), COMPLETE)
        } else {
            ("Current".to_string(), COMPLETE)
        };
        paint_key_value(
            scene,
            layout.inspector_body.x,
            "Version",
            &version_value,
            y,
            text_width,
            version_color,
        );
        y += 48.0;

        paint_text(
            scene,
            "Coverage",
            Rect {
                x: layout.inspector_body.x,
                y,
                width: text_width,
                height: 20.0,
            },
            BODY_FONT,
            TEXT,
            HorizontalAlign::Left,
        );
        y += 24.0;
        let max_entries = summary
            .directories
            .max(summary.files)
            .max(summary.symlinks)
            .max(summary.unreadable)
            .max(summary.excluded)
            .max(1);
        paint_bar(
            scene,
            Rect {
                x: layout.inspector_body.x,
                y,
                width: text_width,
                height: 16.0,
            },
            "Files",
            summary.files,
            max_entries,
            ACCENT,
            false,
        );
        y += 25.0;
        paint_bar(
            scene,
            Rect {
                x: layout.inspector_body.x,
                y,
                width: text_width,
                height: 16.0,
            },
            "Folders",
            summary.directories,
            max_entries,
            Color::rgba(0x94, 0xb8, 0xff, 0xff),
            false,
        );
        y += 25.0;
        paint_bar(
            scene,
            Rect {
                x: layout.inspector_body.x,
                y,
                width: text_width,
                height: 16.0,
            },
            "Links",
            summary.symlinks,
            max_entries,
            Color::rgba(0xbb, 0x9c, 0xfb, 0xff),
            false,
        );
        y += 25.0;
        paint_bar(
            scene,
            Rect {
                x: layout.inspector_body.x,
                y,
                width: text_width,
                height: 16.0,
            },
            "Unreadable",
            summary.unreadable,
            max_entries,
            DANGER,
            false,
        );
        y += 25.0;
        paint_bar(
            scene,
            Rect {
                x: layout.inspector_body.x,
                y,
                width: text_width,
                height: 16.0,
            },
            "Excluded",
            summary.excluded,
            max_entries,
            CAUTION,
            false,
        );
        y += 39.0;

        paint_text(
            scene,
            "Object footprint",
            Rect {
                x: layout.inspector_body.x,
                y,
                width: text_width,
                height: 20.0,
            },
            BODY_FONT,
            TEXT,
            HorizontalAlign::Left,
        );
        y += 24.0;
        let max_bytes = summary
            .logical_bytes
            .max(summary.unique_object_bytes)
            .max(1);
        paint_bar(
            scene,
            Rect {
                x: layout.inspector_body.x,
                y,
                width: text_width,
                height: 16.0,
            },
            "Logical",
            summary.logical_bytes,
            max_bytes,
            ACCENT,
            true,
        );
        y += 25.0;
        paint_bar(
            scene,
            Rect {
                x: layout.inspector_body.x,
                y,
                width: text_width,
                height: 16.0,
            },
            "Unique",
            summary.unique_object_bytes,
            max_bytes,
            COMPLETE,
            true,
        );
        y += 27.0;
        paint_text(
            scene,
            &format!(
                "{} unique blobs • {} reused within this manifest",
                format_number(summary.unique_objects),
                format_bytes(
                    summary
                        .logical_bytes
                        .saturating_sub(summary.unique_object_bytes)
                )
            ),
            Rect {
                x: layout.inspector_body.x,
                y,
                width: text_width,
                height: 17.0,
            },
            CAPTION_FONT,
            MUTED,
            HorizontalAlign::Left,
        );
        y += 35.0;

        paint_text(
            scene,
            "Selection",
            Rect {
                x: layout.inspector_body.x,
                y,
                width: text_width,
                height: 20.0,
            },
            BODY_FONT,
            TEXT,
            HorizontalAlign::Left,
        );
        y += 24.0;
        let selection = self
            .selected_path
            .as_deref()
            .and_then(|path| children.iter().find(|item| item.path == path))
            .map(|item| {
                let detail = item.direct_detail.as_deref().unwrap_or("folder");
                format!("{}\n{}", item.path, detail)
            })
            .unwrap_or_else(|| "Choose a file, link, issue, or exclusion in the path explorer.\nFolders open in place.".to_string());
        paint_multiline(
            scene,
            &selection,
            Rect {
                x: layout.inspector_body.x,
                y,
                width: text_width,
                height: (layout.inspector_body.bottom() - y).max(0.0),
            },
            CAPTION_FONT,
            MUTED,
        );

        self.paint_actions(scene, layout);
    }

    fn paint_actions(&self, scene: &mut Vec<ui_core::PaintOp>, layout: &AppLayout) {
        if let Some(pending) = &self.pending_removal {
            paint_text(
                scene,
                &format!("Remove {}? This writes a new manifest.", pending.label),
                Rect {
                    x: layout.action_button.x,
                    y: layout.action_button.y - 20.0,
                    width: layout.action_button.width,
                    height: 18.0,
                },
                CAPTION_FONT,
                DANGER,
                HorizontalAlign::Left,
            );
            paint_button(
                scene,
                layout.action_confirm,
                "Confirm (Enter)",
                true,
                layout.action_confirm.contains(self.pointer),
            );
            paint_button(
                scene,
                layout.action_cancel,
                "Cancel (Esc)",
                true,
                layout.action_cancel.contains(self.pointer),
            );
            return;
        }
        let candidates = self.removal_candidates();
        let label = if self.checked.is_empty() {
            "Remove selected (Shift+Backspace)".to_string()
        } else {
            format!("Remove {} checked (Shift+Backspace)", self.checked.len())
        };
        paint_button(
            scene,
            layout.action_button,
            &label,
            !candidates.is_empty(),
            layout.action_button.contains(self.pointer),
        );

        if self.signing.is_some() {
            let signed = self.selected_is_signed();
            paint_button(
                scene,
                layout.sign_button,
                if signed {
                    "Manifest already signed"
                } else {
                    "Sign this manifest (S)"
                },
                !signed,
                !signed && layout.sign_button.contains(self.pointer),
            );
        }

        if self.preview_available() {
            let already_open = self
                .preview
                .as_ref()
                .zip(self.selected_item())
                .is_some_and(|(state, item)| state.path == item.path);
            paint_button(
                scene,
                layout.preview_button,
                if already_open {
                    "Previewing (V)"
                } else {
                    "Preview (V)"
                },
                !already_open,
                !already_open && layout.preview_button.contains(self.pointer),
            );
        }
    }
}

fn archive_status(summary: &ArchiveSummary) -> (&'static str, Color) {
    if summary.unreadable > 0 {
        ("Incomplete: unreadable source entries", DANGER)
    } else if summary.excluded > 0 {
        ("Complete within declared scope", CAUTION)
    } else {
        ("Capture complete", COMPLETE)
    }
}

#[derive(Debug, Clone, Copy)]
struct AppLayout {
    header: Rect,
    archives: Rect,
    explorer: Rect,
    inspector: Rect,
    archives_title: Rect,
    archive_list: Rect,
    explorer_title: Rect,
    root_button: Rect,
    up_button: Rect,
    breadcrumb: Rect,
    explorer_list: Rect,
    inspector_title: Rect,
    inspector_body: Rect,
    action_button: Rect,
    action_confirm: Rect,
    action_cancel: Rect,
    sign_button: Rect,
    preview_button: Rect,
}

impl AppLayout {
    fn new(width: f32, height: f32) -> Self {
        let usable_width = (width - OUTER_GUTTER * 2.0).max(720.0);
        let usable_height = (height - OUTER_GUTTER * 2.0).max(540.0);
        let header = Rect {
            x: OUTER_GUTTER,
            y: OUTER_GUTTER,
            width: usable_width,
            height: HEADER_HEIGHT,
        };
        let content_y = header.bottom() + PANEL_GAP;
        let content_height = (usable_height - HEADER_HEIGHT - PANEL_GAP).max(420.0);
        let archive_width = (usable_width * 0.235).clamp(220.0, 340.0);
        let inspector_width = (usable_width * 0.285).clamp(290.0, 400.0);
        let explorer_width =
            (usable_width - archive_width - inspector_width - PANEL_GAP * 2.0).max(260.0);
        let archives = Rect {
            x: OUTER_GUTTER,
            y: content_y,
            width: archive_width,
            height: content_height,
        };
        let explorer = Rect {
            x: archives.right() + PANEL_GAP,
            y: content_y,
            width: explorer_width,
            height: content_height,
        };
        let inspector = Rect {
            x: explorer.right() + PANEL_GAP,
            y: content_y,
            width: inspector_width,
            height: content_height,
        };
        let archives_title = Rect {
            x: archives.x + PANEL_INSET,
            y: archives.y + PANEL_INSET,
            width: archives.width - PANEL_INSET * 2.0,
            height: 22.0,
        };
        let archive_list = Rect {
            x: archives.x + 6.0,
            y: archives.y + 62.0,
            width: archives.width - 12.0,
            height: (archives.height - 94.0).max(0.0),
        };
        let explorer_title = Rect {
            x: explorer.x + PANEL_INSET,
            y: explorer.y + PANEL_INSET,
            width: explorer.width * 0.43,
            height: 22.0,
        };
        let root_button = Rect {
            x: explorer.right() - 116.0,
            y: explorer.y + 11.0,
            width: 50.0,
            height: 26.0,
        };
        let up_button = Rect {
            x: explorer.right() - 59.0,
            y: explorer.y + 11.0,
            width: 42.0,
            height: 26.0,
        };
        let breadcrumb = Rect {
            x: explorer.x + PANEL_INSET,
            y: explorer.y + 42.0,
            width: explorer.width - PANEL_INSET * 2.0,
            height: 18.0,
        };
        let explorer_list = Rect {
            x: explorer.x + 6.0,
            y: explorer.y + 67.0,
            width: explorer.width - 12.0,
            height: (explorer.height - 75.0).max(0.0),
        };
        let inspector_title = Rect {
            x: inspector.x + PANEL_INSET,
            y: inspector.y + PANEL_INSET,
            width: inspector.width - PANEL_INSET * 2.0,
            height: 22.0,
        };
        const ACTION_BAND_HEIGHT: f32 = 40.0;
        let inspector_body = Rect {
            x: inspector.x + PANEL_INSET,
            y: inspector.y + 61.0,
            width: inspector.width - PANEL_INSET * 2.0,
            height: (inspector.height - 75.0 - (ACTION_BAND_HEIGHT + PANEL_GAP) * 3.0).max(0.0),
        };
        let preview_button = Rect {
            x: inspector.x + PANEL_INSET,
            y: inspector.bottom() - PANEL_INSET - ACTION_BAND_HEIGHT * 3.0 - PANEL_GAP * 2.0,
            width: inspector.width - PANEL_INSET * 2.0,
            height: ACTION_BAND_HEIGHT,
        };
        let sign_button = Rect {
            x: inspector.x + PANEL_INSET,
            y: inspector.bottom() - PANEL_INSET - ACTION_BAND_HEIGHT * 2.0 - PANEL_GAP,
            width: inspector.width - PANEL_INSET * 2.0,
            height: ACTION_BAND_HEIGHT,
        };
        let action_button = Rect {
            x: inspector.x + PANEL_INSET,
            y: inspector.bottom() - PANEL_INSET - ACTION_BAND_HEIGHT,
            width: inspector.width - PANEL_INSET * 2.0,
            height: ACTION_BAND_HEIGHT,
        };
        let action_confirm = Rect {
            x: action_button.x,
            y: action_button.y,
            width: (action_button.width - 8.0) / 2.0,
            height: ACTION_BAND_HEIGHT,
        };
        let action_cancel = Rect {
            x: action_confirm.right() + 8.0,
            y: action_button.y,
            width: action_confirm.width,
            height: ACTION_BAND_HEIGHT,
        };
        Self {
            header,
            archives,
            explorer,
            inspector,
            archives_title,
            archive_list,
            explorer_title,
            root_button,
            up_button,
            breadcrumb,
            explorer_list,
            inspector_title,
            inspector_body,
            action_button,
            action_confirm,
            action_cancel,
            sign_button,
            preview_button,
        }
    }

    fn explorer_visible_rows(self) -> usize {
        (self.explorer_list.height / ROW_HEIGHT).floor().max(1.0) as usize
    }
}

/// The largest centered rect with the given aspect ratio that fits
/// entirely inside `bounds`, preserving aspect ratio (never stretching).
fn fit_within(natural_width: f32, natural_height: f32, bounds: Rect) -> Rect {
    if natural_width <= 0.0 || natural_height <= 0.0 {
        return bounds;
    }
    let scale = (bounds.width / natural_width)
        .min(bounds.height / natural_height)
        .max(0.001);
    let width = natural_width * scale;
    let height = natural_height * scale;
    Rect {
        x: bounds.x + (bounds.width - width) / 2.0,
        y: bounds.y + (bounds.height - height) / 2.0,
        width,
        height,
    }
}

fn paint_panel(scene: &mut Vec<ui_core::PaintOp>, rect: Rect, background: Color) {
    let mut panel = PanelModel::new(rect);
    panel.background = Some(background);
    panel.border = Some(PANEL_BORDER);
    panel.paint(scene);
}

fn paint_button(
    scene: &mut Vec<ui_core::PaintOp>,
    rect: Rect,
    label: &str,
    enabled: bool,
    hovered: bool,
) {
    scene.push(ui_core::PaintOp::FillRect {
        rect,
        color: match (enabled, hovered) {
            (true, true) => HOVER_FILL,
            (true, false) => SELECTED,
            (false, _) => Color::rgba(0x22, 0x2c, 0x3d, 0xff),
        },
    });
    scene.push(ui_core::PaintOp::StrokeRect {
        rect,
        color: match (enabled, hovered) {
            (_, true) => HOVER_BORDER,
            (true, false) => ACCENT,
            (false, false) => PANEL_BORDER,
        },
    });
    paint_text(
        scene,
        label,
        rect,
        CAPTION_FONT,
        if enabled { TEXT } else { MUTED },
        HorizontalAlign::Center,
    );
}

fn paint_text(
    scene: &mut Vec<ui_core::PaintOp>,
    text: &str,
    rect: Rect,
    font_size: u16,
    color: Color,
    horizontal_align: HorizontalAlign,
) {
    let mut label = LabelModel::new(text, rect);
    label.style.font_size = font_size;
    label.style.color = color;
    label.style.horizontal_align = horizontal_align;
    label.style.vertical_align = VerticalAlign::Middle;
    label.style.overflow = TextOverflow::EllipsisEnd;
    label.paint(scene);
}

fn paint_multiline(
    scene: &mut Vec<ui_core::PaintOp>,
    text: &str,
    rect: Rect,
    font_size: u16,
    color: Color,
) {
    let style = TextStyle {
        color,
        font_size,
        horizontal_align: HorizontalAlign::Left,
        vertical_align: VerticalAlign::Top,
        vertical_metric_mode: ui_core::TextVerticalMetricMode::LogicalLineBox,
        layout_mode: ui_core::TextLayoutMode::MultiLine,
        overflow: TextOverflow::EllipsisEnd,
    };
    scene.push(ui_core::PaintOp::Text {
        rect,
        clip_rect: Some(rect),
        text: text.to_string(),
        style,
    });
}

fn paint_key_value(
    scene: &mut Vec<ui_core::PaintOp>,
    x: f32,
    key: &str,
    value: &str,
    y: f32,
    width: f32,
    value_color: Color,
) {
    paint_text(
        scene,
        key,
        Rect {
            x,
            y,
            width,
            height: 15.0,
        },
        CAPTION_FONT,
        MUTED,
        HorizontalAlign::Left,
    );
    paint_text(
        scene,
        value,
        Rect {
            x,
            y: y + 15.0,
            width,
            height: 17.0,
        },
        CAPTION_FONT,
        value_color,
        HorizontalAlign::Left,
    );
}

fn paint_bar(
    scene: &mut Vec<ui_core::PaintOp>,
    bounds: Rect,
    label: &str,
    value: u64,
    maximum: u64,
    color: Color,
    format_as_bytes: bool,
) {
    let label_width = 68.0;
    paint_text(
        scene,
        label,
        Rect {
            x: bounds.x,
            y: bounds.y,
            width: label_width,
            height: bounds.height,
        },
        CAPTION_FONT,
        MUTED,
        HorizontalAlign::Left,
    );
    let bar = Rect {
        x: bounds.x + label_width,
        y: bounds.y + 3.0,
        width: (bounds.width - label_width - 72.0).max(20.0),
        height: 11.0,
    };
    scene.push(ui_core::PaintOp::FillRect {
        rect: bar,
        color: Color::rgba(0x29, 0x35, 0x47, 0xff),
    });
    let fraction = value as f64 / maximum.max(1) as f64;
    scene.push(ui_core::PaintOp::FillRect {
        rect: Rect {
            x: bar.x,
            y: bar.y,
            width: (bar.width as f64 * fraction).clamp(0.0, bar.width as f64) as f32,
            height: bar.height,
        },
        color,
    });
    paint_text(
        scene,
        &if format_as_bytes {
            format_bytes(value)
        } else {
            format_number(value)
        },
        Rect {
            x: bar.right() + 5.0,
            y: bounds.y,
            width: 66.0,
            height: bounds.height,
        },
        CAPTION_FONT,
        TEXT,
        HorizontalAlign::Right,
    );
}

fn format_number(value: u64) -> String {
    let digits = value.to_string();
    let mut result = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            result.push(',');
        }
        result.push(character);
    }
    result
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn format_timestamp(timestamp: Option<u64>) -> String {
    timestamp
        .map(|value| format!("unix {value}"))
        .unwrap_or_else(|| "not recorded".to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        directory_children, fit_within, format_bytes, format_number, summarize_manifest,
        BrowserEntryKind, Rect,
    };
    use data::archive_cas::{ArchiveEntry, ArchiveManifest, ArchiveObject};
    use data::cas::CasHash;

    #[test]
    fn fit_within_centers_and_preserves_aspect_ratio_when_wider_than_tall() {
        let bounds = Rect {
            x: 100.0,
            y: 100.0,
            width: 400.0,
            height: 200.0,
        };
        // 800x200 is wider (aspect-wise) than the 400x200 bounds, so width
        // is the limiting dimension: scale = 400/800 = 0.5, giving 800x100,
        // vertically centered.
        let fit = fit_within(800.0, 200.0, bounds);
        assert_eq!((fit.width, fit.height), (400.0, 100.0));
        assert_eq!(fit.x, 100.0);
        assert_eq!(fit.y, 150.0);
    }

    #[test]
    fn fit_within_centers_and_preserves_aspect_ratio_when_taller_than_wide() {
        let bounds = Rect {
            x: 0.0,
            y: 0.0,
            width: 200.0,
            height: 400.0,
        };
        // 200x800 is taller (aspect-wise) than the 200x400 bounds, so
        // height is the limiting dimension: scale = 400/800 = 0.5, giving
        // 100x400, horizontally centered.
        let fit = fit_within(200.0, 800.0, bounds);
        assert_eq!((fit.width, fit.height), (100.0, 400.0));
        assert_eq!(fit.x, 50.0);
        assert_eq!(fit.y, 0.0);
    }

    fn object(bytes: &[u8], size: u64) -> ArchiveObject {
        ArchiveObject {
            hash: CasHash::digest(bytes),
            size,
        }
    }

    #[test]
    fn summary_counts_objects_once_per_manifest() {
        let shared = object(b"shared", 10);
        let manifest = ArchiveManifest::new(
            "test-archive",
            "Test source",
            42,
            vec![
                ArchiveEntry::Directory {
                    path: "docs".to_string(),
                    modified_at_unix_secs: None,
                },
                ArchiveEntry::File {
                    path: "docs/one.txt".to_string(),
                    object: shared,
                    modified_at_unix_secs: None,
                },
                ArchiveEntry::File {
                    path: "docs/two.txt".to_string(),
                    object: shared,
                    modified_at_unix_secs: None,
                },
                ArchiveEntry::Symlink {
                    path: "latest".to_string(),
                    target: "docs/one.txt".to_string(),
                },
                ArchiveEntry::Excluded {
                    path: "blocked.ipa".to_string(),
                    reason: "owner-approved".to_string(),
                },
            ],
        )
        .unwrap();

        let summary = summarize_manifest(&manifest).unwrap();
        assert_eq!(summary.directories, 1);
        assert_eq!(summary.files, 2);
        assert_eq!(summary.symlinks, 1);
        assert_eq!(summary.excluded, 1);
        assert_eq!(summary.logical_bytes, 20);
        assert_eq!(summary.unique_objects, 1);
        assert_eq!(summary.unique_object_bytes, 10);
        assert_eq!(summary.root.len(), 64);
    }

    #[test]
    fn children_group_nested_paths_and_sort_folders_first() {
        let manifest = ArchiveManifest::new(
            "test-archive",
            "Test source",
            42,
            vec![
                ArchiveEntry::File {
                    path: "readme.txt".to_string(),
                    object: object(b"readme", 6),
                    modified_at_unix_secs: None,
                },
                ArchiveEntry::File {
                    path: "art/a.png".to_string(),
                    object: object(b"a", 1),
                    modified_at_unix_secs: None,
                },
                ArchiveEntry::File {
                    path: "art/texture/b.png".to_string(),
                    object: object(b"b", 2),
                    modified_at_unix_secs: None,
                },
                ArchiveEntry::Unreadable {
                    path: "lost.dat".to_string(),
                    operation: "open".to_string(),
                    error: "I/O error".to_string(),
                },
            ],
        )
        .unwrap();

        let root = directory_children(&manifest, None);
        assert_eq!(
            root.iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            ["art", "readme.txt", "lost.dat"]
        );
        assert_eq!(root[0].kind, BrowserEntryKind::Folder);
        assert_eq!(root[0].files, 2);
        assert_eq!(root[0].logical_bytes, 3);
        assert_eq!(root[2].kind, BrowserEntryKind::Unreadable);

        let art = directory_children(&manifest, Some("art"));
        assert_eq!(art.len(), 2);
        assert_eq!(art[0].name, "texture");
        assert_eq!(art[0].kind, BrowserEntryKind::Folder);
        assert_eq!(art[1].name, "a.png");
        assert_eq!(art[1].kind, BrowserEntryKind::File);
    }

    #[test]
    fn human_formatters_keep_large_counts_readable() {
        assert_eq!(format_number(586_828_433_953), "586,828,433,953");
        assert_eq!(format_bytes(1_073_741_824), "1.0 GiB");
    }

    #[test]
    fn children_cache_only_recomputes_on_navigation_not_on_every_call() {
        // Pins the fix for a real perf bug: directory_children is a full
        // linear scan of every manifest entry, and this app used to call it
        // fresh on every input-handling pass and every paint pass -- two to
        // three full scans per event, even a bare pointer move. On a
        // 178k-entry manifest that's the difference between an idle app and
        // a hot one during a mouse-move storm.
        let directory = tempfile::tempdir().unwrap();
        let cas_root = directory.path().join("cas");
        let store = data::archive_cas::ArchiveCasStorage::new(&cas_root).unwrap();
        let object = store.add_content(b"hello").unwrap().object;
        let manifest = ArchiveManifest::new(
            "cache-test",
            "test",
            1,
            vec![
                ArchiveEntry::Directory {
                    path: "folder".to_string(),
                    modified_at_unix_secs: None,
                },
                ArchiveEntry::File {
                    path: "folder/a.txt".to_string(),
                    object,
                    modified_at_unix_secs: None,
                },
                ArchiveEntry::File {
                    path: "root.txt".to_string(),
                    object,
                    modified_at_unix_secs: None,
                },
            ],
        )
        .unwrap();
        store.write_manifest(&manifest).unwrap();

        let catalog = super::ArchiveCatalog::read(std::slice::from_ref(&cas_root)).unwrap();
        let mut app = super::BrowserApp::new(vec![cas_root], Vec::new(), catalog, 0, None);

        app.ensure_children_cache();
        assert_eq!(app.children_cache.len(), 2);
        let key_at_root = app.children_cache_key.clone();

        // No state change: the key must stay exactly what it was (not just
        // equal in value, the same recorded generation) -- this is the
        // "don't recompute" case a pointer-move storm hits constantly.
        app.ensure_children_cache();
        assert_eq!(app.children_cache_key, key_at_root);
        assert_eq!(app.children_cache.len(), 2);

        // A real navigation must still update it.
        app.current_prefix = Some("folder".to_string());
        app.ensure_children_cache();
        assert_ne!(app.children_cache_key, key_at_root);
        assert_eq!(app.children_cache.len(), 1);
        assert_eq!(app.children_cache[0].name, "a.txt");
    }
}
