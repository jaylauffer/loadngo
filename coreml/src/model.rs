//! Minimal encoder of Apple's published Core ML protobuf schema (BSD-3-Clause).
//! Field numbers: coremltools/mlmodel/format/{Model,FeatureTypes,NeuralNetwork}.proto.
//! [`encode_dense`]: a 1x1 convolution with baked weights, Y[o,t] = sum_i W[o,i] X[i,t].
//! [`encode_dynamic_matmul`]: the weight is a runtime input, Y[r,o] = sum_i X[r,i] W[o,i],
//! so one compiled model serves every streamed matrix of the same shape.
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
    #[prost(message, optional, tag = "1045")]
    batched_matmul: Option<BatchedMatMul>,
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
