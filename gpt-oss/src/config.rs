//! A gpt-oss model's shape, from its GGUF metadata (`gpt-oss.*`).

use loadngo_weights::gguf::{Gguf, Value};

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub layers: usize,
    pub hidden: usize,
    pub heads: usize,
    pub kv_heads: usize,
    pub head_dim: usize,
    pub experts: usize,
    pub experts_used: usize,
    /// Width of each expert's hidden layer.
    pub expert_hidden: usize,
    pub vocab: usize,
    /// Keys a sliding layer's query sees, itself included.
    pub sliding_window: usize,
    pub rms_eps: f32,
    pub rope_theta: f64,
    /// YaRN: the context the model was trained at, and how far it is stretched.
    pub rope_original_context: f64,
    pub rope_factor: f64,
    pub context_length: usize,
}

/// YaRN's ramp bounds, in rotations over the original context (OpenAI's `ntk_beta` and
/// `ntk_alpha`; transformers' `beta_fast` and `beta_slow`). The GGUF does not record
/// them; these are gpt-oss's published values.
const YARN_BETA_FAST: f64 = 32.0;
const YARN_BETA_SLOW: f64 = 1.0;

#[derive(Debug, thiserror::Error)]
#[error("gpt-oss GGUF: {0}")]
pub struct ConfigError(pub String);

impl Config {
    pub fn from_gguf(gguf: &Gguf) -> Result<Self, ConfigError> {
        let get = |key: &str| {
            gguf.get(&format!("gpt-oss.{key}"))
                .ok_or_else(|| ConfigError(format!("gpt-oss.{key} is missing")))
        };
        let int = |key: &str| -> Result<usize, ConfigError> {
            get(key)?
                .as_u64()
                .and_then(|v| usize::try_from(v).ok())
                .ok_or_else(|| ConfigError(format!("gpt-oss.{key} is not an integer")))
        };
        let float = |key: &str| -> Result<f64, ConfigError> {
            get(key)?
                .as_f64()
                .ok_or_else(|| ConfigError(format!("gpt-oss.{key} is not a float")))
        };
        if gguf.get("general.architecture").and_then(Value::as_str) != Some("gpt-oss") {
            return Err(ConfigError("general.architecture is not gpt-oss".into()));
        }
        if gguf
            .get("gpt-oss.rope.scaling.type")
            .and_then(Value::as_str)
            != Some("yarn")
        {
            return Err(ConfigError(
                "only YaRN rotary scaling is implemented".into(),
            ));
        }
        let head_dim = int("attention.key_length")?;
        if int("attention.value_length")? != head_dim || !head_dim.is_multiple_of(2) {
            return Err(ConfigError(
                "keys and values must share an even width".into(),
            ));
        }
        let vocab = gguf
            .tensor("token_embd.weight")
            .and_then(|t| t.dims.get(1).copied())
            .ok_or_else(|| ConfigError("token_embd.weight is missing".into()))?;
        let config = Self {
            layers: int("block_count")?,
            hidden: int("embedding_length")?,
            heads: int("attention.head_count")?,
            kv_heads: int("attention.head_count_kv")?,
            head_dim,
            experts: int("expert_count")?,
            experts_used: int("expert_used_count")?,
            expert_hidden: int("expert_feed_forward_length")?,
            vocab: usize::try_from(vocab).map_err(|_| ConfigError("vocabulary size".into()))?,
            sliding_window: int("attention.sliding_window")?,
            rms_eps: float("attention.layer_norm_rms_epsilon")? as f32,
            rope_theta: float("rope.freq_base")?,
            rope_original_context: int("rope.scaling.original_context_length")? as f64,
            rope_factor: float("rope.scaling.factor")?,
            context_length: int("context_length")?,
        };
        if !config.heads.is_multiple_of(config.kv_heads) || config.experts_used > config.experts {
            return Err(ConfigError("inconsistent head or expert counts".into()));
        }
        Ok(config)
    }

    /// gpt-oss alternates attention: even layers see a sliding window, odd layers all
    /// positions.
    pub fn is_sliding(&self, layer: usize) -> bool {
        layer.is_multiple_of(2)
    }

    /// YaRN's inverse frequencies, one per rotated pair, and the factor applied to every
    /// cosine and sine (`0.1 ln(factor) + 1`), as in OpenAI's reference implementation
    /// and transformers' `yarn` with `truncate = false`.
    pub fn rope(&self) -> (Vec<f64>, f64) {
        let dim = self.head_dim as f64;
        let half = self.head_dim / 2;
        let correction = |rotations: f64| {
            dim * (self.rope_original_context / (rotations * 2.0 * std::f64::consts::PI)).ln()
                / (2.0 * self.rope_theta.ln())
        };
        let low = correction(YARN_BETA_FAST).max(0.0);
        let high = correction(YARN_BETA_SLOW).min(dim - 1.0);
        let high = if low == high { high + 0.001 } else { high };
        let inv_freq = (0..half)
            .map(|i| {
                let freq = self.rope_theta.powf(2.0 * i as f64 / dim);
                let ramp = ((i as f64 - low) / (high - low)).clamp(0.0, 1.0);
                let extrapolation = 1.0 - ramp;
                (1.0 / (self.rope_factor * freq)) * (1.0 - extrapolation)
                    + (1.0 / freq) * extrapolation
            })
            .collect();
        let scale = if self.rope_factor <= 1.0 {
            1.0
        } else {
            0.1 * self.rope_factor.ln() + 1.0
        };
        (inv_freq, scale)
    }
}
