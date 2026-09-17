//! A local, metadata-only browser for loadngo Archive CAS manifests.
//!
//! The browser deliberately reads only canonical manifest JSON. It does not
//! open, preview, upload, or otherwise inspect archive blob payloads.

use anyhow::{anyhow, bail, Context, Result};
use data::archive_cas::{ArchiveEntry, ArchiveManifest};
use data::cas::CasHash;
use loadngo_host_core::{FrameDemand, HostKey, InputSnapshot, WindowDescriptor};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use ui_core::{
    Color, HorizontalAlign, LabelModel, PanelModel, Point, Rect, TextOverflow, TextStyle,
    VerticalAlign,
};

const WINDOW_WIDTH: i32 = 1_560;
const WINDOW_HEIGHT: i32 = 980;
const OUTER_GUTTER: f32 = 18.0;
const HEADER_HEIGHT: f32 = 78.0;
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

fn main() {
    if let Err(error) = run() {
        eprintln!("archive_cas_browser: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse()?;
    let catalog = ArchiveCatalog::read(&args.cas_root)?;
    let selected_archive = catalog.initial_selection(args.manifest.as_deref());
    loadngo_host_desktop::launch(window_descriptor(), None, async move {
        BrowserApp::new(args.cas_root, catalog, selected_archive)
            .run()
            .await;
    });
    Ok(())
}

fn window_descriptor() -> WindowDescriptor {
    WindowDescriptor {
        title: "loadngo Archive CAS browser — read-only".to_string(),
        width: Some(WINDOW_WIDTH),
        height: Some(WINDOW_HEIGHT),
        high_dpi: true,
        linux_wm_class: Some("loadngo-archive-cas-browser"),
    }
}

#[derive(Debug)]
struct Args {
    cas_root: PathBuf,
    manifest: Option<PathBuf>,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut cas_root = None;
        let mut manifest = None;
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--cas-root" => cas_root = args.next().map(PathBuf::from),
                "--manifest" => manifest = args.next().map(PathBuf::from),
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                other => bail!("unknown argument: {other}"),
            }
        }
        Ok(Self {
            cas_root: cas_root.ok_or_else(|| anyhow!("missing --cas-root <directory>"))?,
            manifest,
        })
    }
}

fn print_usage() {
    eprintln!(
        "Usage: cargo run -p loadngo-host-desktop --bin archive_cas_browser -- \\\n+         --cas-root <archive-directory> [--manifest <archive-manifest.json>]"
    );
}

#[derive(Debug, Clone)]
struct ArchiveCatalog {
    archives: Vec<ArchiveRecord>,
    warnings: Vec<String>,
}

impl ArchiveCatalog {
    fn read(cas_root: &Path) -> Result<Self> {
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

        let mut archives = Vec::new();
        let mut warnings = Vec::new();
        for path in paths {
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            match ArchiveRecord::read(&path) {
                Ok(record) => archives.push(record),
                Err(error) => warnings.push(format!("{}: {error:#}", path.display())),
            }
        }
        archives.sort_by(|left, right| {
            right
                .manifest
                .created_at_unix_secs
                .cmp(&left.manifest.created_at_unix_secs)
                .then_with(|| left.manifest.archive_id.cmp(&right.manifest.archive_id))
        });
        if archives.is_empty() {
            bail!(
                "no readable canonical archive manifests found in {}",
                manifests_root.display()
            );
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

#[derive(Debug, Clone)]
struct ArchiveRecord {
    manifest_path: PathBuf,
    manifest: ArchiveManifest,
    summary: ArchiveSummary,
}

impl ArchiveRecord {
    fn read(path: &Path) -> Result<Self> {
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

#[derive(Debug)]
struct BrowserApp {
    cas_root: PathBuf,
    catalog: ArchiveCatalog,
    selected_archive: usize,
    current_prefix: Option<String>,
    selected_path: Option<String>,
    child_scroll: usize,
    message: Option<String>,
}

impl BrowserApp {
    fn new(cas_root: PathBuf, catalog: ArchiveCatalog, selected_archive: usize) -> Self {
        Self {
            cas_root,
            catalog,
            selected_archive,
            current_prefix: None,
            selected_path: None,
            child_scroll: 0,
            message: None,
        }
    }

    async fn run(&mut self) {
        loop {
            let frame = loadngo_host_desktop::capture_frame();
            if frame.input.key_pressed(HostKey::Escape) {
                break;
            }
            self.handle_input(&frame.input, frame.surface.width, frame.surface.height);
            let mut scene = Vec::new();
            self.paint(&mut scene, frame.surface.width, frame.surface.height);
            loadngo_host_desktop::clear(BACKGROUND);
            loadngo_host_desktop::render_widget_paint_ops(&scene);
            loadngo_host_desktop::next_frame(FrameDemand::after(Duration::from_millis(16))).await;
        }
    }

    fn selected(&self) -> &ArchiveRecord {
        &self.catalog.archives[self.selected_archive]
    }

    fn select_archive(&mut self, index: usize) {
        if index < self.catalog.archives.len() && index != self.selected_archive {
            self.selected_archive = index;
            self.current_prefix = None;
            self.selected_path = None;
            self.child_scroll = 0;
        }
    }

    fn navigate_to(&mut self, path: String) {
        self.current_prefix = Some(path);
        self.selected_path = None;
        self.child_scroll = 0;
    }

    fn refresh(&mut self) {
        let selected_manifest = self
            .catalog
            .archives
            .get(self.selected_archive)
            .map(|archive| archive.manifest_path.clone());
        match ArchiveCatalog::read(&self.cas_root) {
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
                self.message = Some("Manifest index reloaded; blobs were not read.".to_string());
            }
            Err(error) => self.message = Some(format!("Refresh failed: {error:#}")),
        }
    }

    fn handle_input(&mut self, input: &InputSnapshot, width: f32, height: f32) {
        if input.key_pressed(HostKey::R) {
            self.refresh();
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
        let children =
            directory_children(&self.selected().manifest, self.current_prefix.as_deref());
        let visible_rows = layout.explorer_visible_rows();
        self.clamp_child_scroll(children.len(), visible_rows);
        let pointer = Point {
            x: input.mouse_x,
            y: input.mouse_y,
        };

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
            let index = ((pointer.y - layout.archive_list.y) / ROW_HEIGHT).floor() as usize;
            self.select_archive(index);
            return;
        }
        if layout.explorer_list.contains(pointer) {
            let visible_index =
                ((pointer.y - layout.explorer_list.y) / ROW_HEIGHT).floor() as usize;
            let index = self.child_scroll.saturating_add(visible_index);
            if let Some(item) = children.get(index) {
                if item.kind == BrowserEntryKind::Folder {
                    self.navigate_to(item.path.clone());
                } else {
                    self.selected_path = Some(item.path.clone());
                }
            }
        }
    }

    fn clamp_child_scroll(&mut self, child_count: usize, visible_rows: usize) {
        self.child_scroll = self
            .child_scroll
            .min(child_count.saturating_sub(visible_rows.max(1)));
    }

    fn paint(&self, scene: &mut Vec<ui_core::PaintOp>, width: f32, height: f32) {
        let layout = AppLayout::new(width, height);
        let record = self.selected();
        let children = directory_children(&record.manifest, self.current_prefix.as_deref());
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
            "Local metadata view — manifests only; archive blob bytes are never opened",
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
        paint_text(
            scene,
            "R refresh   Home root   Backspace up   Esc close",
            Rect {
                x: layout.header.x + layout.header.width * 0.58,
                y: layout.header.y + 25.0,
                width: layout.header.width * 0.40 - PANEL_INSET,
                height: 24.0,
            },
            CAPTION_FONT,
            ACCENT,
            HorizontalAlign::Right,
        );

        self.paint_archives(scene, &layout);
        self.paint_explorer(scene, &layout, &children, scroll, visible_rows);
        self.paint_inspector(scene, &layout, record, &children);

        if let Some(message) = self.message.as_deref() {
            paint_text(
                scene,
                message,
                Rect {
                    x: layout.header.x + PANEL_INSET,
                    y: layout.header.bottom() - 22.0,
                    width: layout.header.width - PANEL_INSET * 2.0,
                    height: 18.0,
                },
                CAPTION_FONT,
                CAUTION,
                HorizontalAlign::Left,
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

        for (index, archive) in self.catalog.archives.iter().enumerate() {
            let row = Rect {
                x: layout.archive_list.x,
                y: layout.archive_list.y + index as f32 * ROW_HEIGHT,
                width: layout.archive_list.width,
                height: ROW_HEIGHT - 2.0,
            };
            if row.bottom() > layout.archive_list.bottom() {
                break;
            }
            let selected = index == self.selected_archive;
            if selected {
                scene.push(ui_core::PaintOp::FillRect {
                    rect: row,
                    color: SELECTED,
                });
            }
            let status = archive_status(&archive.summary);
            paint_text(
                scene,
                &archive.manifest.archive_id,
                Rect {
                    x: row.x + 9.0,
                    y: row.y + 3.0,
                    width: row.width - 18.0,
                    height: 17.0,
                },
                BODY_FONT,
                if selected { TEXT } else { ACCENT },
                HorizontalAlign::Left,
            );
            paint_text(
                scene,
                &format!(
                    "{} • {} files",
                    status.0,
                    format_number(archive.summary.files)
                ),
                Rect {
                    x: row.x + 9.0,
                    y: row.y + 20.0,
                    width: row.width - 18.0,
                    height: 15.0,
                },
                CAPTION_FONT,
                status.1,
                HorizontalAlign::Left,
            );
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
        );
        paint_button(scene, layout.up_button, "Up", self.current_prefix.is_some());
        let breadcrumb = self.current_prefix.as_deref().unwrap_or("/");
        paint_text(
            scene,
            breadcrumb,
            layout.breadcrumb,
            CAPTION_FONT,
            ACCENT,
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
            }
            paint_text(
                scene,
                &format!("{}  {}", item.kind.label(), item.name),
                Rect {
                    x: row.x + 8.0,
                    y: row.y + 3.0,
                    width: row.width * 0.62,
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
                    x: row.x + 8.0,
                    y: row.y + 20.0,
                    width: row.width - 16.0,
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
                height: (layout.inspector.bottom() - PANEL_INSET - y).max(0.0),
            },
            CAPTION_FONT,
            MUTED,
        );
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
        let inspector_body = Rect {
            x: inspector.x + PANEL_INSET,
            y: inspector.y + 61.0,
            width: inspector.width - PANEL_INSET * 2.0,
            height: (inspector.height - 75.0).max(0.0),
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
        }
    }

    fn explorer_visible_rows(self) -> usize {
        (self.explorer_list.height / ROW_HEIGHT).floor().max(1.0) as usize
    }
}

fn paint_panel(scene: &mut Vec<ui_core::PaintOp>, rect: Rect, background: Color) {
    let mut panel = PanelModel::new(rect);
    panel.background = Some(background);
    panel.border = Some(PANEL_BORDER);
    panel.paint(scene);
}

fn paint_button(scene: &mut Vec<ui_core::PaintOp>, rect: Rect, label: &str, enabled: bool) {
    scene.push(ui_core::PaintOp::FillRect {
        rect,
        color: if enabled {
            SELECTED
        } else {
            Color::rgba(0x22, 0x2c, 0x3d, 0xff)
        },
    });
    scene.push(ui_core::PaintOp::StrokeRect {
        rect,
        color: if enabled { ACCENT } else { PANEL_BORDER },
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
        directory_children, format_bytes, format_number, summarize_manifest, BrowserEntryKind,
    };
    use data::archive_cas::{ArchiveEntry, ArchiveManifest, ArchiveObject};
    use data::cas::CasHash;

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
}
