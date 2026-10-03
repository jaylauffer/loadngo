//! The GGUF reader and decoders against gpt-oss-20b's real file, row by row, with ggml's
//! own Python reader as the reference (`scripts/gguf_reference_rows.py`).

use std::path::PathBuf;

use loadngo_weights::{
    gguf::{self, GgmlType},
    mxfp4::{ggml_to_ocp, Mxfp4Matrix},
    q8_0::Q8Matrix,
};
use serde_json::Value;

fn model() -> PathBuf {
    std::env::var_os("GPT_OSS_GGUF").map_or_else(
        || {
            PathBuf::from(std::env::var_os("HOME").unwrap())
                .join(".loadngo/models/56fcc05caeabd1f4f352f7b9d6762cad2035b860973c07a5c330a7fa3944e8e1.gguf")
        },
        PathBuf::from,
    )
}

#[test]
#[ignore = "needs gpt-oss-20b's GGUF (~/.loadngo/models or GPT_OSS_GGUF)"]
fn rows_decode_as_ggml_decodes_them() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/gpt-oss-20b-rows.json")).unwrap();
    let (file, reader) = gguf::open(&model()).unwrap();
    assert_eq!(file.version, 3);
    assert_eq!(
        file.tensors.len() as u64,
        fixture["header"]["tensor_count"].as_u64().unwrap()
    );
    assert_eq!(
        file.get("general.architecture").and_then(|v| v.as_str()),
        Some("gpt-oss")
    );
    let mut checked = 0;
    for entry in fixture["rows"].as_array().unwrap() {
        let name = entry["name"].as_str().unwrap();
        let tensor = file.tensor(name).unwrap();
        let dims: Vec<u64> = entry["dims"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d.as_u64().unwrap())
            .collect();
        assert_eq!(tensor.dims, dims, "{name}");
        assert_eq!(tensor.offset, entry["offset"].as_u64().unwrap(), "{name}");
        let bytes = reader
            .read_ranges(&[(name, tensor.offset, tensor.len.unwrap() as usize)])
            .unwrap()
            .pop()
            .unwrap();
        let cols = dims[0] as usize;
        let rows = (tensor.numel() / dims[0]) as usize;
        // Every row of the tensor, matrices of further dimensions flattened: [e][r] is
        // row e * dims[1] + r.
        let row = |index: usize| -> Vec<f32> {
            let mut out = vec![0.0; cols];
            match tensor.ggml_type {
                GgmlType::F32 => {
                    for (o, b) in out
                        .iter_mut()
                        .zip(bytes[index * cols * 4..].as_chunks::<4>().0)
                    {
                        *o = f32::from_le_bytes(*b);
                    }
                }
                GgmlType::Q8_0 => Q8Matrix::new(&bytes, rows, cols)
                    .unwrap()
                    .dequantize_row(index, &mut out),
                GgmlType::Mxfp4 => {
                    let per_row = cols / 32 * 17;
                    let (elements, scales) =
                        ggml_to_ocp(&bytes[index * per_row..][..per_row], 1, cols);
                    Mxfp4Matrix::new(&elements, &scales, 1, cols)
                        .unwrap()
                        .dequantize_row(0, &mut out);
                }
                other => panic!("{name}: {other:?}"),
            }
            out
        };
        let want = |values: &Value| -> Vec<f32> {
            values
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_f64().unwrap() as f32)
                .collect()
        };
        if let Some(values) = entry.get("values") {
            assert_eq!(row(0), want(values), "{name}");
            checked += 1;
            continue;
        }
        for picked in entry["rows"].as_array().unwrap() {
            let index = match &picked["index"] {
                Value::Number(n) => n.as_u64().unwrap() as usize,
                pair => {
                    let pair = pair.as_array().unwrap();
                    pair[0].as_u64().unwrap() as usize * dims[1] as usize
                        + pair[1].as_u64().unwrap() as usize
                }
            };
            assert_eq!(row(index), want(&picked["values"]), "{name} row {index}");
            checked += 1;
        }
    }
    assert_eq!(checked, 9);
}
