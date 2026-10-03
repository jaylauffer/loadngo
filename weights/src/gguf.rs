//! GGUF, the model file format of the ggml project (`docs/gguf.md` in ggml, version 3).
//!
//! A file is a header, then the tensor data:
//!
//! - the magic `GGUF`, a `u32` version, a `u64` tensor count and a `u64` count of
//!   metadata entries;
//! - each metadata entry: a key (a GGUF string: `u64` byte length, then UTF-8), a `u32`
//!   value type and the value. An array value is a `u32` element type, a `u64` length and
//!   the elements;
//! - each tensor's description: name, `u32` dimension count, that many `u64` dimensions
//!   (the first varies fastest), a `u32` ggml type and a `u64` offset from the start of
//!   the data;
//! - padding to the alignment (`general.alignment`, 32 when absent), where the data
//!   starts.
//!
//! Everything is little-endian. Versions 2 and 3 share this layout; version 1 (32-bit
//! counts) is refused. Only the element types listed in [`GgmlType`] have known sizes;
//! a tensor of any other type is described but has no byte length.

use std::collections::BTreeMap;
use std::path::Path;

use crate::reader::{FileReader, ReadError};

const MAGIC: [u8; 4] = *b"GGUF";
const DEFAULT_ALIGNMENT: u64 = 32;
/// The header is read in pieces of this size, doubled until it fits.
const FIRST_READ: usize = 16 * 1024 * 1024;

/// A metadata value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    U8(u8),
    I8(i8),
    U16(u16),
    I16(i16),
    U32(u32),
    I32(i32),
    F32(f32),
    Bool(bool),
    String(String),
    Array(Vec<Value>),
    U64(u64),
    I64(i64),
    F64(f64),
}

impl Value {
    /// The value as an unsigned integer, when it is one of the integer types and fits.
    pub fn as_u64(&self) -> Option<u64> {
        match *self {
            Self::U8(v) => Some(u64::from(v)),
            Self::U16(v) => Some(u64::from(v)),
            Self::U32(v) => Some(u64::from(v)),
            Self::U64(v) => Some(v),
            Self::I8(v) => u64::try_from(v).ok(),
            Self::I16(v) => u64::try_from(v).ok(),
            Self::I32(v) => u64::try_from(v).ok(),
            Self::I64(v) => u64::try_from(v).ok(),
            _ => None,
        }
    }

    /// The value as a float, when it is `f32` or `f64`.
    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            Self::F32(v) => Some(f64::from(v)),
            Self::F64(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Self::Array(values) => Some(values),
            _ => None,
        }
    }
}

/// A tensor element type, by its ggml number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GgmlType {
    F32,
    F16,
    /// Blocks of 32: an IEEE binary16 scale, then 32 signed bytes.
    Q8_0,
    Bf16,
    /// Blocks of 32: an E8M0 scale byte, then 16 bytes of E2M1 codes (see
    /// [`crate::mxfp4::ggml_to_ocp`]).
    Mxfp4,
    /// A type this module does not size.
    Other(u32),
}

impl GgmlType {
    pub fn from_id(id: u32) -> Self {
        match id {
            0 => Self::F32,
            1 => Self::F16,
            8 => Self::Q8_0,
            30 => Self::Bf16,
            39 => Self::Mxfp4,
            other => Self::Other(other),
        }
    }

    /// `(elements per block, bytes per block)`, or `None` for an unsized type.
    pub fn block(self) -> Option<(u64, u64)> {
        match self {
            Self::F32 => Some((1, 4)),
            Self::F16 | Self::Bf16 => Some((1, 2)),
            Self::Q8_0 => Some((32, 34)),
            Self::Mxfp4 => Some((32, 17)),
            Self::Other(_) => None,
        }
    }
}

/// One tensor's description.
#[derive(Clone, Debug, PartialEq)]
pub struct Tensor {
    pub name: String,
    /// Dimensions, the first varying fastest (a matrix of `rows` rows of `cols` is
    /// `[cols, rows]`).
    pub dims: Vec<u64>,
    pub ggml_type: GgmlType,
    /// Absolute byte offset in the file.
    pub offset: u64,
    /// Byte length, or `None` for an unsized type.
    pub len: Option<u64>,
}

impl Tensor {
    pub fn numel(&self) -> u64 {
        self.dims.iter().product()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GgufError {
    #[error("not a GGUF file (magic {0:02x?})")]
    Magic([u8; 4]),
    #[error("GGUF version {0} is not supported (2 and 3 are)")]
    Version(u32),
    #[error("the header continues past the {0} bytes read")]
    Truncated(usize),
    #[error("metadata value type {0} is unknown")]
    ValueType(u32),
    #[error("a GGUF string is not UTF-8")]
    Utf8,
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Read(#[from] ReadError),
}

/// A parsed GGUF header.
#[derive(Clone, Debug)]
pub struct Gguf {
    pub version: u32,
    pub metadata: BTreeMap<String, Value>,
    pub tensors: Vec<Tensor>,
    /// Absolute offset where tensor data starts.
    pub data_offset: u64,
    pub alignment: u64,
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], GgufError> {
        let end = self
            .at
            .checked_add(n)
            .filter(|&end| end <= self.bytes.len())
            .ok_or(GgufError::Truncated(self.bytes.len()))?;
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], GgufError> {
        Ok(self.take(N)?.try_into().expect("took N bytes"))
    }

    fn u32(&mut self) -> Result<u32, GgufError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, GgufError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    fn len(&mut self) -> Result<usize, GgufError> {
        let n = self.u64()?;
        // A length past the bytes at hand is either a longer header or a corrupt one;
        // either way, more bytes decide.
        usize::try_from(n)
            .ok()
            .filter(|&n| n <= self.bytes.len())
            .ok_or(GgufError::Truncated(self.bytes.len()))
    }

    fn string(&mut self) -> Result<String, GgufError> {
        let n = self.len()?;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| GgufError::Utf8)
    }

    fn value(&mut self, kind: u32, depth: u32) -> Result<Value, GgufError> {
        Ok(match kind {
            0 => Value::U8(self.array::<1>()?[0]),
            1 => Value::I8(i8::from_le_bytes(self.array()?)),
            2 => Value::U16(u16::from_le_bytes(self.array()?)),
            3 => Value::I16(i16::from_le_bytes(self.array()?)),
            4 => Value::U32(self.u32()?),
            5 => Value::I32(i32::from_le_bytes(self.array()?)),
            6 => Value::F32(f32::from_le_bytes(self.array()?)),
            7 => match self.array::<1>()?[0] {
                0 => Value::Bool(false),
                1 => Value::Bool(true),
                other => return Err(GgufError::Invalid(format!("bool byte {other}"))),
            },
            8 => Value::String(self.string()?),
            9 => {
                if depth > 0 {
                    return Err(GgufError::Invalid("nested metadata arrays".into()));
                }
                let element = self.u32()?;
                let n = self.len()?;
                let mut values = Vec::with_capacity(n);
                for _ in 0..n {
                    values.push(self.value(element, depth + 1)?);
                }
                Value::Array(values)
            }
            10 => Value::U64(self.u64()?),
            11 => Value::I64(i64::from_le_bytes(self.array()?)),
            12 => Value::F64(f64::from_le_bytes(self.array()?)),
            other => return Err(GgufError::ValueType(other)),
        })
    }
}

impl Gguf {
    /// Parses the header from the file's first bytes. [`GgufError::Truncated`] means the
    /// header needs more of the file than `prefix` holds.
    pub fn parse(prefix: &[u8], file_len: u64) -> Result<Self, GgufError> {
        let mut at = Cursor {
            bytes: prefix,
            at: 0,
        };
        let magic: [u8; 4] = at.array()?;
        if magic != MAGIC {
            return Err(GgufError::Magic(magic));
        }
        let version = at.u32()?;
        if !(2..=3).contains(&version) {
            return Err(GgufError::Version(version));
        }
        let tensor_count = at.len()?;
        let metadata_count = at.len()?;
        let mut metadata = BTreeMap::new();
        for _ in 0..metadata_count {
            let key = at.string()?;
            let kind = at.u32()?;
            let value = at.value(kind, 0)?;
            if metadata.insert(key.clone(), value).is_some() {
                return Err(GgufError::Invalid(format!("metadata key {key:?} repeats")));
            }
        }
        let alignment = match metadata.get("general.alignment") {
            None => DEFAULT_ALIGNMENT,
            Some(value) => value
                .as_u64()
                .filter(|a| a.is_power_of_two())
                .ok_or_else(|| GgufError::Invalid("general.alignment".into()))?,
        };
        let mut described = Vec::with_capacity(tensor_count);
        for _ in 0..tensor_count {
            let name = at.string()?;
            let n_dims = at.u32()?;
            if n_dims == 0 || n_dims > 8 {
                return Err(GgufError::Invalid(format!("{name}: {n_dims} dimensions")));
            }
            let dims = (0..n_dims)
                .map(|_| at.u64())
                .collect::<Result<Vec<_>, _>>()?;
            let ggml_type = GgmlType::from_id(at.u32()?);
            let relative = at.u64()?;
            described.push((name, dims, ggml_type, relative));
        }
        let data_offset = (at.at as u64).next_multiple_of(alignment);
        let data_len = file_len
            .checked_sub(data_offset)
            .ok_or_else(|| GgufError::Invalid("the file ends inside its header".into()))?;
        let mut tensors = Vec::with_capacity(described.len());
        for (name, dims, ggml_type, relative) in described {
            if relative % alignment != 0 {
                return Err(GgufError::Invalid(format!("{name}: unaligned offset")));
            }
            let numel = dims
                .iter()
                .try_fold(1_u64, |n, &d| n.checked_mul(d))
                .ok_or_else(|| GgufError::Invalid(format!("{name}: size overflows")))?;
            let len = match ggml_type.block() {
                None => None,
                Some((elements, bytes)) => {
                    if dims[0] % elements != 0 {
                        return Err(GgufError::Invalid(format!(
                            "{name}: rows of {} are not whole blocks of {elements}",
                            dims[0]
                        )));
                    }
                    let len = (numel / elements)
                        .checked_mul(bytes)
                        .ok_or_else(|| GgufError::Invalid(format!("{name}: size overflows")))?;
                    if relative.checked_add(len).is_none_or(|end| end > data_len) {
                        return Err(GgufError::Invalid(format!(
                            "{name}: past the end of the file"
                        )));
                    }
                    Some(len)
                }
            };
            tensors.push(Tensor {
                name,
                dims,
                ggml_type,
                offset: data_offset + relative,
                len,
            });
        }
        Ok(Self {
            version,
            metadata,
            tensors,
            data_offset,
            alignment,
        })
    }

    /// Reads and parses the header of the file `reader` serves.
    pub fn read<P: loadngo_proactor::IoPort>(reader: &FileReader<P>) -> Result<Self, GgufError> {
        Self::read_from(reader, FIRST_READ)
    }

    fn read_from<P: loadngo_proactor::IoPort>(
        reader: &FileReader<P>,
        first: usize,
    ) -> Result<Self, GgufError> {
        let file_len = std::fs::metadata(reader.path())
            .map_err(|source| {
                GgufError::Read(ReadError::Open {
                    path: reader.path().to_owned(),
                    source,
                })
            })?
            .len();
        let mut want = first;
        loop {
            let take = usize::try_from(file_len).map_or(want, |len| want.min(len));
            let prefix = reader
                .read_ranges(&[("GGUF header", 0, take)])?
                .pop()
                .expect("one range");
            match Self::parse(&prefix, file_len) {
                Err(GgufError::Truncated(_)) if (take as u64) < file_len => want *= 2,
                result => return result,
            }
        }
    }

    pub fn tensor(&self, name: &str) -> Option<&Tensor> {
        self.tensors.iter().find(|t| t.name == name)
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.metadata.get(key)
    }
}

/// Opens `path` and parses its header; the reader serves the tensor bytes afterwards.
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly",
    target_os = "android",
    windows
))]
pub fn open(path: &Path) -> Result<(Gguf, FileReader<loadngo_proactor::PlatformPort>), GgufError> {
    let reader = FileReader::open(path)?;
    Ok((Gguf::read(&reader)?, reader))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Writes a GGUF v3 file: metadata entries (key, type, encoded value) and tensors
    /// (name, dims, ggml type id, bytes).
    pub(crate) fn file(
        metadata: &[(&str, u32, Vec<u8>)],
        tensors: &[(&str, &[u64], u32, Vec<u8>)],
        alignment: u64,
    ) -> Vec<u8> {
        let string = |s: &str| {
            let mut out = (s.len() as u64).to_le_bytes().to_vec();
            out.extend_from_slice(s.as_bytes());
            out
        };
        let mut out = b"GGUF".to_vec();
        out.extend_from_slice(&3_u32.to_le_bytes());
        out.extend_from_slice(&(tensors.len() as u64).to_le_bytes());
        out.extend_from_slice(&(metadata.len() as u64).to_le_bytes());
        for (key, kind, value) in metadata {
            out.extend(string(key));
            out.extend_from_slice(&kind.to_le_bytes());
            out.extend_from_slice(value);
        }
        let mut offset = 0_u64;
        let mut data = Vec::new();
        for (name, dims, kind, bytes) in tensors {
            out.extend(string(name));
            out.extend_from_slice(&(dims.len() as u32).to_le_bytes());
            for d in *dims {
                out.extend_from_slice(&d.to_le_bytes());
            }
            out.extend_from_slice(&kind.to_le_bytes());
            out.extend_from_slice(&offset.to_le_bytes());
            data.extend_from_slice(bytes);
            data.resize(data.len().next_multiple_of(alignment as usize), 0);
            offset = data.len() as u64;
        }
        out.resize(out.len().next_multiple_of(alignment as usize), 0);
        out.extend(data);
        out
    }

    pub(crate) fn string_value(s: &str) -> Vec<u8> {
        let mut out = (s.len() as u64).to_le_bytes().to_vec();
        out.extend_from_slice(s.as_bytes());
        out
    }

    fn sample() -> Vec<u8> {
        let mut strings = 8_u32.to_le_bytes().to_vec();
        strings.extend_from_slice(&2_u64.to_le_bytes());
        strings.extend(string_value("a b"));
        strings.extend(string_value("é"));
        file(
            &[
                ("general.architecture", 8, string_value("gpt-oss")),
                ("gpt-oss.block_count", 4, 24_u32.to_le_bytes().to_vec()),
                (
                    "gpt-oss.rope.freq_base",
                    6,
                    150_000_f32.to_le_bytes().to_vec(),
                ),
                ("tokenizer.ggml.merges", 9, strings),
            ],
            &[
                (
                    "norm",
                    &[3],
                    0,
                    [1.0_f32, 2.0, 3.0]
                        .iter()
                        .flat_map(|v| v.to_le_bytes())
                        .collect(),
                ),
                ("q", &[32, 2], 8, (0..68).map(|i| i as u8).collect()),
                ("odd", &[5], 2, vec![9; 18]),
            ],
            32,
        )
    }

    #[test]
    fn metadata_and_tensors_parse_with_absolute_aligned_offsets() {
        let bytes = sample();
        let gguf = Gguf::parse(&bytes, bytes.len() as u64).unwrap();
        assert_eq!(gguf.version, 3);
        assert_eq!(
            gguf.get("general.architecture").unwrap().as_str(),
            Some("gpt-oss")
        );
        assert_eq!(gguf.get("gpt-oss.block_count").unwrap().as_u64(), Some(24));
        assert_eq!(
            gguf.get("gpt-oss.rope.freq_base").unwrap().as_f64(),
            Some(150_000.0)
        );
        let merges = gguf
            .get("tokenizer.ggml.merges")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(merges[1].as_str(), Some("é"));
        assert_eq!(gguf.data_offset % 32, 0);
        let norm = gguf.tensor("norm").unwrap();
        assert_eq!((norm.ggml_type, norm.len), (GgmlType::F32, Some(12)));
        assert_eq!(&bytes[norm.offset as usize..][..4], &1.0_f32.to_le_bytes());
        let q = gguf.tensor("q").unwrap();
        assert_eq!(
            (q.ggml_type, q.len, q.numel()),
            (GgmlType::Q8_0, Some(68), 64)
        );
        assert_eq!(q.offset - gguf.data_offset, 32);
        assert_eq!(bytes[q.offset as usize + 67], 67);
        assert_eq!(
            gguf.tensor("odd").unwrap().len,
            None,
            "Q4_0 is not sized here"
        );
    }

    #[test]
    fn a_short_prefix_asks_for_more_and_bad_files_are_refused() {
        let bytes = sample();
        assert!(matches!(
            Gguf::parse(&bytes[..40], bytes.len() as u64),
            Err(GgufError::Truncated(40))
        ));
        let mut wrong = bytes.clone();
        wrong[0] = b'X';
        assert!(matches!(
            Gguf::parse(&wrong, bytes.len() as u64),
            Err(GgufError::Magic(_))
        ));
        let mut old = bytes.clone();
        old[4] = 1;
        assert!(matches!(
            Gguf::parse(&old, bytes.len() as u64),
            Err(GgufError::Version(1))
        ));
        // A file cut short inside the tensor data.
        let cut = bytes.len() as u64 - 70; // inside q, the last sized tensor
        assert!(matches!(
            Gguf::parse(&bytes, cut),
            Err(GgufError::Invalid(_))
        ));
    }

    #[test]
    fn a_file_is_read_through_the_proactor_in_growing_pieces() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.gguf");
        std::fs::write(&path, sample()).unwrap();
        let (gguf, reader) = open(&path).unwrap();
        let small = Gguf::read_from(&reader, 16).unwrap();
        assert_eq!(small.tensors, gguf.tensors);
        let q = gguf.tensor("q").unwrap();
        let bytes = reader
            .read_ranges(&[("q", q.offset, q.len.unwrap() as usize)])
            .unwrap();
        assert_eq!(bytes[0], (0..68).collect::<Vec<u8>>());
    }
}
