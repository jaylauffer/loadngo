//! The pointer head: a layer norm, then a query from the hidden state at `<answer>` and a
//! key from the hidden state at the end of each option's line; an option's score is
//! their scaled dot product. It holds no parameter per option, so options are scored by
//! what they say, not where they stand, and a question may have any number of them.

use std::path::Path;

use crate::model::{LoadError, Weights};

/// `torch.nn.LayerNorm`'s default epsilon.
const LAYER_NORM_EPS: f32 = 1e-5;

pub struct Head {
    hidden: usize,
    dim: usize,
    norm_w: Vec<f32>,
    norm_b: Vec<f32>,
    q_w: Vec<f32>,
    q_b: Vec<f32>,
    k_w: Vec<f32>,
    k_b: Vec<f32>,
}

impl Head {
    /// Reads `norm`, `q` and `k` from the safetensors files in `dir`.
    ///
    /// # Errors
    /// When the tensors are missing or have the wrong shape.
    pub fn load(dir: &Path, hidden: usize, dim: usize) -> Result<Self, LoadError> {
        let weights = Weights::open(dir)?;
        let mut t = weights.read(&[
            ("norm.weight", hidden),
            ("norm.bias", hidden),
            ("q.weight", dim * hidden),
            ("q.bias", dim),
            ("k.weight", dim * hidden),
            ("k.bias", dim),
        ])?;
        let k_b = t.pop().expect("six");
        let k_w = t.pop().expect("six");
        let q_b = t.pop().expect("six");
        let q_w = t.pop().expect("six");
        let norm_b = t.pop().expect("six");
        let norm_w = t.pop().expect("six");
        Ok(Self {
            hidden,
            dim,
            norm_w,
            norm_b,
            q_w,
            q_b,
            k_w,
            k_b,
        })
    }

    fn project(&self, h: &[f32], w: &[f32], b: &[f32]) -> Vec<f32> {
        #[allow(clippy::cast_precision_loss)]
        let n = self.hidden as f32;
        let mean = h.iter().sum::<f32>() / n;
        let var = h.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n;
        let inv = 1.0 / (var + LAYER_NORM_EPS).sqrt();
        let normed: Vec<f32> = h
            .iter()
            .zip(self.norm_w.iter().zip(&self.norm_b))
            .map(|(v, (w, b))| (v - mean) * inv * w + b)
            .collect();
        (0..self.dim)
            .map(|r| crate::math::dot(&normed, &w[r * self.hidden..(r + 1) * self.hidden]) + b[r])
            .collect()
    }

    /// Each option's score, before temperature.
    #[must_use]
    pub fn logits(&self, answer: &[f32], options: &[&[f32]]) -> Vec<f32> {
        let q = self.project(answer, &self.q_w, &self.q_b);
        #[allow(clippy::cast_precision_loss)]
        let scale = 1.0 / (self.dim as f32).sqrt();
        options
            .iter()
            .map(|o| crate::math::dot(&self.project(o, &self.k_w, &self.k_b), &q) * scale)
            .collect()
    }
}
