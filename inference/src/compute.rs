//! Small accelerator contract, independent of any OS or model architecture.
//! Policies are permissions, not evidence of device execution.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComputePolicy {
    CpuOnly,
    CpuAndNpu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Cpu,
    Gpu,
    Npu,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct Placement {
    pub operation: String,
    /// Compiler/runtime plan only, not a measured hardware trace.
    pub preferred: DeviceKind,
    pub supported: Vec<DeviceKind>,
}

#[derive(Debug, Clone)]
pub struct Tensor {
    shape: Vec<usize>,
    values: Vec<f32>,
}

impl Tensor {
    pub fn new(shape: Vec<usize>, values: Vec<f32>) -> Result<Self, String> {
        let count = shape.iter().try_fold(1usize, |n, &d| n.checked_mul(d));
        if shape.is_empty() || shape.contains(&0) || count != Some(values.len()) {
            return Err("tensor shape does not match storage or overflows".into());
        }
        Ok(Self { shape, values })
    }
    pub fn shape(&self) -> &[usize] {
        &self.shape
    }
    pub fn values(&self) -> &[f32] {
        &self.values
    }
}

pub type PredictionCallback = Box<dyn FnOnce(Result<Tensor, String>) + Send>;

/// One prepared partition. Admission must be bounded (Busy rather than an
/// unbounded queue). Callback owns its result and may post it to a host proactor.
/// A successful submission requires exactly one callback, including failures.
/// The backend must retain model and input until completion even if dropped.
pub trait PreparedCompute {
    fn submit(&mut self, input: Tensor, done: PredictionCallback) -> Result<(), String>;
    fn is_busy(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_bad_shapes_without_allocating() {
        assert!(Tensor::new(vec![usize::MAX, 2], vec![]).is_err());
        assert!(Tensor::new(vec![0], vec![]).is_err());
        assert!(Tensor::new(vec![2], vec![0.0]).is_err());
        assert_eq!(
            Tensor::new(vec![1, 2], vec![1.0, 2.0]).unwrap().values(),
            [1.0, 2.0]
        );
    }
}
