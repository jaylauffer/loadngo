//! Minimal encoder of Apple's published Core ML protobuf schema (BSD-3-Clause).
//! Field numbers: coremltools/mlmodel/format/{Model,FeatureTypes,NeuralNetwork}.proto.
//! A 1x1 convolution implements Y[o,t] = sum_i W[o,i] X[i,t]. No bias.
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
}
