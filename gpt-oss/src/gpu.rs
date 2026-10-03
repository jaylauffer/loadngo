//! gpt-oss's forward pass on Apple GPUs, through `loadngo-metal-compute`.
//!
//! The weights are copied once into GPU memory in their file formats: `Q8_0` repacked
//! into signed-byte rows and binary16 scales, and MXFP4 as `Mxfp4Matrix` lays it out. A
//! layer's experts sit one after another in one buffer per matrix and bias, so the GPU
//! can pick them by index. Every submission finishes as a completion on a loadngo
//! proactor.
//!
//! - **One new position (decoding)** is one submission for the whole token. For each
//!   layer, the GPU does:
//!   - attention (below);
//!   - the router and its top-k with softmax;
//!   - the chosen experts' products, picked by index;
//!   - the weighted combine into the residual.
//!
//!   Then the output.
//! - **Several positions (a prompt)** take two submissions per layer:
//!   1. Attention: norm; q, k and v with biases; rotary; the new keys and values into
//!      the layer's cache; grouped attention with sinks; the output projection and its
//!      bias added to the residual; the norm before the experts.
//!   2. Experts: the CPU routes each position from the normalized rows, read straight
//!      from shared memory, and gathers them by expert. The GPU computes every chosen
//!      expert. The CPU adds the weighted results to the residual.
//!
//! Sliding layers keep their keys and values in a ring of `sliding_window + chunk` rows
//! (rounded up to 32). Full layers keep `max_context` rows. From 32 positions the
//! products and attention run on the matrix units.
//!
//! [`crate::model::Model`] stays the reference: `tests/gpu_oracle.rs` holds this path to
//! transformers, both ways of routing.

use std::cell::RefCell;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use loadngo_metal_compute::{
    Batch, Buffer, Completed, Dispatch, Gpu, GroupedShape, Resident, Slice,
};
use loadngo_proactor::{new_platform_proactor, PlatformPort, Proactor};
use loadngo_weights::q8_0::split_blocks;

use crate::model::{Format, Matrix, Model};

/// The clamp and the sigmoid's slope in gpt-oss's SwiGLU.
const SWIGLU: (f32, f32) = (7.0, 1.702);

#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    #[error(transparent)]
    Metal(#[from] loadngo_metal_compute::Error),
    #[error("proactor: {0}")]
    Proactor(#[from] std::io::Error),
    #[error("{0}")]
    Invalid(String),
}

/// Bytes `index * len .. + len` of a slice: one expert of several stored in a row.
fn part(s: Slice, index: usize, len: usize) -> Slice {
    Slice::new(s.buffer, s.offset + index * len, len)
}

/// `y[p] = W x[p]` for `n` positions with an MXFP4 matrix given as slices. From 32
/// positions the tiled kernel runs on `n` rounded up to 32 (`x` and `y` must hold that
/// many rows).
fn mul_mxfp4(
    batch: &mut Batch<'_>,
    (e, s): (Slice, Slice),
    (rows, cols): (usize, usize),
    (x, y): (Slice, Slice),
    n: usize,
) -> Result<(), GpuError> {
    let strides = (cols, rows);
    let tiled = n >= 32;
    let n_run = if tiled { n.next_multiple_of(32) } else { n };
    let x = Slice::new(x.buffer, x.offset, n_run * cols * 4);
    let y = Slice::new(y.buffer, y.offset, n_run * rows * 4);
    if n == 1 {
        batch.gemv_mxfp4(e, s, x, y, rows, cols)?;
    } else if tiled {
        batch.gemm_mxfp4_tiled(e, s, x, y, rows, cols, n_run, strides)?;
    } else {
        batch.gemm_mxfp4(e, s, x, y, rows, cols, n, strides)?;
    }
    Ok(())
}

/// A `Q8_0` matrix in GPU memory.
struct Q8 {
    rows: usize,
    cols: usize,
    codes: Arc<Resident>,
    scales: Arc<Resident>,
}

impl Q8 {
    fn new(gpu: &Gpu, m: &Matrix) -> Result<Self, GpuError> {
        let Format::Q8(blocks) = &m.format else {
            return Err(GpuError::Invalid("expected a Q8_0 matrix".into()));
        };
        let (codes, scales) = split_blocks(blocks);
        Ok(Self {
            rows: m.rows,
            cols: m.cols,
            codes: gpu.resident(&codes)?,
            scales: gpu.resident(&scales)?,
        })
    }

    /// As [`mul_mxfp4`].
    fn mul(&self, batch: &mut Batch<'_>, x: Slice, y: Slice, n: usize) -> Result<(), GpuError> {
        let (rows, cols) = (self.rows, self.cols);
        let strides = (cols, rows);
        let tiled = n >= 32;
        let n_run = if tiled { n.next_multiple_of(32) } else { n };
        let x = Slice::new(x.buffer, x.offset, n_run * cols * 4);
        let y = Slice::new(y.buffer, y.offset, n_run * rows * 4);
        let m = (batch.attach(&self.codes), batch.attach(&self.scales));
        if n == 1 {
            batch.gemv_q8_0(m, x, y, rows, cols)?;
        } else if tiled {
            batch.gemm_q8_0_tiled(m, x, y, rows, cols, n_run, strides)?;
        } else {
            batch.gemm_q8_0(m, x, y, rows, cols, n, strides)?;
        }
        Ok(())
    }
}

/// One MXFP4 matrix kind (gate, up or down) for every expert of a layer, one after
/// another.
struct ExpertMatrices {
    rows: usize,
    cols: usize,
    elements: Arc<Resident>,
    scales: Arc<Resident>,
}

impl ExpertMatrices {
    fn new<'m>(gpu: &Gpu, matrices: impl Iterator<Item = &'m Matrix>) -> Result<Self, GpuError> {
        let (mut elements, mut scales, mut shape) = (Vec::new(), Vec::new(), None);
        for m in matrices {
            let Format::Mxfp4 {
                elements: e,
                scales: s,
            } = &m.format
            else {
                return Err(GpuError::Invalid("expected MXFP4 experts".into()));
            };
            if shape.is_some_and(|shape| shape != (m.rows, m.cols)) {
                return Err(GpuError::Invalid("experts of different shapes".into()));
            }
            shape = Some((m.rows, m.cols));
            elements.extend_from_slice(e);
            scales.extend_from_slice(s);
        }
        let (rows, cols) = shape.ok_or_else(|| GpuError::Invalid("no experts".into()))?;
        Ok(Self {
            rows,
            cols,
            elements: gpu.resident(&elements)?,
            scales: gpu.resident(&scales)?,
        })
    }

    /// Every expert's elements and scales, for the selected-product kernel.
    fn all(&self, batch: &mut Batch<'_>) -> (Slice, Slice) {
        (batch.attach(&self.elements), batch.attach(&self.scales))
    }

    /// Expert `e`'s elements and scales.
    fn one(&self, batch: &mut Batch<'_>, e: usize) -> (Slice, Slice) {
        let (elements, scales) = self.all(batch);
        (
            part(elements, e, self.rows * self.cols / 2),
            part(scales, e, self.rows * self.cols / 32),
        )
    }
}

struct Experts {
    count: usize,
    gate: ExpertMatrices,
    up: ExpertMatrices,
    down: ExpertMatrices,
    /// `count x width` each.
    gate_bias: Arc<Resident>,
    up_bias: Arc<Resident>,
    down_bias: Arc<Resident>,
    /// The router, `count x hidden`, and its bias.
    router: Arc<Resident>,
    router_bias: Arc<Resident>,
}

struct GpuLayer {
    attn_norm: Arc<Resident>,
    q: Q8,
    k: Q8,
    v: Q8,
    o: Q8,
    q_bias: Arc<Resident>,
    k_bias: Arc<Resident>,
    v_bias: Arc<Resident>,
    o_bias: Arc<Resident>,
    sinks: Arc<Resident>,
    ffn_norm: Arc<Resident>,
    experts: Experts,
}

fn floats(gpu: &Gpu, v: &[f32]) -> Result<Arc<Resident>, GpuError> {
    let bytes: Vec<u8> = v.iter().flat_map(|f| f.to_le_bytes()).collect();
    Ok(gpu.resident(&bytes)?)
}

/// Shared buffers by position in a session's buffer list.
const H: usize = 0;
const X: usize = 1;
const Q: usize = 2;
const K: usize = 3;
const V: usize = 4;
const ATT: usize = 5;
const TMP: usize = 6;
const TABLE: usize = 7;
const XE: usize = 8;
const GATE: usize = 9;
const UP: usize = 10;
const HIDDEN: usize = 11;
const YE: usize = 12;
const LOGITS: usize = 13;
const ROUTER: usize = 14;
const IDS: usize = 15;
const WEIGHTS: usize = 16;
const CACHES: usize = 17;

/// Where a pass's time went, summed over the passes since the profile was last taken.
#[derive(Clone, Debug, Default)]
pub struct Profile {
    pub passes: usize,
    pub positions: usize,
    /// Per GPU phase: submissions, CPU encoding, GPU execution (Metal's timestamps) and
    /// wall time from commit to the proactor delivering the completion.
    pub phases: [PhaseTime; 4],
    /// CPU work between submissions: embedding and rotary table, routing, gathering
    /// rows by expert, adding expert results to the residual.
    pub prepare: Duration,
    pub route: Duration,
    pub gather: Duration,
    pub scatter: Duration,
    pub total: Duration,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PhaseTime {
    pub submissions: usize,
    pub encode: Duration,
    pub gpu: Duration,
    pub wall: Duration,
}

/// The GPU submissions a pass is made of.
#[derive(Clone, Copy, Debug)]
enum Phase {
    Attention = 0,
    Experts = 1,
    Output = 2,
    /// A whole decoded token: every layer and the output.
    Token = 3,
}

const PHASE_NAMES: [&str; 4] = ["attention", "experts", "output", "token"];

impl fmt::Display for Profile {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        let per = |d: Duration| ms(d) / self.passes.max(1) as f64;
        write!(
            out,
            "{} passes, {} positions, {:.2} ms per pass",
            self.passes,
            self.positions,
            per(self.total)
        )?;
        for (name, p) in PHASE_NAMES.iter().zip(&self.phases) {
            if p.submissions > 0 {
                write!(
                    out,
                    "\n  {name:9} {:5} submissions: encode {:7.2} ms, GPU {:7.2} ms, wall {:7.2} ms per pass",
                    p.submissions,
                    per(p.encode),
                    per(p.gpu),
                    per(p.wall)
                )?;
            }
        }
        write!(
            out,
            "\n  CPU: prepare {:.2}, route {:.2}, gather {:.2}, scatter {:.2} ms per pass",
            per(self.prepare),
            per(self.route),
            per(self.gather),
            per(self.scatter)
        )
    }
}

/// Which logits a pass returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Logits {
    Last,
    All,
}

fn f(buffer: usize, row: usize, width: usize, rows: usize) -> Slice {
    Slice::new(buffer, row * width * 4, rows * width * 4)
}

pub struct GpuModel {
    /// The CPU side: shape, router (for prompts), embedding and rotary frequencies. Its
    /// other matrices are released once they are in GPU memory.
    model: Model,
    gpu: Gpu,
    proactor: Proactor<PlatformPort>,
    layers: Vec<GpuLayer>,
    output: Q8,
    output_norm: Arc<Resident>,
    /// Positions per GPU pass.
    pub chunk: usize,
    /// Rows a full layer's cache holds.
    pub max_context: usize,
    /// Route single positions on the GPU (one submission per token); otherwise on the
    /// CPU, as prompts are.
    pub gpu_routing: bool,
    profile: RefCell<Profile>,
}

/// A conversation on the GPU: its buffers (caches included) and position.
pub struct GpuSession {
    buffers: Vec<Buffer>,
    len: usize,
    slots: Vec<usize>,
}

impl GpuSession {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl GpuModel {
    /// Copies `model`'s weights into GPU memory. `chunk` is the most positions one pass
    /// takes; `max_context` the positions a conversation can reach.
    pub fn new(mut model: Model, chunk: usize, max_context: usize) -> Result<Self, GpuError> {
        if chunk == 0 || max_context == 0 {
            return Err(GpuError::Invalid(
                "chunk and max_context must be positive".into(),
            ));
        }
        let gpu = Gpu::new()?;
        let mut layers = Vec::with_capacity(model.layers.len());
        for layer in &model.layers {
            let list = &layer.experts;
            let concat = |pick: fn(&crate::model::Expert) -> &Vec<f32>| -> Vec<f32> {
                list.iter().flat_map(|e| pick(e).iter().copied()).collect()
            };
            let Format::F32(router) = &layer.router.format else {
                return Err(GpuError::Invalid("the router must be f32".into()));
            };
            let experts = Experts {
                count: list.len(),
                gate: ExpertMatrices::new(&gpu, list.iter().map(|e| &e.gate))?,
                up: ExpertMatrices::new(&gpu, list.iter().map(|e| &e.up))?,
                down: ExpertMatrices::new(&gpu, list.iter().map(|e| &e.down))?,
                gate_bias: floats(&gpu, &concat(|e| &e.gate_bias))?,
                up_bias: floats(&gpu, &concat(|e| &e.up_bias))?,
                down_bias: floats(&gpu, &concat(|e| &e.down_bias))?,
                router: floats(&gpu, router)?,
                router_bias: floats(&gpu, &layer.router_bias)?,
            };
            layers.push(GpuLayer {
                attn_norm: floats(&gpu, &layer.attn_norm)?,
                q: Q8::new(&gpu, &layer.q)?,
                k: Q8::new(&gpu, &layer.k)?,
                v: Q8::new(&gpu, &layer.v)?,
                o: Q8::new(&gpu, &layer.o)?,
                q_bias: floats(&gpu, &layer.q_bias)?,
                k_bias: floats(&gpu, &layer.k_bias)?,
                v_bias: floats(&gpu, &layer.v_bias)?,
                o_bias: floats(&gpu, &layer.o_bias)?,
                sinks: floats(&gpu, &layer.sinks)?,
                ffn_norm: floats(&gpu, &layer.ffn_norm)?,
                experts,
            });
        }
        let output = Q8::new(&gpu, &model.output)?;
        // The GPU has its own copies now.
        for layer in &mut model.layers {
            for m in [&mut layer.q, &mut layer.k, &mut layer.v, &mut layer.o] {
                m.release();
            }
            for e in &mut layer.experts {
                for m in [&mut e.gate, &mut e.up, &mut e.down] {
                    m.release();
                }
            }
        }
        model.output.release();
        Ok(Self {
            output,
            output_norm: floats(&gpu, &model.output_norm)?,
            layers,
            gpu,
            proactor: new_platform_proactor()?,
            chunk,
            max_context: max_context.next_multiple_of(32),
            gpu_routing: true,
            profile: RefCell::default(),
            model,
        })
    }

    pub fn config(&self) -> &crate::config::Config {
        &self.model.config
    }

    pub fn device(&self) -> String {
        self.gpu.name()
    }

    pub fn session(&self) -> Result<GpuSession, GpuError> {
        let c = &self.model.config;
        let n = self.chunk.next_multiple_of(32);
        let q_width = c.heads * c.head_dim;
        let kv_width = c.kv_heads * c.head_dim;
        // Every position is routed to `experts_used` experts; each expert's rows are
        // padded to 32 for the tiled products.
        let assigned = n * c.experts_used + 32 * c.experts;
        let zeroed = |gpu: &Gpu, len: usize| -> Result<Buffer, GpuError> {
            let mut buffer = gpu.buffer(len.max(16))?;
            buffer.as_bytes_mut().fill(0);
            Ok(buffer)
        };
        let g = &self.gpu;
        let mut buffers = vec![
            zeroed(g, n * c.hidden * 4)?,
            zeroed(g, n * c.hidden * 4)?,
            zeroed(g, n * q_width * 4)?,
            zeroed(g, n * kv_width * 4)?,
            zeroed(g, n * kv_width * 4)?,
            zeroed(g, n * q_width * 4)?,
            zeroed(g, n * c.hidden * 4)?,
            zeroed(g, n * c.head_dim * 4)?,
            zeroed(g, assigned * c.hidden * 4)?,
            zeroed(g, assigned * c.expert_hidden * 4)?,
            zeroed(g, assigned * c.expert_hidden * 4)?,
            zeroed(g, assigned * c.expert_hidden * 4)?,
            zeroed(g, assigned * c.hidden * 4)?,
            zeroed(g, 32 * c.vocab * 4)?,
            zeroed(g, c.experts * 4)?,
            zeroed(g, c.experts_used * 4)?,
            zeroed(g, c.experts_used * 4)?,
        ];
        let mut slots = Vec::with_capacity(c.layers);
        for l in 0..c.layers {
            let rows = if c.is_sliding(l) {
                (c.sliding_window + self.chunk).next_multiple_of(32)
            } else {
                self.max_context
            };
            buffers.push(zeroed(g, rows * kv_width * 4)?);
            buffers.push(zeroed(g, rows * kv_width * 4)?);
            slots.push(rows);
        }
        Ok(GpuSession {
            buffers,
            len: 0,
            slots,
        })
    }

    /// The time spent since the last call, by phase.
    pub fn take_profile(&self) -> Profile {
        self.profile.take()
    }

    /// Commits `batch`, encoded since `encoding`, and waits on the proactor until the
    /// GPU has finished; the time goes to `phase`.
    fn run(
        &self,
        batch: Batch<'_>,
        phase: Phase,
        encoding: Instant,
    ) -> Result<Vec<Buffer>, GpuError> {
        let committed = Instant::now();
        let slot: Arc<Mutex<Option<Completed>>> = Arc::default();
        let filled = Arc::clone(&slot);
        batch.commit(&self.proactor.handle(), move |done| {
            *filled
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(done);
        });
        loop {
            self.proactor.run_once()?;
            let done = slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(done) = done {
                let gpu = done.gpu_time?;
                let mut profile = self.profile.borrow_mut();
                let time = &mut profile.phases[phase as usize];
                time.submissions += 1;
                time.encode += committed - encoding;
                time.gpu += gpu;
                time.wall += committed.elapsed();
                return Ok(done.buffers);
            }
        }
    }

    /// Feeds `tokens` at the session's next positions, in passes of at most `chunk`, and
    /// returns the logits after the last one, or after each.
    pub fn feed(
        &self,
        session: &mut GpuSession,
        tokens: &[u32],
        which: Logits,
    ) -> Result<Vec<Vec<f32>>, GpuError> {
        if session.len + tokens.len() > self.max_context {
            return Err(GpuError::Invalid(format!(
                "{} positions exceed the context of {}",
                session.len + tokens.len(),
                self.max_context
            )));
        }
        let mut out = Vec::new();
        for (i, part) in tokens.chunks(self.chunk).enumerate() {
            let last = (i + 1) * self.chunk >= tokens.len();
            let want = match which {
                Logits::All => Some(Logits::All),
                Logits::Last => last.then_some(Logits::Last),
            };
            out.extend(self.pass(session, part, want)?);
        }
        Ok(out)
    }

    /// Writes the embedding rows of `tokens` and the rotary table for positions
    /// `start..`.
    fn prepare(&self, buffers: &mut [Buffer], tokens: &[u32], start: usize) {
        let c = &self.model.config;
        let hidden = c.hidden;
        for (p, &token) in tokens.iter().enumerate() {
            self.model
                .embed(token, &mut buffers[H].as_f32_mut()[p * hidden..][..hidden]);
        }
        let half = c.head_dim / 2;
        let table = buffers[TABLE].as_f32_mut();
        for p in 0..tokens.len() {
            for (i, &freq) in self.model.inv_freq.iter().enumerate() {
                let (sin, cos) = ((start + p) as f64 * freq).sin_cos();
                table[(p * half + i) * 2] = (cos * self.model.rope_scale) as f32;
                table[(p * half + i) * 2 + 1] = (sin * self.model.rope_scale) as f32;
            }
        }
    }

    /// Layer `l`'s attention for `n` new positions from `start`, its output added to the
    /// residual `H`, then the experts' norm of `H` into `X`.
    fn encode_attention(
        &self,
        batch: &mut Batch<'_>,
        l: usize,
        n: usize,
        start: usize,
        slots: usize,
    ) -> Result<(), GpuError> {
        let c = &self.model.config;
        let layer = &self.layers[l];
        let (hidden, q_width, kv_width) = (c.hidden, c.heads * c.head_dim, c.kv_heads * c.head_dim);
        let (kc, vc) = (CACHES + 2 * l, CACHES + 2 * l + 1);
        let rows_of = |b: usize, w: usize| f(b, 0, w, n);
        batch.copy_rows(
            (rows_of(H, hidden), hidden),
            (rows_of(X, hidden), hidden),
            n,
            hidden,
        )?;
        let norm = batch.attach(&layer.attn_norm);
        batch.rmsnorm_rows((rows_of(X, hidden), norm), n, (hidden, hidden), c.rms_eps)?;
        layer
            .q
            .mul(batch, rows_of(X, hidden), rows_of(Q, q_width), n)?;
        layer
            .k
            .mul(batch, rows_of(X, hidden), rows_of(K, kv_width), n)?;
        layer
            .v
            .mul(batch, rows_of(X, hidden), rows_of(V, kv_width), n)?;
        for (buffer, bias, width) in [
            (Q, &layer.q_bias, q_width),
            (K, &layer.k_bias, kv_width),
            (V, &layer.v_bias, kv_width),
        ] {
            let bias = batch.attach(bias);
            batch.add_rows(rows_of(buffer, width), (bias, 0), n, width)?;
        }
        let table = rows_of(TABLE, c.head_dim);
        batch.rotate_halves(rows_of(Q, q_width), table, n, (c.heads, c.head_dim))?;
        batch.rotate_halves(rows_of(K, kv_width), table, n, (c.kv_heads, c.head_dim))?;
        // The new rows into the cache: position s goes to row s % slots, in at most two
        // runs.
        let mut done = 0;
        while done < n {
            let row = (start + done) % slots;
            let run = (n - done).min(slots - row);
            for (src, dst) in [(K, kc), (V, vc)] {
                batch.copy_rows(
                    (f(src, done, kv_width, run), kv_width),
                    (f(dst, row, kv_width, run), kv_width),
                    run,
                    kv_width,
                )?;
            }
            done += run;
        }
        let shape = GroupedShape {
            t: n,
            start,
            heads: c.heads,
            kv_heads: c.kv_heads,
            dim: c.head_dim,
            window: if c.is_sliding(l) {
                c.sliding_window
            } else {
                usize::MAX
            },
            slots,
            scale: 1.0 / (c.head_dim as f32).sqrt(),
        };
        let sinks = batch.attach(&layer.sinks);
        if n >= 32 {
            let padded = n.next_multiple_of(32);
            let end = start + n;
            // Both are multiples of 32, so the rounded rows stay inside the cache.
            let rows = if slots < end {
                slots
            } else {
                end.next_multiple_of(32)
            };
            batch.attention_grouped_tiled_with_sinks(
                (
                    f(Q, 0, q_width, padded),
                    f(kc, 0, kv_width, rows),
                    f(vc, 0, kv_width, rows),
                ),
                sinks,
                f(ATT, 0, q_width, padded),
                shape,
            )?;
        } else {
            let rows = slots.min(start + n);
            batch.attention_grouped_with_sinks(
                (
                    rows_of(Q, q_width),
                    f(kc, 0, kv_width, rows),
                    f(vc, 0, kv_width, rows),
                ),
                sinks,
                rows_of(ATT, q_width),
                shape,
            )?;
        }
        layer
            .o
            .mul(batch, rows_of(ATT, q_width), rows_of(TMP, hidden), n)?;
        let o_bias = batch.attach(&layer.o_bias);
        batch.add_rows(rows_of(TMP, hidden), (o_bias, 0), n, hidden)?;
        batch.add_rows(
            rows_of(H, hidden),
            (rows_of(TMP, hidden), hidden),
            n,
            hidden,
        )?;
        batch.copy_rows(
            (rows_of(H, hidden), hidden),
            (rows_of(X, hidden), hidden),
            n,
            hidden,
        )?;
        let norm = batch.attach(&layer.ffn_norm);
        batch.rmsnorm_rows((rows_of(X, hidden), norm), n, (hidden, hidden), c.rms_eps)?;
        Ok(())
    }

    /// Layer `l`'s experts for the one position in `X`, routed on the GPU, added to `H`.
    fn encode_routed_experts(&self, batch: &mut Batch<'_>, l: usize) -> Result<(), GpuError> {
        let c = &self.model.config;
        let e = &self.layers[l].experts;
        let (hidden, eh, k, count) = (c.hidden, c.expert_hidden, c.experts_used, e.count);
        let router = (batch.attach(&e.router), Some(batch.attach(&e.router_bias)));
        batch.gemv_f32(
            router,
            f(X, 0, hidden, 1),
            f(ROUTER, 0, count, 1),
            count,
            hidden,
        )?;
        let (ids, weights) = (f(IDS, 0, k, 1), f(WEIGHTS, 0, k, 1));
        batch.topk_softmax(f(ROUTER, 0, count, 1), (ids, weights), 1, (count, k))?;
        let (ge, gs) = e.gate.all(batch);
        batch.gemv_mxfp4_selected(
            (ge, gs, count),
            ids,
            (f(X, 0, hidden, 1), 0),
            f(GATE, 0, eh, k),
            (eh, hidden),
            k,
        )?;
        let (ue, us) = e.up.all(batch);
        batch.gemv_mxfp4_selected(
            (ue, us, count),
            ids,
            (f(X, 0, hidden, 1), 0),
            f(UP, 0, eh, k),
            (eh, hidden),
            k,
        )?;
        let (gb, ub) = (batch.attach(&e.gate_bias), batch.attach(&e.up_bias));
        batch.clamped_swiglu_selected(
            (f(GATE, 0, eh, k), f(UP, 0, eh, k)),
            (gb, ub, count),
            ids,
            f(HIDDEN, 0, eh, k),
            (k, eh),
            SWIGLU,
        )?;
        let (de, ds) = e.down.all(batch);
        batch.gemv_mxfp4_selected(
            (de, ds, count),
            ids,
            (f(HIDDEN, 0, eh, k), eh),
            f(YE, 0, hidden, k),
            (hidden, eh),
            k,
        )?;
        let db = batch.attach(&e.down_bias);
        batch.moe_combine(
            f(H, 0, hidden, 1),
            (f(YE, 0, hidden, k), db, count),
            (ids, weights),
            (k, hidden),
        )?;
        Ok(())
    }

    /// The final norm of `rows` rows of `H` from `first`, and their logits into `LOGITS`.
    fn encode_output(
        &self,
        batch: &mut Batch<'_>,
        first: usize,
        rows: usize,
    ) -> Result<(), GpuError> {
        let c = &self.model.config;
        let hidden = c.hidden;
        batch.copy_rows(
            (f(H, first, hidden, rows), hidden),
            (f(X, 0, hidden, rows), hidden),
            rows,
            hidden,
        )?;
        let norm = batch.attach(&self.output_norm);
        batch.rmsnorm_rows(
            (f(X, 0, hidden, rows), norm),
            rows,
            (hidden, hidden),
            c.rms_eps,
        )?;
        self.output.mul(
            batch,
            f(X, 0, hidden, rows),
            f(LOGITS, 0, c.vocab, rows),
            rows,
        )
    }

    fn pass(
        &self,
        session: &mut GpuSession,
        tokens: &[u32],
        which: Option<Logits>,
    ) -> Result<Vec<Vec<f32>>, GpuError> {
        let began = Instant::now();
        let n = tokens.len();
        let start = session.len;
        let mut buffers = std::mem::take(&mut session.buffers);
        self.prepare(&mut buffers, tokens, start);
        self.profile.borrow_mut().prepare += began.elapsed();
        let vocab = self.model.config.vocab;
        let mut out = Vec::new();
        if n == 1 && self.gpu_routing {
            // The whole token in one submission.
            let encoding = Instant::now();
            let mut batch = self.gpu.batch(buffers, Dispatch::Serial)?;
            for l in 0..self.layers.len() {
                self.encode_attention(&mut batch, l, 1, start, session.slots[l])?;
                self.encode_routed_experts(&mut batch, l)?;
            }
            if which.is_some() {
                self.encode_output(&mut batch, 0, 1)?;
            }
            buffers = self.run(batch, Phase::Token, encoding)?;
            if which.is_some() {
                out.push(buffers[LOGITS].as_f32()[..vocab].to_vec());
            }
        } else {
            for l in 0..self.layers.len() {
                let encoding = Instant::now();
                let mut batch = self.gpu.batch(buffers, Dispatch::Serial)?;
                self.encode_attention(&mut batch, l, n, start, session.slots[l])?;
                buffers = self.run(batch, Phase::Attention, encoding)?;
                buffers = self.experts_routed_on_cpu(buffers, l, n)?;
            }
            // Logits for the last position or all of them, at most 32 rows a submission.
            let wanted = match which {
                None => 0..0,
                Some(Logits::Last) => n - 1..n,
                Some(Logits::All) => 0..n,
            };
            for first in wanted.clone().step_by(32) {
                let rows = (wanted.end - first).min(32);
                let encoding = Instant::now();
                let mut batch = self.gpu.batch(buffers, Dispatch::Serial)?;
                self.encode_output(&mut batch, first, rows)?;
                buffers = self.run(batch, Phase::Output, encoding)?;
                let logits = buffers[LOGITS].as_f32();
                out.extend((0..rows).map(|r| logits[r * vocab..][..vocab].to_vec()));
            }
        }
        session.buffers = buffers;
        session.len += n;
        let mut profile = self.profile.borrow_mut();
        profile.passes += 1;
        profile.positions += n;
        profile.total += began.elapsed();
        Ok(out)
    }

    /// Layer `l`'s experts for `n` positions: routed on the CPU from `X`, gathered by
    /// expert, computed on the GPU, added to `H` on the CPU.
    fn experts_routed_on_cpu(
        &self,
        mut buffers: Vec<Buffer>,
        l: usize,
        n: usize,
    ) -> Result<Vec<Buffer>, GpuError> {
        let c = &self.model.config;
        let (hidden, eh) = (c.hidden, c.expert_hidden);
        let routing = Instant::now();
        let routes = route_all(
            &self.model,
            &self.model.layers[l],
            buffers[X].as_f32(),
            n,
            hidden,
        );
        let gathering = Instant::now();
        self.profile.borrow_mut().route += gathering - routing;
        let mut by_expert: Vec<Vec<(usize, f32)>> = vec![Vec::new(); c.experts];
        for (p, route) in routes.iter().enumerate() {
            for &(e, w) in route {
                by_expert[e].push((p, w));
            }
        }
        // Each used expert's rows start at `base`, padded to 32 when tiled.
        let mut plan = Vec::new();
        let mut base = 0;
        for (e, rows) in by_expert.iter().enumerate() {
            if !rows.is_empty() {
                let m = rows.len();
                plan.push((e, base, m));
                base += if m >= 32 { m.next_multiple_of(32) } else { m };
            }
        }
        {
            let (x, xe) = two(&mut buffers, X, XE);
            let (x, xe) = (x.as_f32(), xe.as_f32_mut());
            for &(e, at, _) in &plan {
                for (i, &(p, _)) in by_expert[e].iter().enumerate() {
                    xe[(at + i) * hidden..][..hidden].copy_from_slice(&x[p * hidden..][..hidden]);
                }
            }
        }
        let encoding = Instant::now();
        self.profile.borrow_mut().gather += encoding - gathering;
        let experts = &self.layers[l].experts;
        let mut batch = self.gpu.batch(buffers, Dispatch::Serial)?;
        for &(e, at, m) in &plan {
            let run = if m >= 32 { m.next_multiple_of(32) } else { m };
            let gate = experts.gate.one(&mut batch, e);
            mul_mxfp4(
                &mut batch,
                gate,
                (eh, hidden),
                (f(XE, at, hidden, run), f(GATE, at, eh, run)),
                m,
            )?;
            let up = experts.up.one(&mut batch, e);
            mul_mxfp4(
                &mut batch,
                up,
                (eh, hidden),
                (f(XE, at, hidden, run), f(UP, at, eh, run)),
                m,
            )?;
            let gb = part(batch.attach(&experts.gate_bias), e, eh * 4);
            let ub = part(batch.attach(&experts.up_bias), e, eh * 4);
            batch.clamped_swiglu(
                (f(GATE, at, eh, m), f(UP, at, eh, m)),
                (gb, ub),
                f(HIDDEN, at, eh, m),
                m,
                eh,
                SWIGLU,
            )?;
            let down = experts.down.one(&mut batch, e);
            mul_mxfp4(
                &mut batch,
                down,
                (hidden, eh),
                (f(HIDDEN, at, eh, run), f(YE, at, hidden, run)),
                m,
            )?;
            let db = part(batch.attach(&experts.down_bias), e, hidden * 4);
            batch.add_rows(f(YE, at, hidden, m), (db, 0), m, hidden)?;
        }
        let mut buffers = self.run(batch, Phase::Experts, encoding)?;
        let scattering = Instant::now();
        {
            let (h, ye) = two(&mut buffers, H, YE);
            let (h, ye) = (h.as_f32_mut(), ye.as_f32());
            for &(e, at, _) in &plan {
                for (i, &(p, w)) in by_expert[e].iter().enumerate() {
                    for (o, &v) in h[p * hidden..][..hidden]
                        .iter_mut()
                        .zip(&ye[(at + i) * hidden..][..hidden])
                    {
                        *o += w * v;
                    }
                }
            }
        }
        self.profile.borrow_mut().scatter += scattering.elapsed();
        Ok(buffers)
    }
}

/// Two distinct buffers of a list, mutably.
fn two(buffers: &mut [Buffer], a: usize, b: usize) -> (&mut Buffer, &mut Buffer) {
    assert!(a < b, "two() takes buffers in order");
    let (left, right) = buffers.split_at_mut(b);
    (&mut left[a], &mut right[0])
}

/// Every position's experts, the positions split across the CPU cores.
fn route_all(
    model: &Model,
    layer: &crate::model::Layer,
    x: &[f32],
    n: usize,
    hidden: usize,
) -> Vec<Vec<(usize, f32)>> {
    if n == 1 {
        return vec![model.route(layer, &x[..hidden])];
    }
    let threads = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(n);
    let per = n.div_ceil(threads);
    std::thread::scope(|scope| {
        let parts: Vec<_> = (0..n)
            .step_by(per)
            .map(|p0| {
                scope.spawn(move || {
                    (p0..(p0 + per).min(n))
                        .map(|p| model.route(layer, &x[p * hidden..][..hidden]))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        parts
            .into_iter()
            .flat_map(|h| h.join().expect("router thread"))
            .collect()
    })
}
