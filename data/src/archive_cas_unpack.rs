//! Unpacks the zips an archive holds, so their contents are stored, and deduplicated,
//! as ordinary files: `photos.zip` becomes a folder `photos.zip/` holding the members.
//! Zips inside zips are unpacked too, up to a depth. Every member is streamed into the
//! store and checked against the CRC-32 its zip records.
//!
//! The result is a new version made from the archive's current one by a `Derived`
//! record naming every zip unpacked, plus a report for the caller to print (member
//! counts, zips left whole and why). Nothing is deleted: the zips' own objects stay
//! until a purge retires the parent version.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use anyhow::{Context, Result};

use crate::archive_cas::{
    ArchiveCasStorage, ArchiveChange, ArchiveEntry, ArchiveManifest, ArchiveObject, ArchiveRecord,
};
use crate::cas::CasHash;
use crate::zip::{looks_like_zip, member_reader, read_entries};

/// Extensions whose files are zips to unpack. Office documents (.docx, .xlsx, ...) are
/// zips too, but they are documents, and stay whole.
pub const UNPACK_EXTENSIONS: &[&str] = &["zip", "ipa", "jar", "apk"];

/// Most levels of zips inside zips unpacked by default.
pub const DEFAULT_MAX_DEPTH: usize = 3;

/// A zip that was unpacked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnpackedZip {
    /// Its path in the manifest; it is now a folder of its members.
    pub path: String,
    /// The zip itself, as it was stored.
    pub object: ArchiveObject,
    pub members: usize,
    pub member_bytes: u64,
}

/// A zip left as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedZip {
    pub path: String,
    pub reason: String,
}

/// What an unpack did, for the caller to print. The new version's `Derived` record
/// names the zips; this adds the counts and the zips left whole.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnpackReport {
    pub unpacked: Vec<UnpackedZip>,
    pub skipped: Vec<SkippedZip>,
    /// Member objects written that the store did not already hold, and their bytes.
    pub new_objects: usize,
    pub new_object_bytes: u64,
    /// Members whose content was already stored (deduplicated).
    pub reused_objects: usize,
}

fn is_zip_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').is_some_and(|(stem, extension)| {
        !stem.is_empty() && UNPACK_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
    })
}

/// Whether `manifest` holds any file [`unpack_zips`] would try to unpack.
#[must_use]
pub fn has_zips(manifest: &ArchiveManifest) -> bool {
    manifest
        .entries
        .iter()
        .any(|e| matches!(e, ArchiveEntry::File { path, .. } if is_zip_path(path)))
}

/// A member name as a clean relative path, or `None` when it is unsafe (`..`, NUL) and
/// the zip must be left whole. `Some("")` is a name with nothing in it (skipped).
fn clean_member_name(name: &str) -> Option<String> {
    if name.contains('\0') {
        return None;
    }
    let normalized = name.replace('\\', "/");
    let mut parts = Vec::new();
    for part in normalized.split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// Progress of an unpack, for a caller that shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnpackProgress<'a> {
    Zip {
        path: &'a str,
        members: usize,
        bytes: u64,
    },
    Skipped {
        path: &'a str,
        reason: &'a str,
    },
}

/// The zips `manifest` would unpack, with member counts and sizes, reading only each
/// zip's central directory. Nothing is written.
///
/// # Errors
/// When a zip's stored object cannot be opened.
pub fn survey(
    store: &ArchiveCasStorage,
    manifest: &ArchiveManifest,
) -> Result<(Vec<UnpackedZip>, Vec<SkippedZip>)> {
    let mut unpackable = Vec::new();
    let mut skipped = Vec::new();
    for entry in &manifest.entries {
        let ArchiveEntry::File { path, object, .. } = entry else {
            continue;
        };
        if !is_zip_path(path) {
            continue;
        }
        match check_zip(store, *object) {
            Ok((members, member_bytes)) => unpackable.push(UnpackedZip {
                path: path.clone(),
                object: *object,
                members,
                member_bytes,
            }),
            Err(reason) => skipped.push(SkippedZip {
                path: path.clone(),
                reason,
            }),
        }
    }
    Ok((unpackable, skipped))
}

/// Members and their total size, or why the zip cannot be unpacked.
fn check_zip(
    store: &ArchiveCasStorage,
    object: ArchiveObject,
) -> std::result::Result<(usize, u64), String> {
    let mut object = store
        .open_object_seekable(object)
        .map_err(|e| format!("object missing: {e:#}"))?;
    let file = object.file();
    if !looks_like_zip(file) {
        return Err("not a zip".into());
    }
    let entries = read_entries(file).map_err(|e| format!("{e:#}"))?;
    if let Some(entry) = entries.iter().find(|e| !e.is_supported()) {
        return Err(if entry.encrypted {
            format!("encrypted member {}", entry.name)
        } else {
            format!(
                "member {} uses compression method {}",
                entry.name, entry.method
            )
        });
    }
    if let Some(entry) = entries
        .iter()
        .find(|e| clean_member_name(&e.name).is_none())
    {
        return Err(format!("unsafe member name {:?}", entry.name));
    }
    Ok((entries.len(), entries.iter().map(|e| e.size).sum()))
}

/// One zip's members as manifest entries under `folder`, their contents stored.
struct Unpacked {
    entries: Vec<ArchiveEntry>,
    nested: Vec<(String, ArchiveObject)>,
    members: usize,
    member_bytes: u64,
    new_objects: usize,
    new_object_bytes: u64,
    reused_objects: usize,
}

fn unpack_one(
    store: &ArchiveCasStorage,
    folder: &str,
    object: ArchiveObject,
) -> std::result::Result<Unpacked, String> {
    check_zip(store, object)?;
    let mut seekable = store
        .open_object_seekable(object)
        .map_err(|e| format!("{e:#}"))?;
    let file = seekable.file();
    let members = read_entries(file).map_err(|e| format!("{e:#}"))?;
    let mut out = Unpacked {
        entries: Vec::new(),
        nested: Vec::new(),
        members: 0,
        member_bytes: 0,
        new_objects: 0,
        new_object_bytes: 0,
        reused_objects: 0,
    };
    let mut seen = BTreeSet::new();
    let mut folders = BTreeSet::new();
    for member in &members {
        let name = clean_member_name(&member.name).expect("checked");
        if name.is_empty() || !seen.insert(name.clone()) {
            continue;
        }
        let path = format!("{folder}/{name}");
        // Every folder above the member, which a zip need not list.
        let mut parent = folder.to_string();
        for part in name.split('/').take(name.split('/').count() - 1) {
            parent = format!("{parent}/{part}");
            folders.insert(parent.clone());
        }
        if member.is_dir {
            folders.insert(path);
            continue;
        }
        let mut reader = member_reader(&mut *file, member).map_err(|e| format!("{e:#}"))?;
        let stored = store
            .add_stream(&mut reader)
            .map_err(|e| format!("{path}: {e:#}"))?;
        drop(reader);
        if stored.inserted {
            out.new_objects += 1;
            out.new_object_bytes += stored.object.size;
        } else {
            out.reused_objects += 1;
        }
        out.members += 1;
        out.member_bytes += stored.object.size;
        if is_zip_path(&path) {
            out.nested.push((path.clone(), stored.object));
        }
        out.entries.push(ArchiveEntry::File {
            path,
            object: stored.object,
            modified_at_unix_secs: member.modified_unix_secs,
        });
    }
    folders.remove(folder);
    // Where a folder and a file share a path, the caller keeps whichever came first
    // (the file, pushed above).
    for folder_path in folders {
        out.entries.push(ArchiveEntry::Directory {
            path: folder_path,
            modified_at_unix_secs: None,
        });
    }
    out.entries.push(ArchiveEntry::Directory {
        path: folder.to_string(),
        modified_at_unix_secs: None,
    });
    Ok(out)
}

/// Unpacks every zip in `manifest` (whose root is `root`), and zips inside them up to
/// `max_depth` levels, into the store. Returns the new version, made from `root` by a
/// `Derived` record naming each zip unpacked (`None` when none could be), and the
/// report. A zip that cannot be unpacked (encrypted, unsupported compression, unsafe
/// names, a CRC mismatch) is left as a file and listed in the report.
///
/// # Errors
/// When the new manifest is invalid, or `actor` or `reason` is empty.
#[allow(clippy::too_many_arguments)]
pub fn unpack_zips(
    store: &ArchiveCasStorage,
    manifest: &ArchiveManifest,
    root: CasHash,
    max_depth: usize,
    now_unix_secs: u64,
    actor: &str,
    reason: &str,
    mut progress: impl FnMut(UnpackProgress<'_>),
) -> Result<(Option<ArchiveManifest>, UnpackReport)> {
    let mut by_path: BTreeMap<String, ArchiveEntry> = manifest
        .entries
        .iter()
        .map(|e| (e.path().to_string(), e.clone()))
        .collect();
    let mut queue: VecDeque<(String, ArchiveObject, usize)> = manifest
        .entries
        .iter()
        .filter_map(|e| match e {
            ArchiveEntry::File { path, object, .. } if is_zip_path(path) => {
                Some((path.clone(), *object, 1))
            }
            _ => None,
        })
        .collect();
    let mut report = UnpackReport::default();
    while let Some((path, object, depth)) = queue.pop_front() {
        match unpack_one(store, &path, object) {
            Ok(unpacked) => {
                progress(UnpackProgress::Zip {
                    path: &path,
                    members: unpacked.members,
                    bytes: unpacked.member_bytes,
                });
                by_path.remove(&path);
                for entry in unpacked.entries {
                    by_path.entry(entry.path().to_string()).or_insert(entry);
                }
                if depth < max_depth {
                    for (nested_path, nested_object) in unpacked.nested {
                        queue.push_back((nested_path, nested_object, depth + 1));
                    }
                }
                report.new_objects += unpacked.new_objects;
                report.new_object_bytes += unpacked.new_object_bytes;
                report.reused_objects += unpacked.reused_objects;
                report.unpacked.push(UnpackedZip {
                    path,
                    object,
                    members: unpacked.members,
                    member_bytes: unpacked.member_bytes,
                });
            }
            Err(reason) => {
                progress(UnpackProgress::Skipped {
                    path: &path,
                    reason: &reason,
                });
                report.skipped.push(SkippedZip { path, reason });
            }
        }
    }
    if report.unpacked.is_empty() {
        return Ok((None, report));
    }
    let record = ArchiveRecord::new(
        ArchiveChange::Derived {
            paths: report.unpacked.iter().map(|zip| zip.path.clone()).collect(),
        },
        actor,
        reason,
        now_unix_secs,
    )?;
    let mut unpacked = ArchiveManifest::new(
        manifest.archive_id.clone(),
        manifest.source_label.clone(),
        now_unix_secs,
        by_path.into_values().collect(),
    )
    .context("the unpacked manifest is invalid")?;
    unpacked.parents = vec![root];
    unpacked.records = vec![record];
    Ok((Some(unpacked), report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// A zip of `members` (name, bytes), deflated, written with the `flate2` encoder and
    /// the zip layout by hand.
    fn zip_bytes(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, plain) in members {
            let mut encoder =
                flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(plain).unwrap();
            let data = encoder.finish().unwrap();
            let offset = out.len() as u32;
            let crc = crc32fast::hash(plain);
            let fields = |sig: u32, buf: &mut Vec<u8>, central: bool| {
                buf.extend(sig.to_le_bytes());
                if central {
                    buf.extend(20u16.to_le_bytes());
                }
                buf.extend(20u16.to_le_bytes());
                buf.extend(0x0800u16.to_le_bytes());
                buf.extend(8u16.to_le_bytes());
                buf.extend([0u8; 4]);
                buf.extend(crc.to_le_bytes());
                buf.extend((data.len() as u32).to_le_bytes());
                buf.extend((plain.len() as u32).to_le_bytes());
                buf.extend((name.len() as u16).to_le_bytes());
                buf.extend(0u16.to_le_bytes());
            };
            fields(0x0403_4b50, &mut out, false);
            out.extend(name.as_bytes());
            out.extend(&data);
            fields(0x0201_4b50, &mut central, true);
            central.extend([0u8; 6]);
            central.extend(0u32.to_le_bytes());
            central.extend(offset.to_le_bytes());
            central.extend(name.as_bytes());
        }
        let directory_offset = out.len() as u32;
        out.extend(&central);
        out.extend(0x0605_4b50u32.to_le_bytes());
        out.extend([0u8; 4]);
        out.extend((members.len() as u16).to_le_bytes());
        out.extend((members.len() as u16).to_le_bytes());
        out.extend((central.len() as u32).to_le_bytes());
        out.extend(directory_offset.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out
    }

    fn file(store: &ArchiveCasStorage, path: &str, bytes: &[u8]) -> ArchiveEntry {
        ArchiveEntry::File {
            path: path.into(),
            object: store.add_content(bytes).unwrap().object,
            modified_at_unix_secs: None,
        }
    }

    #[test]
    fn zips_become_folders_of_their_members_and_shared_content_is_stored_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArchiveCasStorage::new(dir.path().join("cas")).unwrap();
        let inner = zip_bytes(&[("deep.txt", b"deep inside")]);
        let outer = zip_bytes(&[
            ("docs/readme.txt", b"already archived"),
            ("nested.zip", &inner),
            ("new.txt", b"only in the zip"),
        ]);
        let broken = {
            let mut z = zip_bytes(&[("x.txt", b"will not match")]);
            let at = z.len() - 22 - 46 - 5 + 16; // central header CRC field of the one member
            z[at] ^= 0xff;
            z
        };
        let manifest = ArchiveManifest::new(
            "docs",
            "Docs",
            1,
            vec![
                file(&store, "readme.txt", b"already archived"),
                file(&store, "Backup.ZIP", &outer),
                file(&store, "bad.zip", &broken),
                file(
                    &store,
                    "report.docx",
                    &zip_bytes(&[("word/document.xml", b"<w/>")]),
                ),
            ],
        )
        .unwrap();
        let root = manifest.digest().unwrap();
        let (unpacked, log) = unpack_zips(
            &store,
            &manifest,
            root,
            DEFAULT_MAX_DEPTH,
            2,
            "test",
            "test",
            |_| {},
        )
        .unwrap();
        let unpacked = unpacked.expect("two zips unpacked");
        let paths: Vec<&str> = unpacked.entries.iter().map(ArchiveEntry::path).collect();
        assert_eq!(
            paths,
            [
                "Backup.ZIP",
                "Backup.ZIP/docs",
                "Backup.ZIP/docs/readme.txt",
                "Backup.ZIP/nested.zip",
                "Backup.ZIP/nested.zip/deep.txt",
                "Backup.ZIP/new.txt",
                "bad.zip",
                "readme.txt",
                "report.docx",
            ]
        );
        assert!(matches!(
            &unpacked.entries[0],
            ArchiveEntry::Directory { .. }
        ));
        assert!(
            matches!(&unpacked.entries[6], ArchiveEntry::File { .. }),
            "bad.zip stays whole"
        );
        assert_eq!(unpacked.parents(), vec![root]);
        assert_eq!(
            unpacked.records[0].change,
            crate::archive_cas::ArchiveChange::Derived {
                paths: vec!["Backup.ZIP".into(), "Backup.ZIP/nested.zip".into()]
            }
        );
        assert_eq!(log.unpacked.len(), 2);
        assert_eq!(log.skipped.len(), 1);
        assert!(
            log.skipped[0].reason.contains("the zip records"),
            "{:?}",
            log.skipped
        );
        assert_eq!(log.reused_objects, 1, "readme.txt was already stored");
        // Every member reads back verified.
        for entry in &unpacked.entries {
            if let ArchiveEntry::File { object, .. } = entry {
                store.verify_object(*object).unwrap();
            }
        }
        let (path, _) = store.write_manifest(&unpacked).unwrap();
        assert_eq!(store.read_manifest(&path).unwrap(), unpacked);
    }

    #[test]
    fn unsafe_names_leave_the_zip_whole_and_the_survey_reads_only_directories() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArchiveCasStorage::new(dir.path().join("cas")).unwrap();
        let evil = zip_bytes(&[("../escape.txt", b"no")]);
        let fine = zip_bytes(&[("a/b.txt", b"yes"), ("c.txt", b"also")]);
        let manifest = ArchiveManifest::new(
            "x",
            "X",
            1,
            vec![
                file(&store, "evil.zip", &evil),
                file(&store, "fine.jar", &fine),
            ],
        )
        .unwrap();
        let (unpackable, skipped) = survey(&store, &manifest).unwrap();
        assert_eq!(unpackable.len(), 1);
        assert_eq!((unpackable[0].members, unpackable[0].member_bytes), (2, 7));
        assert!(skipped[0].reason.contains("unsafe member name"));
        assert_eq!(clean_member_name("./a//b\\c"), Some("a/b/c".into()));
        assert_eq!(clean_member_name("a/../b"), None);
        assert!(is_zip_path("x/Photos.ZIP") && !is_zip_path("x/.zip") && !is_zip_path("a.docx"));
    }
}
