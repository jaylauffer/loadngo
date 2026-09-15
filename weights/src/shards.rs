//! A checkpoint split across several safetensors files.
//!
//! Large checkpoints ship as numbered shards plus `model.safetensors.index.json`, whose
//! `weight_map` names the shard file holding each tensor. [`ShardSet::open`] reads every
//! shard's header and cross-checks it against the index in both directions, so a tensor
//! is found in exactly one place and the index cannot point somewhere its shard
//! disagrees with. A directory without an index is read as every `*.safetensors` file in
//! it, in lexical order.

use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
};

use serde_json::Value;

use crate::safetensors::{Header, SafetensorsError, TensorInfo};

/// The conventional name of a sharded checkpoint's index.
pub const INDEX_FILE: &str = "model.safetensors.index.json";

/// One shard file and its validated header.
#[derive(Clone, Debug)]
pub struct Shard {
    pub file_name: String,
    pub path: PathBuf,
    pub header: Header,
}

/// Every shard of a checkpoint, with each tensor located exactly once.
#[derive(Clone, Debug)]
pub struct ShardSet {
    shards: Vec<Shard>,
    by_name: HashMap<String, (usize, usize)>,
}

/// Why a checkpoint's shards cannot be used together.
#[derive(Debug, thiserror::Error)]
pub enum ShardError {
    #[error("cannot read {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("{INDEX_FILE} is malformed: {0}")]
    Index(String),
    #[error("shard {file:?}: {source}")]
    Shard {
        file: String,
        source: SafetensorsError,
    },
    #[error("no safetensors shards in {0}")]
    NoShards(PathBuf),
    #[error("tensor {name:?} appears in both {first:?} and {second:?}")]
    Duplicate {
        name: String,
        first: String,
        second: String,
    },
    #[error("tensor {name:?} is in {found:?} but the index does not list it")]
    Unlisted { name: String, found: String },
    #[error("the index places {name:?} in {listed:?}, but it is in {found:?}")]
    Misplaced {
        name: String,
        listed: String,
        found: String,
    },
    #[error("the index places {name:?} in {listed:?}, which does not contain it")]
    Missing { name: String, listed: String },
}

impl ShardSet {
    /// Opens the checkpoint in `dir`, through its index when present.
    pub fn open(dir: &Path) -> Result<Self, ShardError> {
        let index_path = dir.join(INDEX_FILE);
        if index_path.is_file() {
            let text = fs::read_to_string(&index_path).map_err(|source| ShardError::Io {
                path: index_path.clone(),
                source,
            })?;
            let weight_map = parse_weight_map(&text)?;
            let mut files: Vec<String> = weight_map.values().cloned().collect();
            files.sort();
            files.dedup();
            let set = Self::from_files(dir, files)?;
            set.check_against(&weight_map)?;
            Ok(set)
        } else {
            let mut files = Vec::new();
            let entries = fs::read_dir(dir).map_err(|source| ShardError::Io {
                path: dir.to_path_buf(),
                source,
            })?;
            for entry in entries {
                let entry = entry.map_err(|source| ShardError::Io {
                    path: dir.to_path_buf(),
                    source,
                })?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.ends_with(".safetensors") && entry.path().is_file() {
                    files.push(name);
                }
            }
            files.sort();
            if files.is_empty() {
                return Err(ShardError::NoShards(dir.to_path_buf()));
            }
            Self::from_files(dir, files)
        }
    }

    fn from_files(dir: &Path, files: Vec<String>) -> Result<Self, ShardError> {
        let mut shards = Vec::with_capacity(files.len());
        let mut by_name: HashMap<String, (usize, usize)> = HashMap::new();
        for (s, file_name) in files.into_iter().enumerate() {
            let path = dir.join(&file_name);
            let header = Header::read_path(&path).map_err(|source| ShardError::Shard {
                file: file_name.clone(),
                source,
            })?;
            for (t, tensor) in header.tensors().iter().enumerate() {
                if let Some(&(first, _)) = by_name.get(&tensor.name) {
                    return Err(ShardError::Duplicate {
                        name: tensor.name.clone(),
                        first: shards.get(first).map_or_else(
                            || file_name.clone(),
                            |shard: &Shard| shard.file_name.clone(),
                        ),
                        second: file_name,
                    });
                }
                by_name.insert(tensor.name.clone(), (s, t));
            }
            shards.push(Shard {
                file_name,
                path,
                header,
            });
        }
        Ok(Self { shards, by_name })
    }

    fn check_against(&self, weight_map: &HashMap<String, String>) -> Result<(), ShardError> {
        for (name, &(s, _)) in &self.by_name {
            let found = &self.shards[s].file_name;
            match weight_map.get(name) {
                None => {
                    return Err(ShardError::Unlisted {
                        name: name.clone(),
                        found: found.clone(),
                    })
                }
                Some(listed) if listed != found => {
                    return Err(ShardError::Misplaced {
                        name: name.clone(),
                        listed: listed.clone(),
                        found: found.clone(),
                    })
                }
                Some(_) => {}
            }
        }
        if let Some((name, listed)) = weight_map
            .iter()
            .find(|(name, _)| !self.by_name.contains_key(*name))
        {
            return Err(ShardError::Missing {
                name: name.clone(),
                listed: listed.clone(),
            });
        }
        Ok(())
    }

    /// Where a tensor lives: its shard and its location within it.
    pub fn locate(&self, name: &str) -> Option<(&Shard, &TensorInfo)> {
        self.by_name
            .get(name)
            .map(|&(s, t)| (&self.shards[s], &self.shards[s].header.tensors()[t]))
    }

    /// Shards in the order they were opened.
    pub fn shards(&self) -> &[Shard] {
        &self.shards
    }

    /// Tensors across all shards.
    pub fn tensor_count(&self) -> usize {
        self.by_name.len()
    }
}

/// The index's `weight_map`, refusing shard names that could leave the directory.
fn parse_weight_map(text: &str) -> Result<HashMap<String, String>, ShardError> {
    let root: Value =
        serde_json::from_str(text).map_err(|error| ShardError::Index(error.to_string()))?;
    let map = root
        .get("weight_map")
        .and_then(Value::as_object)
        .ok_or_else(|| ShardError::Index("weight_map must be an object".to_owned()))?;
    map.iter()
        .map(|(name, file)| {
            let file = file.as_str().ok_or_else(|| {
                ShardError::Index(format!("{name:?} does not map to a file name"))
            })?;
            let plain = Path::new(file).file_name().and_then(|f| f.to_str()) == Some(file);
            if !plain || !file.ends_with(".safetensors") {
                return Err(ShardError::Index(format!(
                    "{name:?} maps to {file:?}, which is not a plain .safetensors file name"
                )));
            }
            Ok((name.clone(), file.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::safetensors::tests::file_bytes;

    fn shard(dir: &Path, file: &str, names: &[&str]) {
        let entries: Vec<String> = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                format!(
                    r#""{name}":{{"dtype":"F32","shape":[1],"data_offsets":[{},{}]}}"#,
                    i * 4,
                    i * 4 + 4
                )
            })
            .collect();
        let header = format!("{{{}}}", entries.join(","));
        fs::write(
            dir.join(file),
            file_bytes(&header, &vec![0; names.len() * 4]),
        )
        .unwrap();
    }

    fn index(dir: &Path, map: &[(&str, &str)]) {
        let pairs: Vec<String> = map.iter().map(|(n, f)| format!(r#""{n}":"{f}""#)).collect();
        let text = format!(
            r#"{{"metadata":{{"total_size":0}},"weight_map":{{{}}}}}"#,
            pairs.join(",")
        );
        fs::write(dir.join(INDEX_FILE), text).unwrap();
    }

    #[test]
    fn an_index_locates_each_tensor_in_its_shard() {
        let dir = tempfile::tempdir().unwrap();
        shard(dir.path(), "model-00001-of-00002.safetensors", &["a", "b"]);
        shard(dir.path(), "model-00002-of-00002.safetensors", &["c"]);
        index(
            dir.path(),
            &[
                ("a", "model-00001-of-00002.safetensors"),
                ("b", "model-00001-of-00002.safetensors"),
                ("c", "model-00002-of-00002.safetensors"),
            ],
        );
        let set = ShardSet::open(dir.path()).expect("consistent checkpoint");
        assert_eq!(set.tensor_count(), 3);
        let (shard, tensor) = set.locate("c").unwrap();
        assert_eq!(shard.file_name, "model-00002-of-00002.safetensors");
        assert_eq!(tensor.offset, shard.header.data_start());
        assert!(set.locate("d").is_none());
    }

    #[test]
    fn an_index_that_disagrees_with_its_shards_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        shard(dir.path(), "s1.safetensors", &["a"]);
        shard(dir.path(), "s2.safetensors", &["b"]);

        // Both shards are named but their tensors are swapped: each is found elsewhere.
        index(
            dir.path(),
            &[("a", "s2.safetensors"), ("b", "s1.safetensors")],
        );
        assert!(matches!(
            ShardSet::open(dir.path()),
            Err(ShardError::Misplaced { name, listed, found })
                if (name == "a" && listed == "s2.safetensors" && found == "s1.safetensors")
                    || (name == "b" && listed == "s1.safetensors" && found == "s2.safetensors")
        ));

        // Only the named shards are opened, so a tensor listed in a shard that lacks it
        // is missing there, even if another file in the directory holds it.
        index(
            dir.path(),
            &[("a", "s2.safetensors"), ("b", "s2.safetensors")],
        );
        assert!(
            matches!(ShardSet::open(dir.path()), Err(ShardError::Missing { name, .. }) if name == "a")
        );

        index(
            dir.path(),
            &[
                ("a", "s1.safetensors"),
                ("b", "s2.safetensors"),
                ("x", "s1.safetensors"),
            ],
        );
        assert!(
            matches!(ShardSet::open(dir.path()), Err(ShardError::Missing { name, .. }) if name == "x")
        );

        // An index naming only s1 is consistent with s1; b is simply not part of it.
        index(dir.path(), &[("a", "s1.safetensors")]);
        let partial = ShardSet::open(dir.path()).expect("index names one shard");
        assert!(partial.locate("b").is_none());

        // A tensor in a named shard that the index does not list is refused.
        shard(dir.path(), "s1.safetensors", &["a", "z"]);
        assert!(matches!(
            ShardSet::open(dir.path()),
            Err(ShardError::Unlisted { name, .. }) if name == "z"
        ));

        index(dir.path(), &[("a", "../s1.safetensors")]);
        assert!(matches!(
            ShardSet::open(dir.path()),
            Err(ShardError::Index(_))
        ));
    }

    #[test]
    fn without_an_index_every_shard_is_read_and_a_tensor_may_appear_once() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            ShardSet::open(dir.path()),
            Err(ShardError::NoShards(_))
        ));

        shard(dir.path(), "b.safetensors", &["y"]);
        shard(dir.path(), "a.safetensors", &["x"]);
        fs::write(dir.path().join("notes.txt"), "not a shard").unwrap();
        let set = ShardSet::open(dir.path()).expect("two shards");
        let names: Vec<&str> = set.shards().iter().map(|s| s.file_name.as_str()).collect();
        assert_eq!(names, ["a.safetensors", "b.safetensors"]);

        shard(dir.path(), "c.safetensors", &["x"]);
        assert!(
            matches!(ShardSet::open(dir.path()), Err(ShardError::Duplicate { name, .. }) if name == "x")
        );
    }
}
