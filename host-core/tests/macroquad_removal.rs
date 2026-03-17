use std::fs;
use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("host-core should live under the loadngo workspace")
        .to_path_buf()
}

fn read_to_string(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

fn collect_files(root: &Path, extension: &str, out: &mut Vec<PathBuf>) {
    let entries =
        fs::read_dir(root).unwrap_or_else(|err| panic!("failed to read {}: {err}", root.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|err| {
            panic!(
                "failed to read directory entry under {}: {err}",
                root.display()
            )
        });
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, extension, out);
        } else if path.extension().and_then(|value| value.to_str()) == Some(extension) {
            out.push(path);
        }
    }
}

fn rust_source_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_files(root, "rs", &mut files);
    files.sort();
    files
}

fn assert_paths_do_not_contain(paths: &[PathBuf], needle: &str) {
    let workspace_root = workspace_root();
    let offenders: Vec<_> = paths
        .iter()
        .filter(|path| read_to_string(path).contains(needle))
        .map(|path| {
            path.strip_prefix(&workspace_root)
                .unwrap_or(path)
                .display()
                .to_string()
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "found `{needle}` in: {}",
        offenders.join(", ")
    );
}

#[test]
fn core_workspace_crates_remain_macroquad_free() {
    let root = workspace_root();
    let mut paths = vec![
        root.join("ui-core/Cargo.toml"),
        root.join("host-core/Cargo.toml"),
        root.join("gui/Cargo.toml"),
        root.join("gui-win32/Cargo.toml"),
    ];
    for crate_dir in ["ui-core/src", "host-core/src", "gui/src", "gui-win32/src"] {
        paths.extend(rust_source_files(&root.join(crate_dir)));
    }
    assert_paths_do_not_contain(&paths, "macroquad");
}

#[test]
fn backend_crates_no_longer_reference_macroquad() {
    let root = workspace_root();
    let mut paths = Vec::new();
    for path in [root.join("host-desktop/Cargo.toml"), root.join("host-mac/Cargo.toml")] {
        if path.exists() {
            paths.push(path);
        }
    }
    for crate_dir in ["host-desktop/src", "host-mac/src"] {
        let path = root.join(crate_dir);
        if path.exists() {
            paths.extend(rust_source_files(&path));
        }
    }
    assert_paths_do_not_contain(&paths, "macroquad");
}
