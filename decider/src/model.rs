//! The Qwen3.5 text tower on the CPU, in `f32`, with the decider's LoRA adapter merged
//! into its weights.
//!
//! Written from the architecture (Qwen3.5's model card and configuration, the Gated
//! DeltaNet paper) and checked against transformers' implementation on a random model at
//! toy size (`tests/tiny_oracle.rs`). Each layer is
//!
//! ```text
//! x += mixer(norm(x));  x += mlp(norm(x))
//! ```
//!
//! with zero-centred RMS norms (`x / rms(x) * (1 + w)`), a SiLU-gated MLP, and one of
//! two token mixers:
//!
//! - **Gated DeltaNet** (linear attention). Project to `q k v` (one matrix), run a causal
//!   depthwise convolution over the last `kernel` positions and SiLU, L2-normalize `q` and
//!   `k` and scale `q` by `1/sqrt(d)`; per value head, a `d_k x d_v` state `S` decays by
//!   `exp(g)` with `g = -exp(A_log) softplus(a + dt_bias)`, takes the delta-rule update
//!   `S += k (beta (v - S^T k))^T` with `beta = sigmoid(b)`, and reads `S^T q`. The read
//!   is RMS-normalized, scaled by its weight and gated by `silu(z)`.
//! - **Gated attention.** Softmax attention with grouped key/value heads; `q` and `k`
//!   are RMS-normalized per head and rotated (RoPE on the first `rotary_dim` dimensions
//!   of each head); the output is gated by `sigmoid` of a second half of the query
//!   projection.
//!
//! A [`Session`] holds what the layers carry forward: each DeltaNet layer's state and
//! convolution history, each attention layer's keys and values. Reading a request's
//! state once and then each question from a copy of that session gives the same hidden
//! states as reading every prompt whole.

use std::path::Path;

use loadngo_weights::{
    dtype::{round_to_bf16, widen_to_f32, Dtype},
    reader::TensorReader,
    safetensors::TensorInfo,
    shards::ShardSet,
};

use crate::{
    config::{LayerKind, TorsoConfig},
    math::{
        dot, matmul, par_chunks, rms_norm_centred, rms_normalize, sigmoid, silu, softmax, softplus,
    },
};

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("{0}")]
    Config(#[from] crate::config::ConfigError),
    #[error("opening the weights in {0}: {1}")]
    Shards(String, String),
    #[error("reading weights: {0}")]
    Read(String),
    #[error("tensor {0}: {1}")]
    Tensor(String, String),
}

/// A row-major `rows x cols` matrix, and the adapter's low-rank update when it is kept
/// apart from the weights instead of merged into them.
pub(crate) struct Linear {
    pub(crate) w: Vec<f32>,
    pub(crate) rows: usize,
    pub(crate) cols: usize,
    pub(crate) lora: Option<Lora>,
}

/// `scale * B A`, unmerged: `a` is `r x cols`, `b` is `rows x r`.
pub(crate) struct Lora {
    pub(crate) a: Vec<f32>,
    pub(crate) b: Vec<f32>,
    pub(crate) r: usize,
    pub(crate) scale: f32,
}

impl Linear {
    fn apply(&self, x: &[f32], t: usize) -> Vec<f32> {
        let mut y = matmul(x, t, &self.w, self.rows, self.cols);
        if let Some(l) = &self.lora {
            let ax = matmul(x, t, &l.a, l.r, self.cols);
            let bax = matmul(&ax, t, &l.b, self.rows, l.r);
            for (y, d) in y.iter_mut().zip(&bax) {
                *y += l.scale * d;
            }
        }
        y
    }
}

pub(crate) struct DeltaNet {
    pub(crate) qkv: Linear,
    pub(crate) z: Linear,
    pub(crate) b: Linear,
    pub(crate) a: Linear,
    /// `channels x kernel`.
    pub(crate) conv: Vec<f32>,
    /// `A_log` per value head, as loaded (rounded to bfloat16 with `bf16`); the decay
    /// rate is `-exp(A_log)`.
    pub(crate) a_log: Vec<f32>,
    pub(crate) dt_bias: Vec<f32>,
    pub(crate) norm: Vec<f32>,
    pub(crate) out: Linear,
}

pub(crate) struct Attention {
    pub(crate) q: Linear,
    pub(crate) k: Linear,
    pub(crate) v: Linear,
    pub(crate) o: Linear,
    pub(crate) q_norm: Vec<f32>,
    pub(crate) k_norm: Vec<f32>,
}

pub(crate) enum Mixer {
    Delta(DeltaNet),
    Attention(Attention),
}

pub(crate) struct Layer {
    pub(crate) input_norm: Vec<f32>,
    pub(crate) post_norm: Vec<f32>,
    pub(crate) mixer: Mixer,
    pub(crate) gate: Linear,
    pub(crate) up: Linear,
    pub(crate) down: Linear,
}

/// The token embedding, kept in its file encoding and widened a row at a time.
struct Embedding {
    bytes: Vec<u8>,
    dtype: Dtype,
    hidden: usize,
}

/// What one layer carries from position to position.
#[derive(Clone, Debug)]
enum LayerState {
    Delta {
        /// The last `kernel - 1` convolution inputs, oldest first.
        conv: Vec<f32>,
        /// `value_heads x key_dim x value_dim`.
        s: Vec<f32>,
    },
    Attention {
        /// `positions x kv_heads x head_dim`.
        k: Vec<f32>,
        v: Vec<f32>,
    },
}

/// The torso's memory of what it has read.
#[derive(Clone, Debug)]
pub struct Session {
    len: usize,
    layers: Vec<LayerState>,
}

impl Session {
    /// Positions read so far.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

pub struct Model {
    pub config: TorsoConfig,
    embed: Embedding,
    pub(crate) layers: Vec<Layer>,
    pub(crate) norm: Vec<f32>,
    /// RoPE inverse frequencies, as transformers computes them in `f32`.
    pub(crate) inv_freq: Vec<f32>,
}

/// A LoRA adapter to apply: its directory, `alpha / r`, and whether to merge it into the
/// weights while loading or keep it apart (as the GPU path does, whose weights stay
/// bfloat16: merged there, the adapter's update would be rounded with them).
pub struct Adapter<'a> {
    pub dir: &'a Path,
    pub scale: f32,
    pub merge: bool,
}

pub(crate) struct Weights {
    reader: TensorReader<loadngo_proactor::PlatformPort>,
    set_dir: String,
    /// Round tensors stored in `f32` to bfloat16, as a model loaded in bfloat16 holds them.
    bf16: bool,
}

impl Weights {
    pub(crate) fn open(dir: &Path) -> Result<Self, LoadError> {
        let shards = ShardSet::open(dir)
            .map_err(|e| LoadError::Shards(dir.display().to_string(), e.to_string()))?;
        let reader = TensorReader::open(shards)
            .map_err(|e| LoadError::Shards(dir.display().to_string(), e.to_string()))?;
        Ok(Self {
            reader,
            set_dir: dir.display().to_string(),
            bf16: false,
        })
    }

    fn info(&self, name: &str) -> Result<TensorInfo, LoadError> {
        self.reader
            .shards()
            .locate(name)
            .map(|(_, info)| info.clone())
            .ok_or_else(|| LoadError::Tensor(name.to_owned(), format!("not in {}", self.set_dir)))
    }

    fn has(&self, name: &str) -> bool {
        self.reader.shards().locate(name).is_some()
    }

    /// Tensors widened to `f32`, each checked against its expected element count.
    pub(crate) fn read(&self, wanted: &[(&str, usize)]) -> Result<Vec<Vec<f32>>, LoadError> {
        let infos = wanted
            .iter()
            .map(|(name, _)| self.info(name))
            .collect::<Result<Vec<_>, _>>()?;
        let names: Vec<&str> = wanted.iter().map(|(n, _)| *n).collect();
        let bytes = self
            .reader
            .read_tensors(&names)
            .map_err(|e| LoadError::Read(e.to_string()))?;
        infos
            .iter()
            .zip(bytes)
            .zip(wanted)
            .map(|((info, bytes), (name, len))| {
                let mut values = widen_to_f32(info.dtype, &bytes)
                    .map_err(|e| LoadError::Tensor((*name).to_owned(), e.to_string()))?;
                if self.bf16 && info.dtype == Dtype::F32 {
                    for v in &mut values {
                        *v = round_to_bf16(*v);
                    }
                }
                if values.len() != *len {
                    return Err(LoadError::Tensor(
                        (*name).to_owned(),
                        format!("{} elements, expected {len}", values.len()),
                    ));
                }
                Ok(values)
            })
            .collect()
    }
}

impl Model {
    /// Loads the text tower from a base model directory (`config.json` and safetensors),
    /// merging `adapter` into its projections. With `bf16`, the tower's weights are what a
    /// model loaded in bfloat16 holds: the few tensors its files store in `f32` (each
    /// DeltaNet layer's `A_log` and gated-norm weight) are rounded to bfloat16. The decider
    /// was trained and is served that way; keeping their `f32` values moved its
    /// probabilities by up to 2e-3. The adapter stays in `f32`, as PEFT keeps it.
    ///
    /// # Errors
    /// When a file is missing or malformed, or a tensor has the wrong shape.
    pub fn load(base: &Path, adapter: Option<Adapter<'_>>, bf16: bool) -> Result<Self, LoadError> {
        let config = TorsoConfig::from_file(&base.join("config.json"))?;
        let mut weights = Weights::open(base)?;
        weights.bf16 = bf16;
        let prefix = if weights.has("model.language_model.norm.weight") {
            "model.language_model."
        } else {
            "model."
        };
        let lora = adapter
            .map(|a| Weights::open(a.dir).map(|w| (w, a.scale, a.merge)))
            .transpose()?;
        let c = &config;
        let embed_name = format!("{prefix}embed_tokens.weight");
        let embed_info = weights.info(&embed_name)?;
        if embed_info.shape != [c.vocab as u64, c.hidden as u64] {
            return Err(LoadError::Tensor(
                embed_name,
                format!("shape {:?}", embed_info.shape),
            ));
        }
        let embed_bytes = weights
            .reader
            .read_tensors(&[embed_name.as_str()])
            .map_err(|e| LoadError::Read(e.to_string()))?
            .remove(0);
        let embed = Embedding {
            bytes: embed_bytes,
            dtype: embed_info.dtype,
            hidden: c.hidden,
        };

        let mut layers = Vec::with_capacity(c.layers.len());
        for (i, kind) in c.layers.iter().enumerate() {
            let p = format!("{prefix}layers.{i}.");
            let linear =
                |name: &str, rows: usize, cols: usize, w: Vec<f32>| -> Result<Linear, LoadError> {
                    let mut w = w;
                    let mut kept = None;
                    if let Some((lora, scale, merge)) = &lora {
                        let module = format!("base_model.model.layers.{i}.{name}");
                        let (a, b, r) = read_lora(lora, rows, cols, &module)?;
                        if *merge {
                            merge_lora(&mut w, cols, (&a, &b, r), *scale);
                        } else {
                            kept = Some(Lora {
                                a,
                                b,
                                r,
                                scale: *scale,
                            });
                        }
                    }
                    Ok(Linear {
                        w,
                        rows,
                        cols,
                        lora: kept,
                    })
                };
            let (h, f) = (c.hidden, c.intermediate);
            let mut shared = weights.read(&[
                (&format!("{p}input_layernorm.weight"), h),
                (&format!("{p}post_attention_layernorm.weight"), h),
                (&format!("{p}mlp.gate_proj.weight"), f * h),
                (&format!("{p}mlp.up_proj.weight"), f * h),
                (&format!("{p}mlp.down_proj.weight"), h * f),
            ])?;
            let down = shared.pop().expect("five");
            let up = shared.pop().expect("five");
            let gate = shared.pop().expect("five");
            let post_norm = shared.pop().expect("five");
            let input_norm = shared.pop().expect("five");
            let mixer = match kind {
                LayerKind::Linear => {
                    let (kd, vd) = (c.key_heads * c.key_dim, c.value_heads * c.value_dim);
                    let channels = 2 * kd + vd;
                    let a = format!("{p}linear_attn.");
                    let mut t = weights.read(&[
                        (&format!("{a}in_proj_qkv.weight"), channels * h),
                        (&format!("{a}in_proj_z.weight"), vd * h),
                        (&format!("{a}in_proj_b.weight"), c.value_heads * h),
                        (&format!("{a}in_proj_a.weight"), c.value_heads * h),
                        (&format!("{a}conv1d.weight"), channels * c.conv_kernel),
                        (&format!("{a}A_log"), c.value_heads),
                        (&format!("{a}dt_bias"), c.value_heads),
                        (&format!("{a}norm.weight"), c.value_dim),
                        (&format!("{a}out_proj.weight"), h * vd),
                    ])?;
                    let out = t.pop().expect("nine");
                    let norm = t.pop().expect("nine");
                    let dt_bias = t.pop().expect("nine");
                    let a_log = t.pop().expect("nine");
                    let conv = t.pop().expect("nine");
                    let a_w = t.pop().expect("nine");
                    let b_w = t.pop().expect("nine");
                    let z = t.pop().expect("nine");
                    let qkv = t.pop().expect("nine");
                    Mixer::Delta(DeltaNet {
                        qkv: linear("linear_attn.in_proj_qkv", channels, h, qkv)?,
                        z: linear("linear_attn.in_proj_z", vd, h, z)?,
                        b: linear("linear_attn.in_proj_b", c.value_heads, h, b_w)?,
                        a: linear("linear_attn.in_proj_a", c.value_heads, h, a_w)?,
                        conv,
                        a_log,
                        dt_bias,
                        norm,
                        out: linear("linear_attn.out_proj", h, vd, out)?,
                    })
                }
                LayerKind::Full => {
                    let (q_rows, kv_rows) = (c.heads * c.head_dim * 2, c.kv_heads * c.head_dim);
                    let a = format!("{p}self_attn.");
                    let mut t = weights.read(&[
                        (&format!("{a}q_proj.weight"), q_rows * h),
                        (&format!("{a}k_proj.weight"), kv_rows * h),
                        (&format!("{a}v_proj.weight"), kv_rows * h),
                        (&format!("{a}o_proj.weight"), h * c.heads * c.head_dim),
                        (&format!("{a}q_norm.weight"), c.head_dim),
                        (&format!("{a}k_norm.weight"), c.head_dim),
                    ])?;
                    let k_norm = t.pop().expect("six");
                    let q_norm = t.pop().expect("six");
                    let o = t.pop().expect("six");
                    let v = t.pop().expect("six");
                    let k = t.pop().expect("six");
                    let q = t.pop().expect("six");
                    Mixer::Attention(Attention {
                        q: linear("self_attn.q_proj", q_rows, h, q)?,
                        k: linear("self_attn.k_proj", kv_rows, h, k)?,
                        v: linear("self_attn.v_proj", kv_rows, h, v)?,
                        o: linear("self_attn.o_proj", h, c.heads * c.head_dim, o)?,
                        q_norm,
                        k_norm,
                    })
                }
            };
            layers.push(Layer {
                input_norm,
                post_norm,
                mixer,
                gate: linear("mlp.gate_proj", f, h, gate)?,
                up: linear("mlp.up_proj", f, h, up)?,
                down: linear("mlp.down_proj", h, f, down)?,
            });
        }
        let norm = weights
            .read(&[(&format!("{prefix}norm.weight"), c.hidden)])?
            .remove(0);
        // transformers: 1 / base ** (arange(0, dim, 2) / dim), all in f32.
        #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
        let inv_freq = (0..c.rotary_dim / 2)
            .map(|i| {
                let exponent = (2 * i) as f32 / c.rotary_dim as f32;
                1.0 / (c.rope_theta as f32).powf(exponent)
            })
            .collect();
        Ok(Self {
            config,
            embed,
            layers,
            norm,
            inv_freq,
        })
    }

    /// A session that has read nothing.
    #[must_use]
    pub fn session(&self) -> Session {
        let c = &self.config;
        let channels = 2 * c.key_heads * c.key_dim + c.value_heads * c.value_dim;
        Session {
            len: 0,
            layers: c
                .layers
                .iter()
                .map(|kind| match kind {
                    LayerKind::Linear => LayerState::Delta {
                        conv: vec![0.0; (c.conv_kernel - 1) * channels],
                        s: vec![0.0; c.value_heads * c.key_dim * c.value_dim],
                    },
                    LayerKind::Full => LayerState::Attention {
                        k: Vec::new(),
                        v: Vec::new(),
                    },
                })
                .collect(),
        }
    }

    /// Reads `ids` after what `session` has read; returns each new position's final
    /// hidden state (`ids.len() x hidden`, after the final norm).
    pub fn forward(&self, session: &mut Session, ids: &[u32]) -> Vec<f32> {
        self.forward_traced(session, ids, &mut |_, _| {})
    }

    /// [`Model::forward`], handing `trace` the residual stream after each layer (`0` is
    /// the embedding), for comparing with another implementation layer by layer.
    pub fn forward_traced(
        &self,
        session: &mut Session,
        ids: &[u32],
        trace: &mut dyn FnMut(usize, &[f32]),
    ) -> Vec<f32> {
        let c = &self.config;
        let (t, h) = (ids.len(), c.hidden);
        let mut x = Vec::with_capacity(t * h);
        for &id in ids {
            x.extend(self.embedding(id));
        }
        trace(0, &x);
        for (index, (layer, state)) in self.layers.iter().zip(&mut session.layers).enumerate() {
            let mut normed = x.clone();
            for row in normed.chunks_mut(h) {
                rms_norm_centred(row, &layer.input_norm, c.rms_eps);
            }
            let mixed = match (&layer.mixer, state) {
                (Mixer::Delta(d), LayerState::Delta { conv, s }) => {
                    self.delta(d, &normed, t, conv, s)
                }
                (Mixer::Attention(a), LayerState::Attention { k, v }) => {
                    self.attention(a, &normed, t, session.len, k, v)
                }
                _ => unreachable!("sessions are built from the same layer list"),
            };
            for (x, m) in x.iter_mut().zip(&mixed) {
                *x += m;
            }
            let mut normed = x.clone();
            for row in normed.chunks_mut(h) {
                rms_norm_centred(row, &layer.post_norm, c.rms_eps);
            }
            let gate = layer.gate.apply(&normed, t);
            let up = layer.up.apply(&normed, t);
            let act: Vec<f32> = gate.iter().zip(&up).map(|(g, u)| silu(*g) * u).collect();
            let down = layer.down.apply(&act, t);
            for (x, d) in x.iter_mut().zip(&down) {
                *x += d;
            }
            trace(index + 1, &x);
        }
        for row in x.chunks_mut(h) {
            rms_norm_centred(row, &self.norm, c.rms_eps);
        }
        session.len += t;
        x
    }

    pub(crate) fn embedding(&self, id: u32) -> Vec<f32> {
        let e = &self.embed;
        let size = e.dtype.size_bytes();
        let row = id as usize * e.hidden * size;
        widen_to_f32(e.dtype, &e.bytes[row..row + e.hidden * size]).expect("a float embedding")
    }

    fn delta(
        &self,
        d: &DeltaNet,
        x: &[f32],
        t: usize,
        history: &mut [f32],
        state: &mut [f32],
    ) -> Vec<f32> {
        let c = &self.config;
        let (kh, vh, dk, dv, kernel) = (
            c.key_heads,
            c.value_heads,
            c.key_dim,
            c.value_dim,
            c.conv_kernel,
        );
        let channels = d.qkv.rows;
        let raw = d.qkv.apply(x, t);
        // Causal depthwise convolution over [history, raw], then SiLU.
        let mut mixed = vec![0.0_f32; t * channels];
        let input = |p: isize, ch: usize| -> f32 {
            if p >= 0 {
                #[allow(clippy::cast_sign_loss)]
                raw[p as usize * channels + ch]
            } else {
                #[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
                history[((kernel as isize - 1) + p) as usize * channels + ch]
            }
        };
        for p in 0..t {
            for ch in 0..channels {
                let mut sum = 0.0;
                for j in 0..kernel {
                    #[allow(clippy::cast_possible_wrap)]
                    let at = p as isize + j as isize - (kernel as isize - 1);
                    sum += d.conv[ch * kernel + j] * input(at, ch);
                }
                mixed[p * channels + ch] = silu(sum);
            }
        }
        let keep = kernel - 1;
        let mut joined = history.to_vec();
        joined.extend_from_slice(&raw);
        history.copy_from_slice(&joined[joined.len() - keep * channels..]);

        let z = d.z.apply(x, t);
        let b = d.b.apply(x, t);
        let a = d.a.apply(x, t);
        let group = vh / kh;
        #[allow(clippy::cast_precision_loss)]
        let q_scale = 1.0 / (dk as f32).sqrt();
        let mut read = vec![0.0_f32; vh * t * dv]; // head-major
                                                   // Each value head's recurrence runs on its own thread with its own state.
        let mut heads: Vec<(&mut [f32], &mut [f32])> = state
            .chunks_mut(dk * dv)
            .zip(read.chunks_mut(t * dv))
            .collect();
        std::thread::scope(|scope| {
            for (head, (s, out)) in heads.iter_mut().enumerate() {
                let (mixed, b, a) = (&mixed, &b, &a);
                scope.spawn(move || {
                    let key_head = head / group;
                    let mut q = vec![0.0_f32; dk];
                    let mut k = vec![0.0_f32; dk];
                    let mut kv = vec![0.0_f32; dv];
                    for p in 0..t {
                        let row = &mixed[p * channels..(p + 1) * channels];
                        q.copy_from_slice(&row[key_head * dk..(key_head + 1) * dk]);
                        k.copy_from_slice(
                            &row[kh * dk + key_head * dk..kh * dk + (key_head + 1) * dk],
                        );
                        let v = &row[2 * kh * dk + head * dv..2 * kh * dk + (head + 1) * dv];
                        l2_normalize(&mut q);
                        l2_normalize(&mut k);
                        for x in &mut q {
                            *x *= q_scale;
                        }
                        let rate = -d.a_log[head].exp();
                        let g = rate * softplus(a[p * vh + head] + d.dt_bias[head]);
                        let decay = g.exp();
                        let beta = sigmoid(b[p * vh + head]);
                        for x in s.iter_mut() {
                            *x *= decay;
                        }
                        kv.fill(0.0);
                        for i in 0..dk {
                            let ki = k[i];
                            for (m, sij) in kv.iter_mut().zip(&s[i * dv..(i + 1) * dv]) {
                                *m += sij * ki;
                            }
                        }
                        for i in 0..dk {
                            let ki = k[i];
                            for j in 0..dv {
                                s[i * dv + j] += ki * (v[j] - kv[j]) * beta;
                            }
                        }
                        let o = &mut out[p * dv..(p + 1) * dv];
                        o.fill(0.0);
                        for i in 0..dk {
                            let qi = q[i];
                            for (oj, sij) in o.iter_mut().zip(&s[i * dv..(i + 1) * dv]) {
                                *oj += sij * qi;
                            }
                        }
                    }
                });
            }
        });
        // Gated RMS norm per head, back to position-major.
        let mut gated = vec![0.0_f32; t * vh * dv];
        for p in 0..t {
            for head in 0..vh {
                let o = &mut gated[p * vh * dv + head * dv..p * vh * dv + (head + 1) * dv];
                o.copy_from_slice(&read[head * t * dv + p * dv..head * t * dv + (p + 1) * dv]);
                rms_normalize(o, c.rms_eps);
                for j in 0..dv {
                    o[j] *= d.norm[j] * silu(z[p * vh * dv + head * dv + j]);
                }
            }
        }
        d.out.apply(&gated, t)
    }

    fn attention(
        &self,
        a: &Attention,
        x: &[f32],
        t: usize,
        start: usize,
        keys: &mut Vec<f32>,
        values: &mut Vec<f32>,
    ) -> Vec<f32> {
        let c = &self.config;
        let (heads, kvh, hd) = (c.heads, c.kv_heads, c.head_dim);
        let qg = a.q.apply(x, t);
        let mut k = a.k.apply(x, t);
        let v = a.v.apply(x, t);
        let mut q = vec![0.0_f32; t * heads * hd];
        let mut gate = vec![0.0_f32; t * heads * hd];
        for p in 0..t {
            for head in 0..heads {
                let src = &qg[p * heads * 2 * hd + head * 2 * hd..];
                let at = p * heads * hd + head * hd;
                q[at..at + hd].copy_from_slice(&src[..hd]);
                gate[at..at + hd].copy_from_slice(&src[hd..2 * hd]);
                rms_norm_centred(&mut q[at..at + hd], &a.q_norm, c.rms_eps);
                self.rope(&mut q[at..at + hd], start + p);
            }
            for head in 0..kvh {
                let at = p * kvh * hd + head * hd;
                rms_norm_centred(&mut k[at..at + hd], &a.k_norm, c.rms_eps);
                self.rope(&mut k[at..at + hd], start + p);
            }
        }
        keys.extend_from_slice(&k);
        values.extend_from_slice(&v);
        let (keys, values) = (&*keys, &*values);
        #[allow(clippy::cast_precision_loss)]
        let scale = 1.0 / (hd as f32).sqrt();
        let group = heads / kvh;
        let mut out = vec![0.0_f32; heads * t * hd]; // head-major
        par_chunks(&mut out, t * hd, |head, o| {
            let kv_head = head / group;
            let mut scores = Vec::with_capacity(start + t);
            for p in 0..t {
                let qp = &q[p * heads * hd + head * hd..p * heads * hd + (head + 1) * hd];
                scores.clear();
                for j in 0..=start + p {
                    let kj = &keys[j * kvh * hd + kv_head * hd..j * kvh * hd + (kv_head + 1) * hd];
                    scores.push(dot(qp, kj) * scale);
                }
                softmax(&mut scores);
                let op = &mut o[p * hd..(p + 1) * hd];
                for (j, w) in scores.iter().enumerate() {
                    let vj =
                        &values[j * kvh * hd + kv_head * hd..j * kvh * hd + (kv_head + 1) * hd];
                    for (o, v) in op.iter_mut().zip(vj) {
                        *o += w * v;
                    }
                }
            }
        });
        let mut gated = vec![0.0_f32; t * heads * hd];
        for p in 0..t {
            for head in 0..heads {
                let at = p * heads * hd + head * hd;
                for i in 0..hd {
                    gated[at + i] = out[head * t * hd + p * hd + i] * sigmoid(gate[at + i]);
                }
            }
        }
        a.o.apply(&gated, t)
    }

    /// RoPE on the first `rotary_dim` dimensions of one head, at `position`.
    fn rope(&self, x: &mut [f32], position: usize) {
        let half = self.config.rotary_dim / 2;
        #[allow(clippy::cast_precision_loss)]
        let pos = position as f32;
        for (i, f) in self.inv_freq.iter().enumerate() {
            let angle = pos * f;
            let (sin, cos) = angle.sin_cos();
            let (x1, x2) = (x[i], x[i + half]);
            x[i] = x1 * cos - x2 * sin;
            x[i + half] = x2 * cos + x1 * sin;
        }
    }
}

/// `x / sqrt(sum(x^2) + 1e-6)`, in place.
fn l2_normalize(x: &mut [f32]) {
    let scale = 1.0 / (x.iter().map(|v| v * v).sum::<f32>() + 1e-6).sqrt();
    for v in x {
        *v *= scale;
    }
}

/// The adapter's `lora_A` (`r x cols`) and `lora_B` (`rows x r`) for `module`, and `r`.
fn read_lora(
    lora: &Weights,
    rows: usize,
    cols: usize,
    module: &str,
) -> Result<(Vec<f32>, Vec<f32>, usize), LoadError> {
    let a_name = format!("{module}.lora_A.weight");
    let b_name = format!("{module}.lora_B.weight");
    let r = usize::try_from(lora.info(&a_name)?.shape.first().copied().unwrap_or(0)).unwrap_or(0);
    let mut ab = lora.read(&[(&a_name, r * cols), (&b_name, rows * r)])?;
    let b = ab.pop().expect("two");
    let a = ab.pop().expect("two");
    Ok((a, b, r))
}

/// `w += scale * B A`.
fn merge_lora(w: &mut [f32], cols: usize, (a, b, r): (&[f32], &[f32], usize), scale: f32) {
    par_chunks(w, cols, |row, w| {
        for k in 0..r {
            let coefficient = scale * b[row * r + k];
            for (w, a) in w.iter_mut().zip(&a[k * cols..(k + 1) * cols]) {
                *w += coefficient * a;
            }
        }
    });
}
