//! The safetensors container.
//!
//! A file is an 8-byte little-endian unsigned header length `N`, then `N` bytes of UTF-8
//! JSON, then a byte buffer. The header is an object beginning with `{`, optionally padded
//! with trailing spaces. Each key names a tensor and maps to its `dtype`, `shape` and
//! `data_offsets` (`[begin, end)` relative to the start of the byte buffer); the optional
//! `__metadata__` key maps to a string-to-string object. Tensors must index the entire
//! byte buffer, with no holes and no overlap, and element data is little-endian.
//!
//! [`Header`] enforces all of that before any offset is trusted: a checkpoint whose
//! header disagrees with its own byte buffer would otherwise hand plausible bytes from
//! the wrong place to every kernel downstream.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
    fs::File,
    io::{self, Read},
    path::Path,
};

use serde::de::{self, Deserialize, Deserializer, MapAccess, Visitor};
use serde_json::Value;

use crate::dtype::Dtype;

/// The largest header accepted, in bytes. Larger headers are refused before allocation.
pub const MAX_HEADER_BYTES: u64 = 100 * 1024 * 1024;

const METADATA_KEY: &str = "__metadata__";

/// One tensor's location and type, with offsets made absolute within the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TensorInfo {
    pub name: String,
    pub dtype: Dtype,
    pub shape: Vec<u64>,
    /// Absolute byte offset of the first element.
    pub offset: u64,
    /// Length of the element data in bytes.
    pub len: u64,
}

impl TensorInfo {
    /// Elements in the tensor; a rank-zero tensor holds one.
    pub fn numel(&self) -> u64 {
        self.shape.iter().product()
    }
}

/// A validated safetensors header.
#[derive(Clone, Debug)]
pub struct Header {
    tensors: Vec<TensorInfo>,
    by_name: HashMap<String, usize>,
    metadata: BTreeMap<String, String>,
    data_start: u64,
}

/// Why a safetensors header was refused.
#[derive(Debug, thiserror::Error)]
pub enum SafetensorsError {
    #[error("cannot read the file: {0}")]
    Io(#[from] io::Error),
    #[error("{len} bytes is too short to hold the 8-byte header length")]
    TooShort { len: u64 },
    #[error("header length {len} exceeds the {MAX_HEADER_BYTES}-byte limit")]
    HeaderTooLarge { len: u64 },
    #[error("header of {header} bytes runs past the end of a {file}-byte file")]
    HeaderPastEnd { header: u64, file: u64 },
    #[error("the header must be a JSON object starting with '{{'")]
    NotAnObject,
    #[error("the header is not valid UTF-8 JSON: {0}")]
    InvalidJson(String),
    #[error("`__metadata__` must map strings to strings: {0}")]
    BadMetadata(String),
    #[error("tensor {name:?}: {detail}")]
    BadEntry { name: String, detail: String },
    #[error("tensor {name:?} has unknown dtype {dtype:?}")]
    UnknownDtype { name: String, dtype: String },
    #[error("tensor {name:?} is {actual} bytes but its dtype and shape need {expected}")]
    SizeMismatch {
        name: String,
        expected: u64,
        actual: u64,
    },
    #[error("tensor {name:?} ends at {end}, past the {buffer}-byte data buffer")]
    OutOfBounds { name: String, end: u64, buffer: u64 },
    #[error("tensors {first:?} and {second:?} overlap")]
    Overlap { first: String, second: String },
    #[error("{len} bytes at buffer offset {at} belong to no tensor")]
    Hole { at: u64, len: u64 },
}

impl Header {
    /// The header length a file's first 8 bytes declare.
    pub fn declared_len(prefix: [u8; 8]) -> u64 {
        u64::from_le_bytes(prefix)
    }

    /// Parses and validates a header from the first `8 + N` bytes of a file of
    /// `file_len` bytes. `prefix` may be longer than the header; the rest is ignored.
    pub fn parse(prefix: &[u8], file_len: u64) -> Result<Self, SafetensorsError> {
        let head: [u8; 8] =
            prefix
                .get(..8)
                .and_then(|b| b.try_into().ok())
                .ok_or(SafetensorsError::TooShort {
                    len: prefix.len() as u64,
                })?;
        let header_len = Self::declared_len(head);
        check_header_len(header_len, file_len)?;
        let json = usize::try_from(8 + header_len)
            .ok()
            .and_then(|end| prefix.get(8..end))
            .ok_or(SafetensorsError::HeaderPastEnd {
                header: header_len,
                file: prefix.len() as u64,
            })?;
        if json.first() != Some(&b'{') {
            return Err(SafetensorsError::NotAnObject);
        }
        let entries: UniqueEntries = serde_json::from_slice(json)
            .map_err(|error| SafetensorsError::InvalidJson(error.to_string()))?;
        Self::validate(entries.0, 8 + header_len, file_len)
    }

    /// Reads and validates the header of the safetensors file at `path`, reading only
    /// the header bytes.
    pub fn read_path(path: &Path) -> Result<Self, SafetensorsError> {
        let mut file = File::open(path)?;
        let file_len = file.metadata()?.len();
        if file_len < 8 {
            return Err(SafetensorsError::TooShort { len: file_len });
        }
        let mut head = [0_u8; 8];
        file.read_exact(&mut head)?;
        let header_len = Self::declared_len(head);
        check_header_len(header_len, file_len)?;
        let mut prefix = vec![0_u8; 8 + usize::try_from(header_len).expect("bounded above")];
        prefix[..8].copy_from_slice(&head);
        file.read_exact(&mut prefix[8..])?;
        Self::parse(&prefix, file_len)
    }

    fn validate(
        entries: Vec<(String, Value)>,
        data_start: u64,
        file_len: u64,
    ) -> Result<Self, SafetensorsError> {
        let buffer = file_len - data_start;
        let mut metadata = BTreeMap::new();
        let mut tensors = Vec::with_capacity(entries.len());
        for (name, value) in entries {
            if name == METADATA_KEY {
                metadata = parse_metadata(&value)?;
                continue;
            }
            tensors.push(parse_entry(name, &value, data_start, buffer)?);
        }

        // Sort a view by position to prove the buffer is covered exactly once.
        let mut order: Vec<usize> = (0..tensors.len()).collect();
        order.sort_by_key(|&i| (tensors[i].offset, tensors[i].len));
        let mut cursor = data_start;
        let mut previous: Option<usize> = None;
        for &i in &order {
            let t = &tensors[i];
            if t.offset < cursor {
                let first = previous.map_or_else(String::new, |p| tensors[p].name.clone());
                return Err(SafetensorsError::Overlap {
                    first,
                    second: t.name.clone(),
                });
            }
            if t.offset > cursor {
                return Err(SafetensorsError::Hole {
                    at: cursor - data_start,
                    len: t.offset - cursor,
                });
            }
            cursor = t.offset + t.len;
            previous = Some(i);
        }
        if cursor != file_len {
            return Err(SafetensorsError::Hole {
                at: cursor - data_start,
                len: file_len - cursor,
            });
        }

        let by_name = tensors
            .iter()
            .enumerate()
            .map(|(i, t)| (t.name.clone(), i))
            .collect();
        Ok(Self {
            tensors,
            by_name,
            metadata,
            data_start,
        })
    }

    /// Tensors in header order.
    pub fn tensors(&self) -> &[TensorInfo] {
        &self.tensors
    }

    /// A tensor by exact name.
    pub fn get(&self, name: &str) -> Option<&TensorInfo> {
        self.by_name.get(name).map(|&i| &self.tensors[i])
    }

    /// The `__metadata__` map, empty when absent.
    pub fn metadata(&self) -> &BTreeMap<String, String> {
        &self.metadata
    }

    /// Absolute offset of the byte buffer.
    pub const fn data_start(&self) -> u64 {
        self.data_start
    }
}

fn check_header_len(header_len: u64, file_len: u64) -> Result<(), SafetensorsError> {
    if header_len > MAX_HEADER_BYTES {
        return Err(SafetensorsError::HeaderTooLarge { len: header_len });
    }
    if 8 + header_len > file_len {
        return Err(SafetensorsError::HeaderPastEnd {
            header: header_len,
            file: file_len,
        });
    }
    Ok(())
}

fn parse_metadata(value: &Value) -> Result<BTreeMap<String, String>, SafetensorsError> {
    let object = value
        .as_object()
        .ok_or_else(|| SafetensorsError::BadMetadata("not an object".to_owned()))?;
    object
        .iter()
        .map(|(key, value)| {
            value
                .as_str()
                .map(|text| (key.clone(), text.to_owned()))
                .ok_or_else(|| SafetensorsError::BadMetadata(format!("{key:?} is not a string")))
        })
        .collect()
}

fn parse_entry(
    name: String,
    value: &Value,
    data_start: u64,
    buffer: u64,
) -> Result<TensorInfo, SafetensorsError> {
    let bad = |detail: &str| SafetensorsError::BadEntry {
        name: name.clone(),
        detail: detail.to_owned(),
    };
    let object = value
        .as_object()
        .ok_or_else(|| bad("entry is not an object"))?;
    if let Some(extra) = object
        .keys()
        .find(|key| !matches!(key.as_str(), "dtype" | "shape" | "data_offsets"))
    {
        return Err(bad(&format!("unexpected field {extra:?}")));
    }
    let dtype_name = object
        .get("dtype")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("dtype must be a string"))?;
    let dtype = Dtype::from_name(dtype_name).ok_or_else(|| SafetensorsError::UnknownDtype {
        name: name.clone(),
        dtype: dtype_name.to_owned(),
    })?;
    let shape = object
        .get("shape")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("shape must be an array"))?
        .iter()
        .map(Value::as_u64)
        .collect::<Option<Vec<u64>>>()
        .ok_or_else(|| bad("shape entries must be non-negative integers"))?;
    let offsets = object
        .get("data_offsets")
        .and_then(Value::as_array)
        .filter(|pair| pair.len() == 2)
        .ok_or_else(|| bad("data_offsets must be a two-element array"))?;
    let (Some(begin), Some(end)) = (offsets[0].as_u64(), offsets[1].as_u64()) else {
        return Err(bad("data_offsets must be non-negative integers"));
    };
    if end < begin {
        return Err(bad("data_offsets end before they begin"));
    }
    if end > buffer {
        return Err(SafetensorsError::OutOfBounds { name, end, buffer });
    }
    let expected = shape
        .iter()
        .try_fold(1_u64, |n, &d| n.checked_mul(d))
        .and_then(|n| n.checked_mul(dtype.size_bytes() as u64))
        .ok_or_else(|| bad("shape overflows u64 bytes"))?;
    if expected != end - begin {
        return Err(SafetensorsError::SizeMismatch {
            name,
            expected,
            actual: end - begin,
        });
    }
    Ok(TensorInfo {
        name,
        dtype,
        shape,
        offset: data_start + begin,
        len: end - begin,
    })
}

/// The header's top-level entries in order, refusing duplicate keys. Plain JSON parsing
/// keeps the last duplicate silently, which would let two headers disagree about the same
/// file depending on the reader.
struct UniqueEntries(Vec<(String, Value)>);

impl<'de> Deserialize<'de> for UniqueEntries {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EntriesVisitor;

        impl<'de> Visitor<'de> for EntriesVisitor {
            type Value = UniqueEntries;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON object of tensor entries")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut seen = HashSet::new();
                let mut entries = Vec::new();
                while let Some((key, value)) = map.next_entry::<String, Value>()? {
                    if !seen.insert(key.clone()) {
                        return Err(de::Error::custom(format!("duplicate key {key:?}")));
                    }
                    entries.push((key, value));
                }
                Ok(UniqueEntries(entries))
            }
        }

        deserializer.deserialize_map(EntriesVisitor)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a safetensors file from a header JSON string and a data buffer.
    pub(crate) fn file_bytes(header: &str, data: &[u8]) -> Vec<u8> {
        let mut out = (header.len() as u64).to_le_bytes().to_vec();
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(data);
        out
    }

    fn parse(header: &str, data: &[u8]) -> Result<Header, SafetensorsError> {
        let bytes = file_bytes(header, data);
        Header::parse(&bytes, bytes.len() as u64)
    }

    #[test]
    fn a_well_formed_header_locates_every_tensor_and_keeps_metadata() {
        let header = r#"{"__metadata__":{"format":"pt"},"b":{"dtype":"F16","shape":[2],"data_offsets":[4,8]},"a":{"dtype":"F32","shape":[],"data_offsets":[0,4]},"empty":{"dtype":"U8","shape":[0,3],"data_offsets":[8,8]}}   "#;
        let parsed = parse(header, &[0; 8]).expect("valid file");
        let start = 8 + header.len() as u64;
        assert_eq!(parsed.data_start(), start);
        assert_eq!(parsed.tensors().len(), 3);
        assert_eq!(parsed.tensors()[0].name, "b", "header order is kept");
        let a = parsed.get("a").unwrap();
        assert_eq!(
            (a.dtype, a.offset, a.len, a.numel()),
            (Dtype::F32, start, 4, 1)
        );
        let b = parsed.get("b").unwrap();
        assert_eq!(
            (b.offset, b.len, b.shape.as_slice()),
            (start + 4, 4, &[2][..])
        );
        assert_eq!(parsed.get("empty").unwrap().numel(), 0);
        assert_eq!(
            parsed.metadata().get("format").map(String::as_str),
            Some("pt")
        );
    }

    #[test]
    fn the_header_is_refused_when_it_disagrees_with_its_buffer() {
        let entry = |dtype: &str, shape: &str, begin: u64, end: u64| {
            format!(
                r#""{dtype}{begin}":{{"dtype":"{dtype}","shape":{shape},"data_offsets":[{begin},{end}]}}"#
            )
        };
        let hole = format!("{{{}}}", entry("U8", "[2]", 0, 2));
        assert!(matches!(
            parse(&hole, &[0; 4]),
            Err(SafetensorsError::Hole { at: 2, len: 2 })
        ));

        let gap = format!(
            "{{{},{}}}",
            entry("U8", "[1]", 0, 1),
            entry("U16", "[1]", 2, 4)
        );
        assert!(matches!(
            parse(&gap, &[0; 4]),
            Err(SafetensorsError::Hole { at: 1, len: 1 })
        ));

        let overlap = format!(
            "{{{},{}}}",
            entry("U16", "[2]", 0, 4),
            entry("I16", "[1]", 2, 4)
        );
        assert!(matches!(
            parse(&overlap, &[0; 4]),
            Err(SafetensorsError::Overlap { .. })
        ));

        let size = format!("{{{}}}", entry("F32", "[2]", 0, 4));
        assert!(matches!(
            parse(&size, &[0; 4]),
            Err(SafetensorsError::SizeMismatch {
                expected: 8,
                actual: 4,
                ..
            })
        ));

        let past = format!("{{{}}}", entry("U8", "[8]", 0, 8));
        assert!(matches!(
            parse(&past, &[0; 4]),
            Err(SafetensorsError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn malformed_headers_are_refused_before_any_offset_is_trusted() {
        let one = r#""x":{"dtype":"U8","shape":[1],"data_offsets":[0,1]}"#;
        assert!(matches!(
            parse(&format!("{{{one},{one}}}"), &[0]),
            Err(SafetensorsError::InvalidJson(detail)) if detail.contains("duplicate")
        ));
        assert!(matches!(
            parse(" {}", &[]),
            Err(SafetensorsError::NotAnObject)
        ));
        assert!(matches!(
            parse("[1]", &[]),
            Err(SafetensorsError::NotAnObject)
        ));
        assert!(matches!(
            parse(
                r#"{"x":{"dtype":"F4","shape":[2],"data_offsets":[0,1]}}"#,
                &[0]
            ),
            Err(SafetensorsError::UnknownDtype { .. })
        ));
        assert!(matches!(
            parse(r#"{"__metadata__":{"n":1}}"#, &[]),
            Err(SafetensorsError::BadMetadata(_))
        ));
        assert!(matches!(
            parse(
                r#"{"x":{"dtype":"U8","shape":[1],"data_offsets":[0,1],"extra":0}}"#,
                &[0]
            ),
            Err(SafetensorsError::BadEntry { .. })
        ));
        assert!(matches!(
            parse(
                r#"{"x":{"dtype":"U8","shape":[-1],"data_offsets":[0,1]}}"#,
                &[0]
            ),
            Err(SafetensorsError::BadEntry { .. })
        ));

        let mut truncated = file_bytes("{}", &[]);
        truncated[0] = 200;
        assert!(matches!(
            Header::parse(&truncated, truncated.len() as u64),
            Err(SafetensorsError::HeaderPastEnd { .. })
        ));
        let huge = (MAX_HEADER_BYTES + 1).to_le_bytes();
        assert!(matches!(
            Header::parse(&huge, u64::MAX),
            Err(SafetensorsError::HeaderTooLarge { .. })
        ));
        assert!(matches!(
            Header::parse(&[0; 4], 4),
            Err(SafetensorsError::TooShort { .. })
        ));
    }

    #[test]
    fn reading_a_file_parses_only_its_header() {
        let header = r#"{"w":{"dtype":"BF16","shape":[2,2],"data_offsets":[0,8]}}"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model.safetensors");
        std::fs::write(&path, file_bytes(header, &[7; 8])).unwrap();
        let parsed = Header::read_path(&path).expect("reads");
        assert_eq!(parsed.get("w").unwrap().shape, vec![2, 2]);

        std::fs::write(&path, [1, 2, 3]).unwrap();
        assert!(matches!(
            Header::read_path(&path),
            Err(SafetensorsError::TooShort { len: 3 })
        ));
    }
}
