//! Minimal encoder of Apple's published Core ML protobuf schema (BSD-3-Clause).
//! Field numbers: coremltools/mlmodel/format/{Model,FeatureTypes,NeuralNetwork}.proto.
//! [`encode_dense`]: a 1x1 convolution with baked weights, Y[o,t] = sum_i W[o,i] X[i,t].
//! [`encode_dynamic_matmul`]: the weight is a runtime input, Y[r,o] = sum_i X[r,i] W[o,i],
//! so one compiled model serves every streamed matrix of the same shape.
//! [`encode_grouped_attention`]: softmax(Q K^T + mask) V with grouped key/value heads,
//! keys, values and mask as runtime inputs.
use prost::Message;

#[derive(Clone, Copy, Debug)]
pub struct DenseShape {
    pub inputs: usize,
    pub outputs: usize,
    pub positions: usize,
}

impl DenseShape {
    pub fn input_shape(self) -> Vec<usize> {
        vec![1, self.inputs, 1, self.positions]
    }
    pub fn output_shape(self) -> Vec<usize> {
        vec![1, self.outputs, 1, self.positions]
    }
}

pub fn encode_dense(shape: DenseShape, weights: Vec<f32>) -> Result<Vec<u8>, String> {
    if shape.inputs == 0
        || shape.outputs == 0
        || shape.positions == 0
        || shape.inputs.checked_mul(shape.outputs) != Some(weights.len())
        || weights.len() > 64 * 1024 * 1024
        || shape.positions > 256
        || weights.iter().any(|x| !x.is_finite())
    {
        return Err("invalid/non-finite dense weights or probe bounds exceeded (64M weights, 256 positions)".into());
    }
    let feature = |name: &str, dims: Vec<usize>| Feature {
        name: name.into(),
        kind: Some(FeatureType {
            array: Some(Array {
                shape: dims.into_iter().map(|d| d as i64).collect(),
                dtype: 65568,
            }),
        }),
    };
    let model = Model {
        version: 4,
        description: Some(Description {
            inputs: vec![feature("x", shape.input_shape())],
            outputs: vec![feature("y", shape.output_shape())],
        }),
        network: Some(Network {
            layers: vec![Layer {
                name: "dense_projection".into(),
                inputs: vec!["x".into()],
                outputs: vec!["y".into()],
                convolution: Some(Conv {
                    outputs: shape.outputs as u64,
                    inputs: shape.inputs as u64,
                    groups: 1,
                    kernel: vec![1, 1],
                    stride: vec![1, 1],
                    valid: Some(Empty {}),
                    weights: Some(Weights { values: weights }),
                }),
                ..Layer::default()
            }],
            exact_shape: 1,
        }),
    };
    Ok(model.encode_to_vec())
}

/// Largest dimension the Neural Engine accepts for this layer on the M4 Pro (measured
/// 2026-09-24: 16384 is planned onto the ANE, 16385 falls back to the CPU). Callers tile.
pub const ANE_MAX_DIM: usize = 16384;

/// `ArrayFeatureType.ArrayDataType.FLOAT16` (Core ML specification version 7+).
const FLOAT16: i32 = 65552;

/// One dynamic-weight matmul: inputs `x [rows, inputs]` and `w [outputs, inputs]`, both
/// fp16, output `y [rows, outputs]` fp16, via `BatchedMatMul` with `transposeB`.
pub fn encode_dynamic_matmul(
    rows: usize,
    inputs: usize,
    outputs: usize,
) -> Result<Vec<u8>, String> {
    if rows == 0
        || inputs == 0
        || outputs == 0
        || rows > ANE_MAX_DIM
        || inputs > ANE_MAX_DIM
        || outputs > ANE_MAX_DIM
    {
        return Err(format!(
            "dynamic matmul {rows}x{inputs} -> {outputs} is outside 1..={ANE_MAX_DIM} per dimension"
        ));
    }
    let feature = |name: &str, dims: [usize; 2]| Feature {
        name: name.into(),
        kind: Some(FeatureType {
            array: Some(Array {
                shape: dims.iter().map(|&d| d as i64).collect(),
                dtype: FLOAT16,
            }),
        }),
    };
    let model = Model {
        version: 7,
        description: Some(Description {
            inputs: vec![
                feature("x", [rows, inputs]),
                feature("w", [outputs, inputs]),
            ],
            outputs: vec![feature("y", [rows, outputs])],
        }),
        network: Some(Network {
            layers: vec![Layer {
                name: "dynamic_matmul".into(),
                inputs: vec!["x".into(), "w".into()],
                outputs: vec!["y".into()],
                batched_matmul: Some(BatchedMatMul {
                    transpose_b: true,
                    ..BatchedMatMul::default()
                }),
                ..Layer::default()
            }],
            exact_shape: 1,
        }),
    };
    Ok(model.encode_to_vec())
}

/// The shape of one [`encode_grouped_attention`] model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GroupedAttentionShape {
    /// Key/value heads.
    pub kv_heads: usize,
    /// Query heads per key/value head.
    pub group: usize,
    /// Query positions per prediction.
    pub queries: usize,
    /// Key/value rows (cache slots) per prediction.
    pub keys: usize,
    /// Width of every head.
    pub dim: usize,
}

impl GroupedAttentionShape {
    /// Query heads.
    #[must_use]
    pub const fn heads(self) -> usize {
        self.kv_heads * self.group
    }
}

/// Grouped-query attention with runtime keys, values and an additive mask, scale 1:
/// `o = softmax(q k^T + mask) v` per query head, each key/value head serving `group`
/// query heads (head `h` uses key/value head `h / group`). Inputs and output are fp16,
/// two-dimensional (so they can be IOSurface-backed) and in a decoder's own row layout:
/// `q [queries][heads * dim]`, `k` and `v [keys][kv_heads * dim]`, `mask [queries][keys]`
/// (0 where a query sees a key, a large negative number where it does not), output
/// `o [queries][heads * dim]`. Inside, reshapes and transposes give
/// `[kv_heads, group, queries, dim]` and `[kv_heads, 1, keys, dim]`, so the batched
/// products broadcast each key/value head across its group, and the mask (as
/// `[1, 1, queries, keys]`) across every head. Every dimension stays within
/// [`ANE_MAX_DIM`].
///
/// # Errors
/// A zero dimension, or a row width or count past [`ANE_MAX_DIM`].
pub fn encode_grouped_attention(shape: GroupedAttentionShape) -> Result<Vec<u8>, String> {
    let GroupedAttentionShape {
        kv_heads,
        group,
        queries,
        keys,
        dim,
    } = shape;
    let heads = shape.heads();
    let dims = [kv_heads, group, queries, keys, dim, heads * dim];
    if dims.contains(&0) || dims.iter().any(|&d| d > ANE_MAX_DIM) {
        return Err(format!("unsupported grouped attention shape {shape:?}"));
    }
    let feature = |name: &str, d: [usize; 2]| Feature {
        name: name.into(),
        kind: Some(FeatureType {
            array: Some(Array {
                shape: d.iter().map(|&n| n as i64).collect(),
                dtype: FLOAT16,
            }),
        }),
    };
    let layer = |name: &str, inputs: &[&str]| Layer {
        name: name.into(),
        inputs: inputs.iter().map(|&s| s.to_string()).collect(),
        outputs: vec![name.into()],
        ..Layer::default()
    };
    let reshape = |name: &str, input: &str, to: &[usize]| Layer {
        reshape_static: Some(ReshapeStatic {
            target_shape: to.iter().map(|&n| n as i64).collect(),
        }),
        ..layer(name, &[input])
    };
    let transpose = |name: &str, input: &str, axes: [u64; 4]| Layer {
        transpose: Some(Transpose {
            axes: axes.to_vec(),
        }),
        ..layer(name, &[input])
    };
    let layers = vec![
        reshape("q_split", "q", &[queries, kv_heads, group, dim]),
        transpose("q_heads", "q_split", [1, 2, 0, 3]),
        reshape("k_split", "k", &[keys, kv_heads, 1, dim]),
        transpose("k_heads", "k_split", [1, 2, 0, 3]),
        reshape("v_split", "v", &[keys, kv_heads, 1, dim]),
        transpose("v_heads", "v_split", [1, 2, 0, 3]),
        reshape("mask_heads", "mask", &[1, 1, queries, keys]),
        Layer {
            batched_matmul: Some(BatchedMatMul {
                transpose_b: true,
                ..BatchedMatMul::default()
            }),
            ..layer("scores", &["q_heads", "k_heads"])
        },
        Layer {
            add_broadcastable: Some(Empty {}),
            ..layer("masked", &["scores", "mask_heads"])
        },
        Layer {
            softmax_nd: Some(SoftmaxNd { axis: -1 }),
            ..layer("weights", &["masked"])
        },
        Layer {
            batched_matmul: Some(BatchedMatMul::default()),
            ..layer("weighted", &["weights", "v_heads"])
        },
        transpose("o_rows", "weighted", [2, 0, 1, 3]),
        Layer {
            outputs: vec!["o".into()],
            ..reshape("o_join", "o_rows", &[queries, heads * dim])
        },
    ];
    let model = Model {
        version: 7,
        description: Some(Description {
            inputs: vec![
                feature("q", [queries, heads * dim]),
                feature("k", [keys, kv_heads * dim]),
                feature("v", [keys, kv_heads * dim]),
                feature("mask", [queries, keys]),
            ],
            outputs: vec![feature("o", [queries, heads * dim])],
        }),
        network: Some(Network {
            layers,
            exact_shape: 1,
        }),
    };
    Ok(model.encode_to_vec())
}

#[derive(Clone, PartialEq, Message)]
struct Model {
    #[prost(int32, tag = "1")]
    version: i32,
    #[prost(message, optional, tag = "2")]
    description: Option<Description>,
    #[prost(message, optional, tag = "500")]
    network: Option<Network>,
}
#[derive(Clone, PartialEq, Message)]
struct Description {
    #[prost(message, repeated, tag = "1")]
    inputs: Vec<Feature>,
    #[prost(message, repeated, tag = "10")]
    outputs: Vec<Feature>,
}
#[derive(Clone, PartialEq, Message)]
struct Feature {
    #[prost(string, tag = "1")]
    name: String,
    #[prost(message, optional, tag = "3")]
    kind: Option<FeatureType>,
}
#[derive(Clone, PartialEq, Message)]
struct FeatureType {
    #[prost(message, optional, tag = "5")]
    array: Option<Array>,
}
#[derive(Clone, PartialEq, Message)]
struct Array {
    #[prost(int64, repeated, tag = "1")]
    shape: Vec<i64>,
    #[prost(int32, tag = "2")]
    dtype: i32,
}
#[derive(Clone, PartialEq, Message)]
struct Network {
    #[prost(message, repeated, tag = "1")]
    layers: Vec<Layer>,
    #[prost(int32, tag = "5")]
    exact_shape: i32,
}
#[derive(Clone, PartialEq, Message)]
struct Layer {
    #[prost(string, tag = "1")]
    name: String,
    #[prost(string, repeated, tag = "2")]
    inputs: Vec<String>,
    #[prost(string, repeated, tag = "3")]
    outputs: Vec<String>,
    #[prost(message, optional, tag = "100")]
    convolution: Option<Conv>,
    #[prost(message, optional, tag = "880")]
    add_broadcastable: Option<Empty>,
    #[prost(message, optional, tag = "950")]
    softmax_nd: Option<SoftmaxNd>,
    #[prost(message, optional, tag = "985")]
    transpose: Option<Transpose>,
    #[prost(message, optional, tag = "1045")]
    batched_matmul: Option<BatchedMatMul>,
    #[prost(message, optional, tag = "1140")]
    reshape_static: Option<ReshapeStatic>,
}
#[derive(Clone, PartialEq, Message)]
struct SoftmaxNd {
    #[prost(int64, tag = "1")]
    axis: i64,
}
#[derive(Clone, PartialEq, Message)]
struct Transpose {
    #[prost(uint64, repeated, tag = "1")]
    axes: Vec<u64>,
}
#[derive(Clone, PartialEq, Message)]
struct ReshapeStatic {
    #[prost(int64, repeated, tag = "1")]
    target_shape: Vec<i64>,
}
#[derive(Clone, PartialEq, Message)]
struct BatchedMatMul {
    #[prost(bool, tag = "1")]
    transpose_a: bool,
    #[prost(bool, tag = "2")]
    transpose_b: bool,
}
#[derive(Clone, PartialEq, Message)]
struct Conv {
    #[prost(uint64, tag = "1")]
    outputs: u64,
    #[prost(uint64, tag = "2")]
    inputs: u64,
    #[prost(uint64, tag = "10")]
    groups: u64,
    #[prost(uint64, repeated, tag = "20")]
    kernel: Vec<u64>,
    #[prost(uint64, repeated, tag = "30")]
    stride: Vec<u64>,
    #[prost(message, optional, tag = "50")]
    valid: Option<Empty>,
    #[prost(message, optional, tag = "90")]
    weights: Option<Weights>,
}
#[derive(Clone, PartialEq, Message)]
struct Empty {}
#[derive(Clone, PartialEq, Message)]
struct Weights {
    #[prost(float, repeated, tag = "1")]
    values: Vec<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_projection_shape_and_values_round_trip() {
        let shape = DenseShape {
            inputs: 2,
            outputs: 3,
            positions: 4,
        };
        let bytes = encode_dense(shape, vec![0.25; 6]).unwrap();
        let decoded = Model::decode(bytes.as_slice()).unwrap();
        assert_eq!(
            decoded.description.unwrap().inputs[0]
                .kind
                .as_ref()
                .unwrap()
                .array
                .as_ref()
                .unwrap()
                .shape,
            [1, 2, 1, 4]
        );
        assert_eq!(
            decoded.network.unwrap().layers[0]
                .convolution
                .as_ref()
                .unwrap()
                .weights
                .as_ref()
                .unwrap()
                .values,
            [0.25; 6]
        );
        assert!(encode_dense(shape, vec![]).is_err());
        assert!(encode_dense(shape, vec![f32::NAN; 6]).is_err());
    }

    #[test]
    fn grouped_attention_takes_decoder_rows_and_broadcasts_kv_heads() {
        let shape = GroupedAttentionShape {
            kv_heads: 16,
            group: 2,
            queries: 512,
            keys: 1536,
            dim: 256,
        };
        let decoded = Model::decode(encode_grouped_attention(shape).unwrap().as_slice()).unwrap();
        let description = decoded.description.unwrap();
        let shapes: Vec<(String, Vec<i64>)> = description
            .inputs
            .iter()
            .chain(&description.outputs)
            .map(|f| {
                let array = f.kind.as_ref().unwrap().array.as_ref().unwrap();
                assert_eq!(array.dtype, FLOAT16);
                (f.name.clone(), array.shape.clone())
            })
            .collect();
        assert_eq!(
            shapes,
            [
                ("q".to_string(), vec![512, 32 * 256]),
                ("k".to_string(), vec![1536, 16 * 256]),
                ("v".to_string(), vec![1536, 16 * 256]),
                ("mask".to_string(), vec![512, 1536]),
                ("o".to_string(), vec![512, 32 * 256]),
            ]
        );
        let layers = decoded.network.unwrap().layers;
        let by = |name: &str| layers.iter().find(|l| l.name == name).unwrap();
        assert_eq!(
            by("q_split").reshape_static.as_ref().unwrap().target_shape,
            [512, 16, 2, 256]
        );
        assert_eq!(by("k_heads").transpose.as_ref().unwrap().axes, [1, 2, 0, 3]);
        assert!(by("scores").batched_matmul.as_ref().unwrap().transpose_b);
        assert!(!by("weighted").batched_matmul.as_ref().unwrap().transpose_b);
        assert!(by("masked").add_broadcastable.is_some());
        assert_eq!(by("weights").softmax_nd.as_ref().unwrap().axis, -1);
        assert_eq!(by("o_rows").transpose.as_ref().unwrap().axes, [2, 0, 1, 3]);
        assert_eq!(layers.last().unwrap().outputs, ["o"]);
        // 32 heads of 512 is the widest row the Neural Engine takes; 33 is refused.
        let full = GroupedAttentionShape {
            kv_heads: 4,
            group: 8,
            queries: 1,
            keys: 1536,
            dim: 512,
        };
        assert!(encode_grouped_attention(full).is_ok());
        assert!(encode_grouped_attention(GroupedAttentionShape { dim: 528, ..full }).is_err());
        assert!(encode_grouped_attention(GroupedAttentionShape { dim: 0, ..shape }).is_err());
    }

    #[test]
    fn dynamic_matmul_declares_fp16_weight_input_and_transposed_b() {
        let decoded = Model::decode(encode_dynamic_matmul(8, 3, 5).unwrap().as_slice()).unwrap();
        let description = decoded.description.unwrap();
        let shapes: Vec<_> = description
            .inputs
            .iter()
            .chain(&description.outputs)
            .map(|f| {
                (
                    f.name.clone(),
                    f.kind.as_ref().unwrap().array.as_ref().unwrap().clone(),
                )
            })
            .collect();
        assert_eq!(shapes[0].0, "x");
        assert_eq!(shapes[0].1.shape, [8, 3]);
        assert_eq!(shapes[1].0, "w");
        assert_eq!(shapes[1].1.shape, [5, 3]);
        assert_eq!(shapes[2].1.shape, [8, 5]);
        assert!(shapes.iter().all(|(_, a)| a.dtype == FLOAT16));
        let layer = &decoded.network.unwrap().layers[0];
        assert!(layer.batched_matmul.as_ref().unwrap().transpose_b);
        assert!(layer.convolution.is_none());
        assert!(encode_dynamic_matmul(1, ANE_MAX_DIM + 1, 4).is_err());
        assert!(encode_dynamic_matmul(0, 4, 4).is_err());
    }
}
