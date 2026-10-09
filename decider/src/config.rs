//! The two configuration files a decider needs: the base model's `config.json` (its text
//! tower) and the checkpoint's `strands_decider_config.json`.

use std::{collections::HashMap, path::Path};

use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("reading {0}: {1}")]
    Read(String, std::io::Error),
    #[error("{0}: {1}")]
    Invalid(String, String),
}

/// One layer's token mixer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerKind {
    /// Gated DeltaNet: linear attention with a recurrent state.
    Linear,
    /// Gated softmax attention over every earlier position.
    Full,
}

/// The Qwen3.5 text tower.
#[derive(Clone, Debug, PartialEq)]
pub struct TorsoConfig {
    pub vocab: usize,
    pub hidden: usize,
    pub intermediate: usize,
    pub layers: Vec<LayerKind>,
    pub rms_eps: f32,
    /// Full attention.
    pub heads: usize,
    pub kv_heads: usize,
    pub head_dim: usize,
    /// Dimensions of each head that RoPE rotates (from `partial_rotary_factor`).
    pub rotary_dim: usize,
    pub rope_theta: f64,
    /// Gated DeltaNet.
    pub key_heads: usize,
    pub value_heads: usize,
    pub key_dim: usize,
    pub value_dim: usize,
    pub conv_kernel: usize,
}

/// The decider's own settings.
#[derive(Clone, Debug, PartialEq)]
pub struct DeciderConfig {
    /// The window, in tokens.
    pub max_length: usize,
    /// Width of the pointer head's query and key.
    pub pointer_dim: usize,
    /// Fitted temperature by question kind (`noul`, `choice`, `score`), and the fallback.
    pub temperature: f32,
    pub temperature_by_kind: HashMap<String, f32>,
    /// LoRA rank and scale numerator; the adapter adds `alpha / r * B A`.
    pub lora_r: usize,
    pub lora_alpha: f32,
    /// The torso was trained and is served in bfloat16.
    pub bf16: bool,
}

fn read(path: &Path) -> Result<Value, ConfigError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| ConfigError::Read(path.display().to_string(), e))?;
    serde_json::from_str(&text)
        .map_err(|e| ConfigError::Invalid(path.display().to_string(), e.to_string()))
}

impl TorsoConfig {
    /// From a base model's `config.json` (its `text_config`, or the file itself).
    ///
    /// # Errors
    /// When the file is unreadable or is not a Qwen3.5 text tower this crate implements.
    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        let json = read(path)?;
        let t = json.get("text_config").unwrap_or(&json);
        let invalid =
            |what: &str| ConfigError::Invalid(path.display().to_string(), what.to_owned());
        let int = |key: &str| {
            t[key]
                .as_u64()
                .and_then(|v| usize::try_from(v).ok())
                .ok_or_else(|| invalid(&format!("`{key}` is missing")))
        };
        let layers = t["layer_types"]
            .as_array()
            .ok_or_else(|| invalid("`layer_types` is missing"))?
            .iter()
            .map(|k| match k.as_str() {
                Some("linear_attention") => Ok(LayerKind::Linear),
                Some("full_attention") => Ok(LayerKind::Full),
                other => Err(invalid(&format!("unknown layer type {other:?}"))),
            })
            .collect::<Result<Vec<_>, _>>()?;
        if layers.len() != int("num_hidden_layers")? {
            return Err(invalid("`layer_types` does not list every layer"));
        }
        if t["attn_output_gate"].as_bool() == Some(false) {
            return Err(invalid(
                "attention without its output gate is not implemented",
            ));
        }
        if t["attention_bias"].as_bool() == Some(true) {
            return Err(invalid("attention biases are not implemented"));
        }
        if t["hidden_act"].as_str().is_some_and(|a| a != "silu") {
            return Err(invalid("only the silu activation is implemented"));
        }
        let rope = &t["rope_parameters"];
        if rope["rope_type"].as_str().is_some_and(|r| r != "default") {
            return Err(invalid("only default RoPE is implemented"));
        }
        let head_dim = int("head_dim")?;
        let partial = rope["partial_rotary_factor"].as_f64().unwrap_or(1.0);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let rotary_dim = (head_dim as f64 * partial) as usize;
        let config = Self {
            vocab: int("vocab_size")?,
            hidden: int("hidden_size")?,
            intermediate: int("intermediate_size")?,
            layers,
            #[allow(clippy::cast_possible_truncation)]
            rms_eps: t["rms_norm_eps"].as_f64().unwrap_or(1e-6) as f32,
            heads: int("num_attention_heads")?,
            kv_heads: int("num_key_value_heads")?,
            head_dim,
            rotary_dim,
            rope_theta: rope["rope_theta"]
                .as_f64()
                .ok_or_else(|| invalid("`rope_theta` is missing"))?,
            key_heads: int("linear_num_key_heads")?,
            value_heads: int("linear_num_value_heads")?,
            key_dim: int("linear_key_head_dim")?,
            value_dim: int("linear_value_head_dim")?,
            conv_kernel: int("linear_conv_kernel_dim")?,
        };
        if !config.heads.is_multiple_of(config.kv_heads)
            || !config.value_heads.is_multiple_of(config.key_heads)
        {
            return Err(invalid("head counts do not divide"));
        }
        if !config.rotary_dim.is_multiple_of(2) || config.rotary_dim > head_dim {
            return Err(invalid("the rotary dimension is odd or too large"));
        }
        Ok(config)
    }
}

impl DeciderConfig {
    /// From a checkpoint's `strands_decider_config.json`.
    ///
    /// # Errors
    /// When the file is unreadable or describes a head this crate does not implement.
    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        let json = read(path)?;
        let invalid =
            |what: &str| ConfigError::Invalid(path.display().to_string(), what.to_owned());
        if json["head_type"].as_str() != Some("pointer") {
            return Err(invalid("only the pointer head is implemented"));
        }
        if json["use_lora"].as_bool() != Some(true) {
            return Err(invalid(
                "a checkpoint without its LoRA adapter is not implemented",
            ));
        }
        let int = |key: &str| {
            json[key]
                .as_u64()
                .and_then(|v| usize::try_from(v).ok())
                .ok_or_else(|| invalid(&format!("`{key}` is missing")))
        };
        #[allow(clippy::cast_possible_truncation)]
        let float = |v: &Value| v.as_f64().map(|f| f as f32);
        Ok(Self {
            max_length: int("max_length")?,
            pointer_dim: int("pointer_dim")?,
            temperature: float(&json["temperature"]).unwrap_or(1.0),
            temperature_by_kind: json["temperature_by_kind"]
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(|(k, v)| Some((k.clone(), float(v)?)))
                .collect(),
            lora_r: int("lora_r")?,
            lora_alpha: float(&json["lora_alpha"])
                .ok_or_else(|| invalid("`lora_alpha` is missing"))?,
            bf16: json["torch_dtype"].as_str() == Some("bfloat16"),
        })
    }

    /// The temperature for a question of `kind`.
    #[must_use]
    pub fn temperature(&self, kind: &str) -> f32 {
        self.temperature_by_kind
            .get(kind)
            .copied()
            .unwrap_or(self.temperature)
    }
}
