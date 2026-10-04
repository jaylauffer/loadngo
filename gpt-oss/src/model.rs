//! gpt-oss's forward pass on the CPU: the reference every faster path is checked against.
//!
//! Per layer, with `h` the residual stream:
//!
//! - **Attention.** `h += attention(rms_norm(h))`. Queries, keys and values have biases.
//!   Rotary (YaRN) turns the two halves of each head against each other. Scores are
//!   scaled by `1/sqrt(head_dim)`. Each head has a learned *sink*: one more logit in the
//!   softmax that takes probability but carries no value. Even layers see the newest
//!   `sliding_window` positions, odd layers all of them.
//! - **Experts.** `h += experts(rms_norm(h))`. A router with a bias picks the
//!   `experts_used` highest logits and softmaxes over just those. Each picked expert
//!   computes `down((clamp(up, -7, 7) + 1) * g * sigmoid(1.702 g))` with
//!   `g = min(gate, 7)`, all three products with biases.
//!
//! Then a final RMS norm and the output matrix give the logits.
//!
//! Matrices stay in their file's format (`Q8_0`, MXFP4, or `f32`) and are multiplied
//! without widening; large products are split across the CPU cores.

use std::path::Path;

use loadngo_weights::{
    gguf::{self, GgmlType, Gguf, GgufError, Tensor},
    mxfp4::{ggml_to_ocp, Mxfp4Matrix},
    q8_0::Q8Matrix,
};

use crate::config::{Config, ConfigError};

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error(transparent)]
    Gguf(#[from] GgufError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("{0}")]
    Invalid(String),
}

pub(crate) enum Format {
    F32(Vec<f32>),
    Q8(Vec<u8>),
    Mxfp4 { elements: Vec<u8>, scales: Vec<u8> },
}

/// A row-major matrix in its stored format.
pub struct Matrix {
    pub(crate) rows: usize,
    pub(crate) cols: usize,
    pub(crate) format: Format,
}

impl Matrix {
    /// `rows` x `cols` from a tensor's bytes in ggml type `kind`.
    fn new(kind: GgmlType, bytes: &[u8], rows: usize, cols: usize) -> Result<Self, ModelError> {
        let format = match kind {
            GgmlType::F32 => Format::F32(f32s(bytes)),
            GgmlType::Q8_0 => {
                Q8Matrix::new(bytes, rows, cols).map_err(|e| ModelError::Invalid(e.to_string()))?;
                Format::Q8(bytes.to_vec())
            }
            GgmlType::Mxfp4 => {
                let (elements, scales) = ggml_to_ocp(bytes, rows, cols);
                Format::Mxfp4 { elements, scales }
            }
            other => return Err(ModelError::Invalid(format!("matrices of {other:?}"))),
        };
        Ok(Self { rows, cols, format })
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Frees the weights (the GPU holds its own copy); the matrix must not be used
    /// after.
    #[cfg(target_os = "macos")]
    pub(crate) fn release(&mut self) {
        self.format = Format::F32(Vec::new());
    }

    /// Row `r` widened into `out`.
    pub fn row(&self, r: usize, out: &mut [f32]) {
        match &self.format {
            Format::F32(values) => out.copy_from_slice(&values[r * self.cols..][..self.cols]),
            Format::Q8(bytes) => Q8Matrix::new(bytes, self.rows, self.cols)
                .expect("checked at load")
                .dequantize_row(r, out),
            Format::Mxfp4 { elements, scales } => {
                Mxfp4Matrix::new(elements, scales, self.rows, self.cols)
                    .expect("shaped at load")
                    .dequantize_row(r, out);
            }
        }
    }

    /// `y[i] = row(first + i) · x`.
    pub(crate) fn mul_rows(&self, first: usize, y: &mut [f32], x: &[f32]) {
        match &self.format {
            Format::F32(values) => {
                for (i, out) in y.iter_mut().enumerate() {
                    let row = &values[(first + i) * self.cols..][..self.cols];
                    *out = row.iter().zip(x).map(|(a, b)| a * b).sum();
                }
            }
            Format::Q8(bytes) => Q8Matrix::new(bytes, self.rows, self.cols)
                .expect("checked at load")
                .mul_rows(first, y, x),
            Format::Mxfp4 { elements, scales } => {
                let per_row = Mxfp4Matrix::element_bytes_per_row(self.cols);
                let scales_per_row = Mxfp4Matrix::scales_per_row(self.cols);
                let rows = first..first + y.len();
                Mxfp4Matrix::new(
                    &elements[rows.start * per_row..rows.end * per_row],
                    &scales[rows.start * scales_per_row..rows.end * scales_per_row],
                    y.len(),
                    self.cols,
                )
                .expect("shaped at load")
                .mul_vec(y, x);
            }
        }
    }

    /// `y = W x`, split across the CPU cores when large.
    pub fn mul_vec(&self, y: &mut [f32], x: &[f32]) {
        assert_eq!((y.len(), x.len()), (self.rows, self.cols), "product shape");
        let threads = std::thread::available_parallelism().map_or(1, usize::from);
        if threads == 1 || self.rows * self.cols < 1 << 18 {
            self.mul_rows(0, y, x);
            return;
        }
        let chunk = self.rows.div_ceil(threads);
        std::thread::scope(|scope| {
            for (i, part) in y.chunks_mut(chunk).enumerate() {
                scope.spawn(move || self.mul_rows(i * chunk, part, x));
            }
        });
    }
}

fn f32s(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect()
}

pub(crate) struct Expert {
    pub(crate) gate: Matrix,
    pub(crate) up: Matrix,
    pub(crate) down: Matrix,
    pub(crate) gate_bias: Vec<f32>,
    pub(crate) up_bias: Vec<f32>,
    pub(crate) down_bias: Vec<f32>,
}

pub(crate) struct Layer {
    pub(crate) attn_norm: Vec<f32>,
    pub(crate) q: Matrix,
    pub(crate) k: Matrix,
    pub(crate) v: Matrix,
    pub(crate) o: Matrix,
    pub(crate) q_bias: Vec<f32>,
    pub(crate) k_bias: Vec<f32>,
    pub(crate) v_bias: Vec<f32>,
    pub(crate) o_bias: Vec<f32>,
    pub(crate) sinks: Vec<f32>,
    pub(crate) ffn_norm: Vec<f32>,
    pub(crate) router: Matrix,
    pub(crate) router_bias: Vec<f32>,
    pub(crate) experts: Vec<Expert>,
}

pub struct Model {
    pub config: Config,
    pub(crate) embedding: Matrix,
    pub(crate) output: Matrix,
    pub(crate) output_norm: Vec<f32>,
    pub(crate) layers: Vec<Layer>,
    pub(crate) inv_freq: Vec<f64>,
    pub(crate) rope_scale: f64,
}

/// A conversation's keys and values so far.
pub struct Session {
    keys: Vec<Vec<f32>>,
    values: Vec<Vec<f32>>,
    len: usize,
}

impl Session {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Goes back to position `len`, as if nothing after it had been fed.
    pub fn truncate(&mut self, len: usize) {
        if len < self.len {
            let per = |v: &Vec<f32>| v.len() / self.len.max(1);
            for v in self.keys.iter_mut().chain(self.values.iter_mut()) {
                let width = per(v);
                v.truncate(len * width);
            }
            self.len = len;
        }
    }
}

/// The tensors of a file, read in one batch through the proactor.
struct Tensors<'g> {
    gguf: &'g Gguf,
    bytes: std::collections::HashMap<String, Vec<u8>>,
}

impl Tensors<'_> {
    fn info(&self, name: &str) -> Result<&Tensor, ModelError> {
        self.gguf
            .tensor(name)
            .ok_or_else(|| ModelError::Invalid(format!("{name} is missing")))
    }

    fn bytes(&self, name: &str) -> &[u8] {
        &self.bytes[name]
    }

    fn vector(&self, name: &str, len: usize) -> Result<Vec<f32>, ModelError> {
        let info = self.info(name)?;
        if info.ggml_type != GgmlType::F32 || info.numel() != len as u64 {
            return Err(ModelError::Invalid(format!("{name}: want {len} f32")));
        }
        Ok(f32s(self.bytes(name)))
    }

    fn matrix(&self, name: &str, rows: usize, cols: usize) -> Result<Matrix, ModelError> {
        let info = self.info(name)?;
        if info.dims != [cols as u64, rows as u64] {
            return Err(ModelError::Invalid(format!(
                "{name}: dims {:?}, want [{cols}, {rows}]",
                info.dims
            )));
        }
        Matrix::new(info.ggml_type, self.bytes(name), rows, cols)
    }

    /// One `rows` x `cols` matrix per expert from a `[cols, rows, experts]` tensor.
    fn experts(
        &self,
        name: &str,
        rows: usize,
        cols: usize,
        n: usize,
    ) -> Result<Vec<Matrix>, ModelError> {
        let info = self.info(name)?;
        if info.dims != [cols as u64, rows as u64, n as u64] {
            return Err(ModelError::Invalid(format!("{name}: dims {:?}", info.dims)));
        }
        let bytes = self.bytes(name);
        let each = bytes.len() / n;
        bytes
            .chunks_exact(each)
            .map(|part| Matrix::new(info.ggml_type, part, rows, cols))
            .collect()
    }
}

/// `x * w / sqrt(mean(x^2) + eps)`.
fn rms_norm(x: &[f32], weight: &[f32], eps: f32, out: &mut [f32]) {
    let mean = x.iter().map(|v| f64::from(*v) * f64::from(*v)).sum::<f64>() / x.len() as f64;
    let scale = (1.0 / (mean + f64::from(eps)).sqrt()) as f32;
    for ((o, &v), &w) in out.iter_mut().zip(x).zip(weight) {
        *o = v * scale * w;
    }
}

fn add(x: &mut [f32], bias: &[f32]) {
    for (v, b) in x.iter_mut().zip(bias) {
        *v += b;
    }
}

impl Model {
    /// Reads a gpt-oss GGUF: its header, then every tensor in one batch through the
    /// proactor.
    pub fn load(path: &Path) -> Result<Self, ModelError> {
        let (gguf, reader) = gguf::open(path)?;
        let config = Config::from_gguf(&gguf)?;
        let mut ranges = Vec::new();
        for tensor in &gguf.tensors {
            let len = tensor
                .len
                .ok_or_else(|| ModelError::Invalid(format!("{}: unsupported type", tensor.name)))?;
            ranges.push((tensor.name.as_str(), tensor.offset, len as usize));
        }
        let read = reader.read_ranges(&ranges).map_err(GgufError::from)?;
        let tensors = Tensors {
            gguf: &gguf,
            bytes: ranges.iter().map(|r| r.0.to_owned()).zip(read).collect(),
        };
        Self::from_tensors(config, &tensors)
    }

    fn from_tensors(config: Config, t: &Tensors<'_>) -> Result<Self, ModelError> {
        let c = &config;
        let (q_width, kv_width) = (c.heads * c.head_dim, c.kv_heads * c.head_dim);
        let mut layers = Vec::with_capacity(c.layers);
        for l in 0..c.layers {
            let name = |part: &str| format!("blk.{l}.{part}");
            let gates = t.experts(
                &name("ffn_gate_exps.weight"),
                c.expert_hidden,
                c.hidden,
                c.experts,
            )?;
            let ups = t.experts(
                &name("ffn_up_exps.weight"),
                c.expert_hidden,
                c.hidden,
                c.experts,
            )?;
            let downs = t.experts(
                &name("ffn_down_exps.weight"),
                c.hidden,
                c.expert_hidden,
                c.experts,
            )?;
            let gate_bias = t.vector(&name("ffn_gate_exps.bias"), c.experts * c.expert_hidden)?;
            let up_bias = t.vector(&name("ffn_up_exps.bias"), c.experts * c.expert_hidden)?;
            let down_bias = t.vector(&name("ffn_down_exps.bias"), c.experts * c.hidden)?;
            let experts = gates
                .into_iter()
                .zip(ups)
                .zip(downs)
                .enumerate()
                .map(|(e, ((gate, up), down))| Expert {
                    gate,
                    up,
                    down,
                    gate_bias: gate_bias[e * c.expert_hidden..][..c.expert_hidden].to_vec(),
                    up_bias: up_bias[e * c.expert_hidden..][..c.expert_hidden].to_vec(),
                    down_bias: down_bias[e * c.hidden..][..c.hidden].to_vec(),
                })
                .collect();
            layers.push(Layer {
                attn_norm: t.vector(&name("attn_norm.weight"), c.hidden)?,
                q: t.matrix(&name("attn_q.weight"), q_width, c.hidden)?,
                k: t.matrix(&name("attn_k.weight"), kv_width, c.hidden)?,
                v: t.matrix(&name("attn_v.weight"), kv_width, c.hidden)?,
                o: t.matrix(&name("attn_output.weight"), c.hidden, q_width)?,
                q_bias: t.vector(&name("attn_q.bias"), q_width)?,
                k_bias: t.vector(&name("attn_k.bias"), kv_width)?,
                v_bias: t.vector(&name("attn_v.bias"), kv_width)?,
                o_bias: t.vector(&name("attn_output.bias"), c.hidden)?,
                sinks: t.vector(&name("attn_sinks.weight"), c.heads)?,
                ffn_norm: t.vector(&name("post_attention_norm.weight"), c.hidden)?,
                router: t.matrix(&name("ffn_gate_inp.weight"), c.experts, c.hidden)?,
                router_bias: t.vector(&name("ffn_gate_inp.bias"), c.experts)?,
                experts,
            });
        }
        let (inv_freq, rope_scale) = config.rope();
        Ok(Self {
            embedding: t.matrix("token_embd.weight", c.vocab, c.hidden)?,
            output: t.matrix("output.weight", c.vocab, c.hidden)?,
            output_norm: t.vector("output_norm.weight", c.hidden)?,
            layers,
            inv_freq,
            rope_scale,
            config,
        })
    }

    pub fn session(&self) -> Session {
        Session {
            keys: vec![Vec::new(); self.config.layers],
            values: vec![Vec::new(); self.config.layers],
            len: 0,
        }
    }

    /// Rotates each head of `x` (heads of `head_dim`) for position `pos`: element `i` of
    /// the first half turns with element `i` of the second.
    fn rotate(&self, x: &mut [f32], pos: usize) {
        let half = self.config.head_dim / 2;
        for head in x.chunks_exact_mut(self.config.head_dim) {
            for (i, &freq) in self.inv_freq.iter().enumerate() {
                let angle = pos as f64 * freq;
                let (sin, cos) = angle.sin_cos();
                let (cos, sin) = (
                    (cos * self.rope_scale) as f32,
                    (sin * self.rope_scale) as f32,
                );
                let (a, b) = (head[i], head[i + half]);
                head[i] = a * cos - b * sin;
                head[i + half] = b * cos + a * sin;
            }
        }
    }

    /// Feeds one token at the session's next position and returns the logits for the
    /// token after it.
    pub fn step(&self, session: &mut Session, token: u32) -> Vec<f32> {
        let c = &self.config;
        let pos = session.len;
        let mut h = vec![0.0_f32; c.hidden];
        self.embedding.row(token as usize, &mut h);
        let mut x = vec![0.0_f32; c.hidden];
        let (q_width, kv_width) = (c.heads * c.head_dim, c.kv_heads * c.head_dim);
        let mut q = vec![0.0_f32; q_width];
        let mut k = vec![0.0_f32; kv_width];
        let mut v = vec![0.0_f32; kv_width];
        let mut attended = vec![0.0_f32; q_width];
        let mut out = vec![0.0_f32; c.hidden];
        let group = c.heads / c.kv_heads;
        let scale = 1.0 / (c.head_dim as f32).sqrt();
        for (l, layer) in self.layers.iter().enumerate() {
            rms_norm(&h, &layer.attn_norm, c.rms_eps, &mut x);
            layer.q.mul_vec(&mut q, &x);
            layer.k.mul_vec(&mut k, &x);
            layer.v.mul_vec(&mut v, &x);
            add(&mut q, &layer.q_bias);
            add(&mut k, &layer.k_bias);
            add(&mut v, &layer.v_bias);
            self.rotate(&mut q, pos);
            self.rotate(&mut k, pos);
            session.keys[l].extend_from_slice(&k);
            session.values[l].extend_from_slice(&v);
            let first = if c.is_sliding(l) {
                (pos + 1).saturating_sub(c.sliding_window)
            } else {
                0
            };
            let (keys, values) = (&session.keys[l], &session.values[l]);
            let mut scores = Vec::with_capacity(pos + 1 - first);
            for head in 0..c.heads {
                let kv = head / group;
                let query = &q[head * c.head_dim..][..c.head_dim];
                scores.clear();
                for p in first..=pos {
                    let key = &keys[(p * c.kv_heads + kv) * c.head_dim..][..c.head_dim];
                    scores.push(query.iter().zip(key).map(|(a, b)| a * b).sum::<f32>() * scale);
                }
                let sink = layer.sinks[head];
                let max = scores.iter().copied().fold(sink, f32::max);
                let mut total = (sink - max).exp();
                for s in &mut scores {
                    *s = (*s - max).exp();
                    total += *s;
                }
                let target = &mut attended[head * c.head_dim..][..c.head_dim];
                target.fill(0.0);
                for (p, &weight) in (first..=pos).zip(&scores) {
                    let value = &values[(p * c.kv_heads + kv) * c.head_dim..][..c.head_dim];
                    for (t, &val) in target.iter_mut().zip(value) {
                        *t += weight / total * val;
                    }
                }
            }
            layer.o.mul_vec(&mut out, &attended);
            add(&mut out, &layer.o_bias);
            add(&mut h, &out);

            rms_norm(&h, &layer.ffn_norm, c.rms_eps, &mut x);
            self.experts(layer, &x, &mut out);
            add(&mut h, &out);
        }
        session.len += 1;
        rms_norm(&h, &self.output_norm, c.rms_eps, &mut x);
        let mut logits = vec![0.0_f32; c.vocab];
        self.output.mul_vec(&mut logits, &x);
        logits
    }

    /// The experts `x` goes to and their weights: the `experts_used` highest router
    /// logits (equal logits: the lower index first), softmaxed over just those.
    pub(crate) fn route(&self, layer: &Layer, x: &[f32]) -> Vec<(usize, f32)> {
        let c = &self.config;
        let mut router = vec![0.0_f32; c.experts];
        layer.router.mul_rows(0, &mut router, x);
        add(&mut router, &layer.router_bias);
        let mut order: Vec<usize> = (0..c.experts).collect();
        order.sort_by(|&a, &b| router[b].total_cmp(&router[a]).then(a.cmp(&b)));
        let picked = &order[..c.experts_used];
        let max = router[picked[0]];
        let weights: Vec<f32> = picked.iter().map(|&e| (router[e] - max).exp()).collect();
        let total: f32 = weights.iter().sum();
        picked
            .iter()
            .zip(weights)
            .map(|(&e, w)| (e, w / total))
            .collect()
    }

    /// Row `token` of the embedding, widened into `out`.
    pub fn embed(&self, token: u32, out: &mut [f32]) {
        self.embedding.row(token as usize, out);
    }

    fn experts(&self, layer: &Layer, x: &[f32], out: &mut [f32]) {
        let c = &self.config;
        let routed = self.route(layer, x);
        let picked: Vec<usize> = routed.iter().map(|&(e, _)| e).collect();
        let weights: Vec<f32> = routed.iter().map(|&(_, w)| w).collect();
        out.fill(0.0);
        let results: Vec<Vec<f32>> = std::thread::scope(|scope| {
            let handles: Vec<_> = picked
                .iter()
                .map(|&e| {
                    let expert = &layer.experts[e];
                    scope.spawn(move || {
                        let mut gate = vec![0.0_f32; c.expert_hidden];
                        let mut up = vec![0.0_f32; c.expert_hidden];
                        expert.gate.mul_rows(0, &mut gate, x);
                        expert.up.mul_rows(0, &mut up, x);
                        let hidden: Vec<f32> = gate
                            .iter()
                            .zip(&expert.gate_bias)
                            .zip(up.iter().zip(&expert.up_bias))
                            .map(|((&g, &gb), (&u, &ub))| {
                                let g = (g + gb).min(7.0);
                                let u = (u + ub).clamp(-7.0, 7.0);
                                (u + 1.0) * g / (1.0 + (-1.702 * g).exp())
                            })
                            .collect();
                        let mut y = vec![0.0_f32; c.hidden];
                        expert.down.mul_rows(0, &mut y, &hidden);
                        add(&mut y, &expert.down_bias);
                        y
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("expert thread"))
                .collect()
        });
        for (y, &w) in results.iter().zip(&weights) {
            for (o, &v) in out.iter_mut().zip(y) {
                *o += w * v;
            }
        }
    }
}
