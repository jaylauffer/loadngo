//! The Qwen3.5 torso on Apple GPUs, through `loadngo-metal-compute`.
//!
//! The weights are copied once into GPU memory as bfloat16, exactly as the base model's
//! files hold them. The adapter is kept apart from them (`A` and `scale * B` rounded to
//! bfloat16): merged into bfloat16 weights, its small update would be rounded away with
//! them. Projections that read the same input share one product with their `A`s
//! stacked, padded with zero rows to a multiple of 64; each `B` is padded with zero
//! columns to match, so both products run on the matrix units. Measured on 1,024
//! positions: the adapter took half the pass with a product per `A` and per `B` at their
//! own narrow shapes.
//!
//! A read of `t` positions runs in passes of at most `chunk` positions, one submission
//! per pass for all the layers, each finishing as a completion on a loadngo proactor.
//! Per layer the GPU does what [`crate::model`] does on the CPU:
//!
//! - **Gated DeltaNet:** `q k v` in one product; the causal convolution with SiLU over
//!   the session's history, then the history updated; `q` and `k` L2-normalized (`q`
//!   scaled); `z`, `b` and `a`; `beta = sigmoid(b)` and the per-head decay from `a`; the
//!   delta-rule recurrence on the session's state; the read RMS-normalized and gated by
//!   `silu(z)`; the output product.
//! - **Gated attention:** `q` with its gate, `k` and `v`; per-head RMS norms; partial
//!   rotary; the new keys and values into the session's cache; grouped attention over
//!   every earlier position; the sigmoid gate; the output product.
//! - The SiLU-gated MLP, and residual adds.
//!
//! From 32 positions, products whose shapes allow it run on the matrix units, on
//! positions rounded up to 32. A [`GpuSession`] holds every layer's carried state in one
//! buffer, so copying a session (reading each question from the state's session) is a
//! copy in shared memory.
//!
//! [`crate::model::Model`] stays the reference this path is checked against
//! (`tests/gpu.rs`).

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use loadngo_metal_compute::{
    Batch, Buffer, Completed, Dispatch, Gpu, GroupedShape, RecurrenceShape, Resident, Slice,
};
use loadngo_proactor::{new_platform_proactor, PlatformPort, Proactor};
use loadngo_weights::dtype::round_to_bf16;

use crate::{
    config::{LayerKind, TorsoConfig},
    model::{Linear, Mixer, Model},
};

#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    #[error(transparent)]
    Metal(#[from] loadngo_metal_compute::Error),
    #[error("proactor: {0}")]
    Proactor(#[from] std::io::Error),
    #[error("{0}")]
    Invalid(String),
}

/// A bfloat16 matrix in GPU memory and its adapter's `scale * B`, if any, widened with
/// zero columns to its group's stacked `A` (`rows x width`).
struct Mat {
    w: Arc<Resident>,
    rows: usize,
    cols: usize,
    lora: Option<(Arc<Resident>, usize)>,
}

/// The stacked `A`s of projections that read the same input: `width x cols`, each
/// projection's rows in turn, then zero rows up to a multiple of 64.
struct LoraDown {
    a: Arc<Resident>,
    width: usize,
    cols: usize,
}

fn bf16_words(v: &[f32]) -> Vec<u16> {
    v.iter()
        .map(|x| {
            // `round_to_bf16` rounds to nearest even; the top half of the result is exact.
            #[allow(clippy::cast_possible_truncation)]
            let word = (round_to_bf16(*x).to_bits() >> 16) as u16;
            word
        })
        .collect()
}

fn floats(gpu: &Gpu, v: &[f32]) -> Result<Arc<Resident>, GpuError> {
    let bytes: Vec<u8> = v.iter().flat_map(|f| f.to_le_bytes()).collect();
    Ok(gpu.resident(&bytes)?)
}

/// Copies projections that read the same input into GPU memory, with their adapters'
/// `A`s stacked into one [`LoraDown`].
fn group(gpu: &Gpu, linears: &[&Linear]) -> Result<(Option<LoraDown>, Vec<Mat>), GpuError> {
    let cols = linears[0].cols;
    let ranks: Vec<usize> = linears
        .iter()
        .map(|m| m.lora.as_ref().map_or(0, |l| l.r))
        .collect();
    let total: usize = ranks.iter().sum();
    let width = total.next_multiple_of(64);
    let mut stacked = vec![0.0_f32; width * cols];
    let mut mats = Vec::with_capacity(linears.len());
    let mut first = 0;
    for (m, &r) in linears.iter().zip(&ranks) {
        if m.cols != cols {
            return Err(GpuError::Invalid(
                "projections of one input differ in width".into(),
            ));
        }
        let lora = match &m.lora {
            Some(l) => {
                stacked[first * cols..(first + r) * cols].copy_from_slice(&l.a);
                let mut b = vec![0.0_f32; m.rows * width];
                for (row, wide) in b.chunks_mut(width).enumerate() {
                    for k in 0..r {
                        wide[first + k] = l.scale * l.b[row * r + k];
                    }
                }
                Some((gpu.resident_words(&bf16_words(&b))?, width))
            }
            None => None,
        };
        mats.push(Mat {
            w: gpu.resident_words(&bf16_words(&m.w))?,
            rows: m.rows,
            cols: m.cols,
            lora,
        });
        first += r;
    }
    let down = if total == 0 {
        None
    } else {
        Some(LoraDown {
            a: gpu.resident_words(&bf16_words(&stacked))?,
            width,
            cols,
        })
    };
    Ok((down, mats))
}

/// [`group`] of one projection.
fn single(gpu: &Gpu, m: &Linear) -> Result<(Option<LoraDown>, Mat), GpuError> {
    let (down, mut mats) = group(gpu, &[m])?;
    Ok((down, mats.pop().expect("one")))
}

/// Weights that are plain vectors: norms (with the `1 +` of the zero-centred norms
/// applied) and the DeltaNet parameters.
struct GpuDelta {
    /// For `qkv`, `z`, `b` and `a`.
    lora_in: Option<LoraDown>,
    qkv: Mat,
    z: Mat,
    b: Mat,
    a: Mat,
    conv: Arc<Resident>,
    a_log: Arc<Resident>,
    dt_bias: Arc<Resident>,
    norm: Arc<Resident>,
    lora_out: Option<LoraDown>,
    out: Mat,
}

struct GpuAttention {
    /// For `q`, `k` and `v`.
    lora_in: Option<LoraDown>,
    q: Mat,
    k: Mat,
    v: Mat,
    lora_o: Option<LoraDown>,
    o: Mat,
    q_norm: Arc<Resident>,
    k_norm: Arc<Resident>,
}

enum GpuMixer {
    Delta(GpuDelta),
    Attention(GpuAttention),
}

struct GpuLayer {
    input_norm: Arc<Resident>,
    post_norm: Arc<Resident>,
    mixer: GpuMixer,
    /// For `gate` and `up`.
    lora_mlp: Option<LoraDown>,
    gate: Mat,
    up: Mat,
    lora_down: Option<LoraDown>,
    down: Mat,
}

/// A pass's working buffers, by position in the batch; the session's state buffer comes
/// after them.
const X: usize = 0;
const N: usize = 1;
const BIG1: usize = 2;
const BIG2: usize = 3;
const Q: usize = 4;
const K: usize = 5;
const V: usize = 6;
const Z: usize = 7;
const O: usize = 8;
const BETA: usize = 9;
const A: usize = 10;
const ALPHA: usize = 11;
const M: usize = 12;
const TABLE: usize = 13;
const LT: usize = 14;
const LY: usize = 15;
const STATE: usize = 16;

/// `rows` rows of `width` floats from row `row` of `buffer`.
fn f(buffer: usize, row: usize, width: usize, rows: usize) -> Slice {
    Slice::new(buffer, row * width * 4, rows * width * 4)
}

/// Where one layer's carried state sits in a session's buffer, in floats.
#[derive(Clone, Copy, Debug)]
enum Carried {
    /// Convolution history (`channels x (kernel - 1)`) and recurrent state.
    Delta { history: usize, state: usize },
    /// Key and value caches, `capacity x kv_heads x head_dim` each.
    Attention { keys: usize, values: usize },
}

fn lost() -> GpuError {
    GpuError::Invalid("the session was lost to a failed pass".into())
}

/// What a session has read, on the GPU: every layer's carried state in one buffer.
pub struct GpuSession {
    state: Option<Buffer>,
    len: usize,
}

impl GpuSession {
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

/// Time spent in the passes since the last [`GpuModel::take_time`].
#[derive(Clone, Copy, Debug, Default)]
pub struct PassTime {
    pub passes: usize,
    pub positions: usize,
    pub gpu: Duration,
    pub wall: Duration,
}

pub struct GpuModel {
    pub config: TorsoConfig,
    /// The CPU side: the embedding and the rotary frequencies. Its matrices are released
    /// once they are in GPU memory.
    model: Model,
    gpu: Gpu,
    proactor: Proactor<PlatformPort>,
    layers: Vec<GpuLayer>,
    norm: Arc<Resident>,
    /// Positions per pass: a multiple of 32.
    pub chunk: usize,
    /// Positions a session can hold.
    pub capacity: usize,
    carried: Vec<Carried>,
    /// Floats in a session's buffer.
    state_floats: usize,
    /// Floats of the buffer that are DeltaNet history and state (copied whole).
    delta_floats: usize,
    /// The working buffers between passes.
    work: Mutex<Option<Vec<Buffer>>>,
    time: Mutex<PassTime>,
}

/// `1 + w`, for the zero-centred RMS norms.
fn centred(w: &[f32]) -> Vec<f32> {
    w.iter().map(|w| 1.0 + w).collect()
}

impl GpuModel {
    /// Copies `model`'s weights into GPU memory. `chunk` is the most positions one pass
    /// reads (rounded up to 32); `capacity` the most a session holds.
    ///
    /// # Errors
    /// When there is no Metal device, memory runs out, or the model's shapes are ones
    /// the kernels cannot run.
    pub fn new(mut model: Model, chunk: usize, capacity: usize) -> Result<Self, GpuError> {
        if chunk == 0 || capacity == 0 {
            return Err(GpuError::Invalid(
                "chunk and capacity must be positive".into(),
            ));
        }
        let c = model.config.clone();
        if !c.head_dim.is_multiple_of(32) || c.head_dim > 512 {
            return Err(GpuError::Invalid(format!(
                "attention heads {} wide: the kernel needs a multiple of 32, at most 512",
                c.head_dim
            )));
        }
        if !c.key_dim.is_multiple_of(2) || c.key_dim > 128 || c.value_dim > 128 {
            return Err(GpuError::Invalid(format!(
                "DeltaNet heads {} x {}: the recurrence needs even key heads, both at most 128",
                c.key_dim, c.value_dim
            )));
        }
        let gpu = Gpu::new()?;
        let mut layers = Vec::with_capacity(model.layers.len());
        for layer in &model.layers {
            let mixer = match &layer.mixer {
                Mixer::Delta(d) => {
                    let (lora_in, mut m) = group(&gpu, &[&d.qkv, &d.z, &d.b, &d.a])?;
                    let (lora_out, out) = single(&gpu, &d.out)?;
                    let (a, b, z, qkv) = (m.pop(), m.pop(), m.pop(), m.pop());
                    GpuMixer::Delta(GpuDelta {
                        lora_in,
                        qkv: qkv.expect("four"),
                        z: z.expect("four"),
                        b: b.expect("four"),
                        a: a.expect("four"),
                        conv: floats(&gpu, &d.conv)?,
                        a_log: floats(&gpu, &d.a_log)?,
                        dt_bias: floats(&gpu, &d.dt_bias)?,
                        norm: floats(&gpu, &d.norm)?,
                        lora_out,
                        out,
                    })
                }
                Mixer::Attention(a) => {
                    let (lora_in, mut m) = group(&gpu, &[&a.q, &a.k, &a.v])?;
                    let (lora_o, o) = single(&gpu, &a.o)?;
                    let (v, k, q) = (m.pop(), m.pop(), m.pop());
                    GpuMixer::Attention(GpuAttention {
                        lora_in,
                        q: q.expect("three"),
                        k: k.expect("three"),
                        v: v.expect("three"),
                        lora_o,
                        o,
                        q_norm: floats(&gpu, &centred(&a.q_norm))?,
                        k_norm: floats(&gpu, &centred(&a.k_norm))?,
                    })
                }
            };
            let (lora_mlp, mut m) = group(&gpu, &[&layer.gate, &layer.up])?;
            let (lora_down, down) = single(&gpu, &layer.down)?;
            let (up, gate) = (m.pop(), m.pop());
            layers.push(GpuLayer {
                input_norm: floats(&gpu, &centred(&layer.input_norm))?,
                post_norm: floats(&gpu, &centred(&layer.post_norm))?,
                mixer,
                lora_mlp,
                gate: gate.expect("two"),
                up: up.expect("two"),
                lora_down,
                down,
            });
        }
        // The GPU has its own copies now; the CPU model keeps the embedding.
        model.layers.clear();
        let norm = floats(&gpu, &centred(&model.norm))?;

        let chunk = chunk.next_multiple_of(32);
        let capacity = capacity.next_multiple_of(32);
        let channels = 2 * c.key_heads * c.key_dim + c.value_heads * c.value_dim;
        let mut carried = Vec::with_capacity(c.layers.len());
        let mut at = 0;
        // DeltaNet layers first, so a copy takes them in one piece.
        for kind in &c.layers {
            if *kind == LayerKind::Linear {
                let history = at;
                let state = history + channels * (c.conv_kernel - 1);
                at = state + c.value_heads * c.key_dim * c.value_dim;
                carried.push(Some(Carried::Delta { history, state }));
            } else {
                carried.push(None);
            }
        }
        let delta_floats = at;
        let row = c.kv_heads * c.head_dim;
        let carried = carried
            .into_iter()
            .map(|slot| {
                slot.unwrap_or_else(|| {
                    let keys = at;
                    let values = keys + capacity * row;
                    at = values + capacity * row;
                    Carried::Attention { keys, values }
                })
            })
            .collect();
        Ok(Self {
            config: c,
            model,
            gpu,
            proactor: new_platform_proactor()?,
            layers,
            norm,
            chunk,
            capacity,
            carried,
            state_floats: at,
            delta_floats,
            work: Mutex::new(None),
            time: Mutex::default(),
        })
    }

    /// The GPU's name, for example "Apple M4 Pro".
    #[must_use]
    pub fn device(&self) -> String {
        self.gpu.name()
    }

    /// Time spent in passes since the last call.
    pub fn take_time(&self) -> PassTime {
        std::mem::take(&mut *self.time.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn zeroed(&self, floats: usize) -> Result<Buffer, GpuError> {
        let mut buffer = self.gpu.buffer(floats.max(1) * 4)?;
        buffer.as_bytes_mut().fill(0);
        Ok(buffer)
    }

    /// A session that has read nothing.
    ///
    /// # Errors
    /// When the buffer cannot be allocated.
    pub fn session(&self) -> Result<GpuSession, GpuError> {
        Ok(GpuSession {
            state: Some(self.zeroed(self.state_floats)?),
            len: 0,
        })
    }

    /// Makes `session` one that has read nothing: its DeltaNet state and history zeroed
    /// (cache rows past what a session has read are never looked at).
    ///
    /// # Errors
    /// When the session was lost to a failed pass.
    pub fn reset(&self, session: &mut GpuSession) -> Result<(), GpuError> {
        let state = session.state.as_mut().ok_or_else(lost)?.as_f32_mut();
        state[..self.delta_floats].fill(0.0);
        session.len = 0;
        Ok(())
    }

    /// Makes `into` a copy of `from`: the DeltaNet state whole, and the caches' rows up
    /// to what `from` has read.
    ///
    /// # Errors
    /// When either session was lost to a failed pass.
    pub fn copy_session(&self, from: &GpuSession, into: &mut GpuSession) -> Result<(), GpuError> {
        let src = from.state.as_ref().ok_or_else(lost)?.as_f32();
        let dst = into.state.as_mut().ok_or_else(lost)?.as_f32_mut();
        dst[..self.delta_floats].copy_from_slice(&src[..self.delta_floats]);
        let used = from.len * self.config.kv_heads * self.config.head_dim;
        for carried in &self.carried {
            if let Carried::Attention { keys, values } = *carried {
                dst[keys..keys + used].copy_from_slice(&src[keys..keys + used]);
                dst[values..values + used].copy_from_slice(&src[values..values + used]);
            }
        }
        into.len = from.len;
        Ok(())
    }

    fn work(&self) -> Result<Vec<Buffer>, GpuError> {
        if let Some(work) = self
            .work
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            return Ok(work);
        }
        let c = &self.config;
        let n = self.chunk;
        let channels = 2 * c.key_heads * c.key_dim + c.value_heads * c.value_dim;
        let big = channels.max(c.intermediate).max(2 * c.heads * c.head_dim);
        let heads_wide = (c.value_heads * c.value_dim)
            .max(c.value_heads * c.key_dim)
            .max(c.heads * c.head_dim);
        let lora_rank = self
            .mats()
            .filter_map(|m| m.lora.as_ref().map(|l| l.1))
            .max()
            .unwrap_or(1);
        let lora_rows = self.mats().map(|m| m.rows).max().unwrap_or(1);
        let sizes = [
            (X, c.hidden),
            (N, c.hidden),
            (BIG1, big),
            (BIG2, big),
            (Q, heads_wide),
            (K, heads_wide),
            (V, heads_wide),
            (Z, heads_wide),
            (O, heads_wide),
            (BETA, c.value_heads),
            (A, c.value_heads),
            (ALPHA, c.value_heads),
            (M, c.hidden),
            (TABLE, c.rotary_dim),
            (LT, lora_rank),
            (LY, lora_rows),
        ];
        sizes
            .iter()
            .enumerate()
            .map(|(i, &(index, width))| {
                debug_assert_eq!(i, index);
                self.zeroed(n * width)
            })
            .collect()
    }

    fn mats(&self) -> impl Iterator<Item = &Mat> {
        self.layers.iter().flat_map(|l| {
            let mixer: Vec<&Mat> = match &l.mixer {
                GpuMixer::Delta(d) => vec![&d.qkv, &d.z, &d.b, &d.a, &d.out],
                GpuMixer::Attention(a) => vec![&a.q, &a.k, &a.v, &a.o],
            };
            mixer.into_iter().chain([&l.gate, &l.up, &l.down])
        })
    }

    /// Reads `ids` after what `session` has read. Returns the final hidden states (after
    /// the final norm) of the new positions listed in `wanted` (indices into `ids`),
    /// one after another.
    ///
    /// # Errors
    /// When the session would pass its capacity, or the GPU fails.
    pub fn read(
        &self,
        session: &mut GpuSession,
        ids: &[u32],
        wanted: &[usize],
    ) -> Result<Vec<f32>, GpuError> {
        if session.len + ids.len() > self.capacity {
            return Err(GpuError::Invalid(format!(
                "{} positions after {} pass the session's {}",
                ids.len(),
                session.len,
                self.capacity
            )));
        }
        let h = self.config.hidden;
        let mut rows = vec![0.0_f32; wanted.len() * h];
        for (piece, chunk) in ids.chunks(self.chunk).enumerate() {
            let first = piece * self.chunk;
            let state = session.state.take().ok_or_else(lost)?;
            let mut set = self.work()?;
            self.prepare(&mut set, chunk, session.len);
            set.push(state);
            // A failed pass drops its buffers, and with them the session's state.
            let mut set = self.pass(set, chunk.len(), session.len)?;
            session.state = set.pop();
            session.len += chunk.len();
            let x = set[X].as_f32();
            for (k, &p) in wanted.iter().enumerate() {
                if (first..first + chunk.len()).contains(&p) {
                    let r = p - first;
                    rows[k * h..(k + 1) * h].copy_from_slice(&x[r * h..(r + 1) * h]);
                }
            }
            *self.work.lock().unwrap_or_else(PoisonError::into_inner) = Some(set);
        }
        Ok(rows)
    }

    /// Writes the embeddings and the rotary table for a pass into the working buffers.
    fn prepare(&self, set: &mut [Buffer], ids: &[u32], start: usize) {
        let h = self.config.hidden;
        let x = set[X].as_f32_mut();
        for (p, &id) in ids.iter().enumerate() {
            x[p * h..(p + 1) * h].copy_from_slice(&self.model.embedding(id));
        }
        let half = self.config.rotary_dim / 2;
        let table = set[TABLE].as_f32_mut();
        for p in 0..ids.len() {
            #[allow(clippy::cast_precision_loss)]
            let pos = (start + p) as f32;
            for (i, f) in self.model.inv_freq.iter().enumerate() {
                let (sin, cos) = (pos * f).sin_cos();
                table[(p * half + i) * 2] = cos;
                table[(p * half + i) * 2 + 1] = sin;
            }
        }
    }

    /// One pass of `t` positions starting at `start`, all layers in one submission.
    fn pass(&self, set: Vec<Buffer>, t: usize, start: usize) -> Result<Vec<Buffer>, GpuError> {
        let mut batch = self.gpu.batch(set, Dispatch::Serial)?;
        let c = &self.config;
        let h = c.hidden;
        for (layer, carried) in self.layers.iter().zip(&self.carried) {
            let norm = batch.attach(&layer.input_norm);
            batch.copy_rows((f(X, 0, h, t), h), (f(N, 0, h, t), h), t, h)?;
            batch.rmsnorm_rows((f(N, 0, h, t), norm), t, (h, h), c.rms_eps)?;
            match (&layer.mixer, *carried) {
                (GpuMixer::Delta(d), Carried::Delta { history, state }) => {
                    self.encode_delta(&mut batch, d, t, (history, state))?;
                }
                (GpuMixer::Attention(a), Carried::Attention { keys, values }) => {
                    self.encode_attention(&mut batch, a, t, start, (keys, values))?;
                }
                _ => unreachable!("carried state follows the layer list"),
            }
            batch.add_rows(f(X, 0, h, t), (f(M, 0, h, t), h), t, h)?;

            let norm = batch.attach(&layer.post_norm);
            batch.copy_rows((f(X, 0, h, t), h), (f(N, 0, h, t), h), t, h)?;
            batch.rmsnorm_rows((f(N, 0, h, t), norm), t, (h, h), c.rms_eps)?;
            let fw = c.intermediate;
            self.lora_down(&mut batch, layer.lora_mlp.as_ref(), N, t)?;
            self.mul(&mut batch, &layer.gate, N, BIG1, t)?;
            self.mul(&mut batch, &layer.up, N, BIG2, t)?;
            batch.silu_mul(f(BIG1, 0, fw, t), f(BIG2, 0, fw, t), t * fw)?;
            self.lora_down(&mut batch, layer.lora_down.as_ref(), BIG1, t)?;
            self.mul(&mut batch, &layer.down, BIG1, M, t)?;
            batch.add_rows(f(X, 0, h, t), (f(M, 0, h, t), h), t, h)?;
        }
        let norm = batch.attach(&self.norm);
        batch.rmsnorm_rows((f(X, 0, h, t), norm), t, (h, h), c.rms_eps)?;
        self.run(batch, t)
    }

    /// `y = W x` for `t` positions, packed rows of `x` in buffer `xb` into buffer `yb`. On
    /// the matrix units when the shape allows, over `t` rounded up to 32 positions (the
    /// buffers hold whole chunks).
    fn product(
        batch: &mut Batch<'_>,
        w: Slice,
        (xb, yb): (usize, usize),
        (rows, cols): (usize, usize),
        t: usize,
    ) -> Result<(), GpuError> {
        if t >= 32 && rows.is_multiple_of(64) && cols.is_multiple_of(32) {
            let n = t.next_multiple_of(32);
            batch.gemm_bf16_tiled(
                w,
                f(xb, 0, cols, n),
                f(yb, 0, rows, n),
                rows,
                cols,
                n,
                (cols, rows),
            )?;
        } else {
            batch.gemm_bf16(
                w,
                f(xb, 0, cols, t),
                f(yb, 0, rows, t),
                rows,
                cols,
                t,
                (cols, rows),
            )?;
        }
        Ok(())
    }

    /// The stacked `A x` of a group's adapters, into `LT`, for the group's [`Self::mul`]s.
    fn lora_down(
        &self,
        batch: &mut Batch<'_>,
        down: Option<&LoraDown>,
        xb: usize,
        t: usize,
    ) -> Result<(), GpuError> {
        if let Some(d) = down {
            let a = batch.attach(&d.a);
            Self::product(batch, a, (xb, LT), (d.width, d.cols), t)?;
        }
        Ok(())
    }

    /// `y = W x` for `t` positions of `x` in buffer `xb` into buffer `yb`, plus the
    /// adapter's update `B (A x)` from the `A x` that [`Self::lora_down`] left in `LT`.
    fn mul(
        &self,
        batch: &mut Batch<'_>,
        m: &Mat,
        xb: usize,
        yb: usize,
        t: usize,
    ) -> Result<(), GpuError> {
        let w = batch.attach(&m.w);
        Self::product(batch, w, (xb, yb), (m.rows, m.cols), t)?;
        if let Some((b, width)) = &m.lora {
            let b = batch.attach(b);
            Self::product(batch, b, (LT, LY), (m.rows, *width), t)?;
            batch.add_rows(
                f(yb, 0, m.rows, t),
                (f(LY, 0, m.rows, t), m.rows),
                t,
                m.rows,
            )?;
        }
        Ok(())
    }

    fn encode_delta(
        &self,
        batch: &mut Batch<'_>,
        d: &GpuDelta,
        t: usize,
        (history, state): (usize, usize),
    ) -> Result<(), GpuError> {
        let c = &self.config;
        let (kh, vh, dk, dv, kernel) = (
            c.key_heads,
            c.value_heads,
            c.key_dim,
            c.value_dim,
            c.conv_kernel,
        );
        let channels = 2 * kh * dk + vh * dv;
        let group = vh / kh;
        let history = Slice::new(STATE, history * 4, channels * (kernel - 1) * 4);
        let state = Slice::new(STATE, state * 4, vh * dk * dv * 4);

        // q k v, the convolution over the history, then the history moved on.
        self.lora_down(batch, d.lora_in.as_ref(), N, t)?;
        self.mul(batch, &d.qkv, N, BIG1, t)?;
        let taps = batch.attach(&d.conv);
        batch.causal_conv_silu(
            (f(BIG1, 0, channels, t), taps, history),
            f(BIG2, 0, channels, t),
            t,
            channels,
            kernel,
        )?;
        batch.causal_conv_history(f(BIG1, 0, channels, t), history, t, channels, kernel)?;

        // q and k per value head (a key head serves `group` value heads), v as it is.
        let column = |offset: usize, width: usize| {
            Slice::new(BIG2, offset * 4, ((t - 1) * channels + width) * 4)
        };
        if group == 1 {
            batch.copy_rows(
                (column(0, kh * dk), channels),
                (f(Q, 0, vh * dk, t), vh * dk),
                t,
                kh * dk,
            )?;
            batch.copy_rows(
                (column(kh * dk, kh * dk), channels),
                (f(K, 0, vh * dk, t), vh * dk),
                t,
                kh * dk,
            )?;
        } else {
            for head in 0..vh {
                let key_head = head / group;
                let dst = Slice::new(Q, head * dk * 4, ((t - 1) * vh * dk + dk) * 4);
                batch.copy_rows((column(key_head * dk, dk), channels), (dst, vh * dk), t, dk)?;
                let dst = Slice::new(K, head * dk * 4, ((t - 1) * vh * dk + dk) * 4);
                batch.copy_rows(
                    (column(kh * dk + key_head * dk, dk), channels),
                    (dst, vh * dk),
                    t,
                    dk,
                )?;
            }
        }
        batch.copy_rows(
            (column(2 * kh * dk, vh * dv), channels),
            (f(V, 0, vh * dv, t), vh * dv),
            t,
            vh * dv,
        )?;
        #[allow(clippy::cast_precision_loss)]
        let q_scale = 1.0 / (dk as f32).sqrt();
        batch.l2norm_rows(f(Q, 0, vh * dk, t), t * vh, dk, 1e-6, q_scale)?;
        batch.l2norm_rows(f(K, 0, vh * dk, t), t * vh, dk, 1e-6, 1.0)?;

        // beta = sigmoid(b); the decay exp(-exp(A_log) softplus(a + dt_bias)).
        self.mul(batch, &d.z, N, Z, t)?;
        self.mul(batch, &d.b, N, BETA, t)?;
        self.mul(batch, &d.a, N, A, t)?;
        batch.sigmoid_in_place(f(BETA, 0, vh, t), t * vh)?;
        let (a_log, dt_bias) = (batch.attach(&d.a_log), batch.attach(&d.dt_bias));
        batch.softplus_decay(
            (f(A, 0, vh, t), a_log, dt_bias),
            f(ALPHA, 0, vh, t),
            t * vh,
            vh,
            1,
        )?;
        batch.delta_rule_recurrence_head_decay(
            (
                f(Q, 0, vh * dk, t),
                f(K, 0, vh * dk, t),
                f(V, 0, vh * dv, t),
                f(ALPHA, 0, vh, t),
                f(BETA, 0, vh, t),
            ),
            state,
            f(O, 0, vh * dv, t),
            RecurrenceShape {
                t,
                heads: vh,
                dk,
                dv,
            },
        )?;

        // The read, RMS-normalized per head and gated by silu(z), then projected.
        let norm = batch.attach(&d.norm);
        batch.rmsnorm_rows((f(O, 0, vh * dv, t), norm), t * vh, (dv, dv), c.rms_eps)?;
        batch.silu_mul(f(Z, 0, vh * dv, t), f(O, 0, vh * dv, t), t * vh * dv)?;
        self.lora_down(batch, d.lora_out.as_ref(), Z, t)?;
        self.mul(batch, &d.out, Z, M, t)
    }

    fn encode_attention(
        &self,
        batch: &mut Batch<'_>,
        a: &GpuAttention,
        t: usize,
        start: usize,
        (keys, values): (usize, usize),
    ) -> Result<(), GpuError> {
        let c = &self.config;
        let (heads, kvh, hd) = (c.heads, c.kv_heads, c.head_dim);
        let row = kvh * hd;
        let end = start + t;

        // Each head's query and gate sit side by side in the projection.
        self.lora_down(batch, a.lora_in.as_ref(), N, t)?;
        self.mul(batch, &a.q, N, BIG1, t)?;
        let qg = |offset: usize| Slice::new(BIG1, offset * 4, ((t * heads - 1) * 2 * hd + hd) * 4);
        batch.copy_rows((qg(0), 2 * hd), (f(Q, 0, hd, t * heads), hd), t * heads, hd)?;
        batch.copy_rows(
            (qg(hd), 2 * hd),
            (f(Z, 0, hd, t * heads), hd),
            t * heads,
            hd,
        )?;
        self.mul(batch, &a.k, N, K, t)?;
        self.mul(batch, &a.v, N, V, t)?;
        let (q_norm, k_norm) = (batch.attach(&a.q_norm), batch.attach(&a.k_norm));
        batch.rmsnorm_rows(
            (f(Q, 0, hd, t * heads), q_norm),
            t * heads,
            (hd, hd),
            c.rms_eps,
        )?;
        batch.rmsnorm_rows((f(K, 0, hd, t * kvh), k_norm), t * kvh, (hd, hd), c.rms_eps)?;
        let table = f(TABLE, 0, c.rotary_dim, t);
        batch.rotate_halves_partial(f(Q, 0, heads * hd, t), table, t, (heads, hd, c.rotary_dim))?;
        batch.rotate_halves_partial(f(K, 0, row, t), table, t, (kvh, hd, c.rotary_dim))?;

        // The new keys and values into the cache, then attention over all of it.
        let cache = |at: usize, first: usize, rows: usize| {
            Slice::new(STATE, (at + first * row) * 4, rows * row * 4)
        };
        batch.copy_rows((f(K, 0, row, t), row), (cache(keys, start, t), row), t, row)?;
        batch.copy_rows(
            (f(V, 0, row, t), row),
            (cache(values, start, t), row),
            t,
            row,
        )?;
        #[allow(clippy::cast_precision_loss)]
        let scale = 1.0 / (hd as f32).sqrt();
        batch.attention_grouped(
            f(Q, 0, heads * hd, t),
            cache(keys, 0, end),
            cache(values, 0, end),
            f(O, 0, heads * hd, t),
            GroupedShape {
                t,
                start,
                heads,
                kv_heads: kvh,
                dim: hd,
                window: usize::MAX,
                slots: self.capacity,
                scale,
            },
        )?;
        batch.sigmoid_gate(
            f(O, 0, heads * hd, t),
            f(Z, 0, heads * hd, t),
            t * heads * hd,
        )?;
        self.lora_down(batch, a.lora_o.as_ref(), O, t)?;
        self.mul(batch, &a.o, O, M, t)
    }

    /// Commits `batch` and runs the proactor until the GPU has finished it.
    fn run(&self, batch: Batch<'_>, t: usize) -> Result<Vec<Buffer>, GpuError> {
        let slot: Arc<Mutex<Option<Completed>>> = Arc::default();
        let filled = Arc::clone(&slot);
        let committed = std::time::Instant::now();
        batch.commit(&self.proactor.handle(), move |done| {
            *filled.lock().unwrap_or_else(PoisonError::into_inner) = Some(done);
        });
        loop {
            self.proactor.run_once()?;
            let done = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
            if let Some(done) = done {
                let gpu = done.gpu_time?;
                let mut time = self.time.lock().unwrap_or_else(PoisonError::into_inner);
                time.passes += 1;
                time.positions += t;
                time.gpu += gpu;
                time.wall += committed.elapsed();
                return Ok(done.buffers);
            }
        }
    }
}
