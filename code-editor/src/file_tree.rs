//! The folder tree on the left: one listing per expanded directory, loaded
//! on demand, flattened into rows only when something changes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ui_core::{HorizontalAlign, PaintOp, Point, Rect};

use crate::draw;
use crate::fs_ops::DirEntry;
use crate::theme;

#[derive(Debug, Clone, Default)]
struct DirState {
    /// `None` until the listing arrives.
    entries: Option<Vec<DirEntry>>,
    expanded: bool,
    requested: bool,
    error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
}

/// What a click in the tree asks of the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeAction {
    /// List this directory (it was expanded before its listing was loaded).
    List(PathBuf),
    Open(PathBuf),
}

#[derive(Debug, Clone)]
pub struct FileTree {
    root: PathBuf,
    dirs: HashMap<PathBuf, DirState>,
    rows: Vec<TreeRow>,
    rows_stale: bool,
    pub bounds: Rect,
    pub scroll_y: f32,
    pub selected: Option<PathBuf>,
    hover_row: Option<usize>,
}

impl FileTree {
    /// A tree showing `root`'s contents, expanded. The caller lists `root`
    /// (see [`FileTree::take_listing_requests`]).
    pub fn new(root: PathBuf) -> Self {
        let mut dirs = HashMap::new();
        dirs.insert(
            root.clone(),
            DirState {
                expanded: true,
                ..DirState::default()
            },
        );
        Self {
            root,
            dirs,
            rows: Vec::new(),
            rows_stale: true,
            bounds: Rect::default(),
            scroll_y: 0.0,
            selected: None,
            hover_row: None,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Directories that are expanded but neither listed nor requested yet;
    /// marks them requested.
    pub fn take_listing_requests(&mut self) -> Vec<PathBuf> {
        let mut wanted = Vec::new();
        for (path, state) in &mut self.dirs {
            if state.expanded && state.entries.is_none() && !state.requested {
                state.requested = true;
                wanted.push(path.clone());
            }
        }
        wanted.sort();
        wanted
    }

    pub fn apply_listing(&mut self, dir: &Path, entries: Result<Vec<DirEntry>, String>) {
        let Some(state) = self.dirs.get_mut(dir) else {
            return;
        };
        state.requested = false;
        match entries {
            Ok(entries) => {
                state.entries = Some(entries);
                state.error = None;
            }
            Err(error) => {
                state.entries = Some(Vec::new());
                state.error = Some(error);
            }
        }
        self.rows_stale = true;
    }

    /// Drops every listing so expanded directories are listed again.
    pub fn refresh(&mut self) {
        for state in self.dirs.values_mut() {
            state.entries = None;
            state.requested = false;
        }
        self.rows_stale = true;
    }

    /// Expands the directories above `path` (to reveal it) and selects it.
    pub fn reveal(&mut self, path: &Path) {
        let mut dir = path.parent();
        while let Some(current) = dir {
            if !current.starts_with(&self.root) {
                break;
            }
            self.dirs.entry(current.to_path_buf()).or_default().expanded = true;
            if current == self.root {
                break;
            }
            dir = current.parent();
        }
        self.selected = Some(path.to_path_buf());
        self.rows_stale = true;
    }

    /// The expanded directories, for the session.
    pub fn expanded_dirs(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<_> = self
            .dirs
            .iter()
            .filter(|(path, state)| state.expanded && **path != self.root)
            .map(|(path, _)| path.clone())
            .collect();
        dirs.sort();
        dirs
    }

    pub fn set_expanded(&mut self, dir: &Path, expanded: bool) {
        self.dirs.entry(dir.to_path_buf()).or_default().expanded = expanded;
        self.rows_stale = true;
    }

    pub fn rows(&mut self) -> &[TreeRow] {
        if self.rows_stale {
            let mut rows = Vec::new();
            self.push_rows(&self.root.clone(), 0, &mut rows);
            self.rows = rows;
            self.rows_stale = false;
            self.clamp_scroll();
        }
        &self.rows
    }

    fn push_rows(&self, dir: &Path, depth: usize, rows: &mut Vec<TreeRow>) {
        let Some(entries) = self.dirs.get(dir).and_then(|state| state.entries.as_ref()) else {
            return;
        };
        for entry in entries {
            let path = dir.join(&entry.name);
            let expanded = entry.is_dir && self.dirs.get(&path).is_some_and(|state| state.expanded);
            rows.push(TreeRow {
                path: path.clone(),
                name: entry.name.clone(),
                depth,
                is_dir: entry.is_dir,
                expanded,
            });
            if expanded {
                self.push_rows(&path, depth + 1, rows);
            }
        }
    }

    fn content_height(&self) -> f32 {
        self.rows.len() as f32 * theme::TREE_ROW_HEIGHT
    }

    fn clamp_scroll(&mut self) {
        let max = (self.content_height() - self.bounds.height).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, max);
    }

    pub fn scroll_by(&mut self, delta: f32) -> bool {
        let before = self.scroll_y;
        self.scroll_y += delta;
        self.clamp_scroll();
        (self.scroll_y - before).abs() > f32::EPSILON
    }

    fn row_at(&mut self, point: Point) -> Option<usize> {
        if !self.bounds.contains(point) {
            return None;
        }
        let index = ((point.y - self.bounds.y + self.scroll_y) / theme::TREE_ROW_HEIGHT) as usize;
        (index < self.rows().len()).then_some(index)
    }

    /// Hover tracking; true when the hovered row changed.
    pub fn pointer_moved(&mut self, point: Point) -> bool {
        let hover = self.row_at(point);
        let changed = hover != self.hover_row;
        self.hover_row = hover;
        changed
    }

    /// A click: toggles a directory or opens a file.
    pub fn click(&mut self, point: Point) -> Option<TreeAction> {
        let index = self.row_at(point)?;
        let row = self.rows()[index].clone();
        self.selected = Some(row.path.clone());
        if !row.is_dir {
            return Some(TreeAction::Open(row.path));
        }
        let state = self.dirs.entry(row.path.clone()).or_default();
        state.expanded = !state.expanded;
        let needs_listing = state.expanded && state.entries.is_none() && !state.requested;
        if needs_listing {
            state.requested = true;
        }
        self.rows_stale = true;
        needs_listing.then_some(TreeAction::List(row.path))
    }

    pub fn paint(&mut self, scene: &mut Vec<PaintOp>) {
        let bounds = self.bounds;
        draw::fill(scene, bounds, theme::PANEL);
        let row_height = theme::TREE_ROW_HEIGHT;
        let first = (self.scroll_y / row_height).floor().max(0.0) as usize;
        let visible = (bounds.height / row_height).ceil() as usize + 1;
        let scroll_y = self.scroll_y;
        let hover_row = self.hover_row;
        let selected = self.selected.clone();
        let rows = self.rows();
        if rows.is_empty() {
            draw::label(
                scene,
                "Loading…",
                Rect {
                    x: bounds.x + 12.0,
                    y: bounds.y,
                    width: bounds.width - 24.0,
                    height: row_height,
                },
                theme::TEXT_DIM,
                theme::UI_FONT,
                HorizontalAlign::Left,
            );
            return;
        }
        for (index, row) in rows.iter().enumerate().skip(first).take(visible) {
            let y = bounds.y + index as f32 * row_height - scroll_y;
            let rect = Rect {
                x: bounds.x,
                y,
                width: bounds.width,
                height: row_height,
            };
            if selected.as_ref() == Some(&row.path) {
                draw::fill(scene, draw::intersect(rect, bounds), theme::SELECTED);
            } else if hover_row == Some(index) {
                draw::fill(scene, draw::intersect(rect, bounds), theme::HOVER);
            }
            let indent = bounds.x + 10.0 + row.depth as f32 * theme::TREE_INDENT;
            if row.is_dir && y >= bounds.y && y + row_height <= bounds.bottom() {
                draw::disclosure(
                    scene,
                    Point {
                        x: indent + 4.0,
                        y: y + row_height / 2.0,
                    },
                    row.expanded,
                    theme::TEXT_DIM,
                );
            }
            draw::label_in(
                scene,
                &row.name,
                Rect {
                    x: indent + 14.0,
                    y,
                    width: (bounds.right() - indent - 20.0).max(0.0),
                    height: row_height,
                },
                bounds,
                if row.is_dir {
                    theme::TEXT
                } else {
                    theme::TEXT_DIM
                },
                theme::UI_FONT,
                HorizontalAlign::Left,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, is_dir: bool) -> DirEntry {
        DirEntry {
            name: name.to_string(),
            is_dir,
        }
    }

    fn tree() -> FileTree {
        let mut tree = FileTree::new(PathBuf::from("/w"));
        tree.bounds = Rect {
            x: 0.0,
            y: 0.0,
            width: 200.0,
            height: 240.0,
        };
        assert_eq!(tree.take_listing_requests(), vec![PathBuf::from("/w")]);
        assert!(tree.take_listing_requests().is_empty(), "requested once");
        tree.apply_listing(
            Path::new("/w"),
            Ok(vec![entry("src", true), entry("Cargo.toml", false)]),
        );
        tree
    }

    fn row_point(index: usize) -> Point {
        Point {
            x: 40.0,
            y: index as f32 * theme::TREE_ROW_HEIGHT + 4.0,
        }
    }

    #[test]
    fn expanding_a_directory_asks_for_its_listing_once_and_shows_its_children() {
        let mut tree = tree();
        assert_eq!(tree.rows().len(), 2);
        assert_eq!(
            tree.click(row_point(0)),
            Some(TreeAction::List(PathBuf::from("/w/src")))
        );
        assert!(tree.take_listing_requests().is_empty());
        tree.apply_listing(Path::new("/w/src"), Ok(vec![entry("main.rs", false)]));
        let names: Vec<_> = tree
            .rows()
            .iter()
            .map(|row| (row.name.clone(), row.depth))
            .collect();
        assert_eq!(
            names,
            vec![
                ("src".to_string(), 0),
                ("main.rs".to_string(), 1),
                ("Cargo.toml".to_string(), 0)
            ]
        );
        assert_eq!(
            tree.click(row_point(1)),
            Some(TreeAction::Open(PathBuf::from("/w/src/main.rs")))
        );
        // Collapse, then expand again: the listing is kept.
        assert_eq!(tree.click(row_point(0)), None);
        assert_eq!(tree.rows().len(), 2);
        assert_eq!(tree.click(row_point(0)), None);
        assert_eq!(tree.rows().len(), 3);
    }

    #[test]
    fn reveal_expands_parents_for_a_restored_session() {
        let mut tree = tree();
        tree.reveal(Path::new("/w/src/bin/tool.rs"));
        assert_eq!(
            tree.take_listing_requests(),
            vec![PathBuf::from("/w/src"), PathBuf::from("/w/src/bin")]
        );
        assert_eq!(
            tree.expanded_dirs(),
            vec![PathBuf::from("/w/src"), PathBuf::from("/w/src/bin")]
        );
    }

    #[test]
    fn a_failed_listing_shows_an_empty_directory() {
        let mut tree = tree();
        tree.click(row_point(0));
        tree.apply_listing(Path::new("/w/src"), Err("permission denied".to_string()));
        assert_eq!(tree.rows().len(), 2);
    }
}
