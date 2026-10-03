//! gpt-oss's forward pass on Apple GPUs, through `loadngo-metal-compute`.
//!
//! The weights are copied once into GPU memory in their file formats: `Q8_0` repacked
//! into signed-byte rows and binary16 scales, and MXFP4 as `Mxfp4Matrix` lays it out.
//! Each layer is two GPU submissions, each finishing as a completion on a loadngo
//! proactor:
//!
//! 1. Attention: norm; q, k and v with biases; rotary; the new keys and values into the
//!    layer's cache; grouped attention with sinks; the output projection with its
//!    bias, added to the residual; the norm before the experts.
//! 2. Experts: the CPU routes each position from the normalized rows, which it reads
//!    straight from shared memory, and gathers them by expert. The GPU computes every
//!    chosen expert. The CPU adds the weighted results to the residual.
//!
//! Sliding layers keep their keys and values in a ring of `sliding_window + chunk`
//! rows (rounded up to 32). Full layers keep `max_context` rows. Positions are fed in
//! chunks of at most `chunk`. From 32 positions the products and attention run on the
//! matrix units.
//!
//! [`crate::model::Model`] stays the reference: `tests/gpu_oracle.rs` holds this path to
//! it and to transformers.

use std::sync::{Arc, Mutex};

use loadngo_metal_compute::{
    Batch, Buffer, Completed, Dispatch, Gpu, GroupedShape, Resident, Slice,
};
use loadngo_proactor::{new_platform_proactor, PlatformPort, Proactor};
use loadngo_weights::q8_0::split_blocks;

use crate::model::{Format, Matrix, Model};

#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    #[error(transparent)]
    Metal(#[from] loadngo_metal_compute::Error),
    #[error("proactor: {0}")]
    Proactor(#[from] std::io::Error),
    #[error("{0}")]
    Invalid(String),
}

/// How a matrix lives on the GPU.
enum Weights {
    Q8 {
        codes: Arc<Resident>,
        scales: Arc<Resident>,
    },
    Mxfp4 {
        elements: Arc<Resident>,
        scales: Arc<Resident>,
    },
}

struct GpuMatrix {
    rows: usize,
    cols: usize,
    weights: Weights,
}

impl GpuMatrix {
    fn new(gpu: &Gpu, m: &Matrix) -> Result<Self, GpuError> {
        let weights = match &m.format {
            Format::Q8(blocks) => {
                let (codes, scales) = split_blocks(blocks);
                Weights::Q8 {
                    codes: gpu.resident(&codes)?,
                    scales: gpu.resident(&scales)?,
                }
            }
            Format::Mxfp4 { elements, scales } => Weights::Mxfp4 {
                elements: gpu.resident(elements)?,
                scales: gpu.resident(scales)?,
            },
            Format::F32(_) => {
                return Err(GpuError::Invalid("f32 matrices stay on the CPU".into()));
            }
        };
        Ok(Self {
            rows: m.rows,
            cols: m.cols,
            weights,
        })
    }

    /// `y[p] = W x[p]` for `n` positions (rows of `cols` and `rows` floats). From 32
    /// positions the tiled kernel runs on `n` rounded up to 32, so `x` and `y` must
    /// hold that many rows.
    fn mul(&self, batch: &mut Batch<'_>, x: Slice, y: Slice, n: usize) -> Result<(), GpuError> {
        let (rows, cols) = (self.rows, self.cols);
        let strides = (cols, rows);
        let tiled = n >= 32;
        let n_run = if tiled { n.next_multiple_of(32) } else { n };
        let x = Slice::new(x.buffer, x.offset, n_run * cols * 4);
        let y = Slice::new(y.buffer, y.offset, n_run * rows * 4);
        match &self.weights {
            Weights::Q8 { codes, scales } => {
                let m = (batch.attach(codes), batch.attach(scales));
                if n == 1 {
                    batch.gemv_q8_0(m, x, y, rows, cols)?;
                } else if tiled {
                    batch.gemm_q8_0_tiled(m, x, y, rows, cols, n_run, strides)?;
                } else {
                    batch.gemm_q8_0(m, x, y, rows, cols, n, strides)?;
                }
            }
            Weights::Mxfp4 { elements, scales } => {
                let (e, s) = (batch.attach(elements), batch.attach(scales));
                if n == 1 {
                    batch.gemv_mxfp4(e, s, x, y, rows, cols)?;
                } else if tiled {
                    batch.gemm_mxfp4_tiled(e, s, x, y, rows, cols, n_run, strides)?;
                } else {
                    batch.gemm_mxfp4(e, s, x, y, rows, cols, n, strides)?;
                }
            }
        }
        Ok(())
    }
}

struct GpuExpert {
    gate: GpuMatrix,
    up: GpuMatrix,
    down: GpuMatrix,
    gate_bias: Arc<Resident>,
    up_bias: Arc<Resident>,
    down_bias: Arc<Resident>,
}

struct GpuLayer {
    attn_norm: Arc<Resident>,
    q: GpuMatrix,
    k: GpuMatrix,
    v: GpuMatrix,
    o: GpuMatrix,
    q_bias: Arc<Resident>,
    k_bias: Arc<Resident>,
    v_bias: Arc<Resident>,
    o_bias: Arc<Resident>,
    sinks: Arc<Resident>,
    ffn_norm: Arc<Resident>,
    experts: Vec<GpuExpert>,
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
const CACHES: usize = 14;

pub struct GpuModel {
    /// The CPU side: shape, router, embedding and rotary frequencies. Its other
    /// matrices are released once they are in GPU memory.
    model: Model,
    gpu: Gpu,
    proactor: Proactor<PlatformPort>,
    layers: Vec<GpuLayer>,
    output: GpuMatrix,
    output_norm: Arc<Resident>,
    /// Positions per GPU pass.
    pub chunk: usize,
    /// Rows a full layer's cache holds.
    pub max_context: usize,
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

/// Which logits a pass returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Logits {
    Last,
    All,
}

fn f(buffer: usize, row: usize, width: usize, rows: usize) -> Slice {
    Slice::new(buffer, row * width * 4, rows * width * 4)
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
            let experts = layer
                .experts
                .iter()
                .map(|e| {
                    Ok(GpuExpert {
                        gate: GpuMatrix::new(&gpu, &e.gate)?,
                        up: GpuMatrix::new(&gpu, &e.up)?,
                        down: GpuMatrix::new(&gpu, &e.down)?,
                        gate_bias: floats(&gpu, &e.gate_bias)?,
                        up_bias: floats(&gpu, &e.up_bias)?,
                        down_bias: floats(&gpu, &e.down_bias)?,
                    })
                })
                .collect::<Result<Vec<_>, GpuError>>()?;
            layers.push(GpuLayer {
                attn_norm: floats(&gpu, &layer.attn_norm)?,
                q: GpuMatrix::new(&gpu, &layer.q)?,
                k: GpuMatrix::new(&gpu, &layer.k)?,
                v: GpuMatrix::new(&gpu, &layer.v)?,
                o: GpuMatrix::new(&gpu, &layer.o)?,
                q_bias: floats(&gpu, &layer.q_bias)?,
                k_bias: floats(&gpu, &layer.k_bias)?,
                v_bias: floats(&gpu, &layer.v_bias)?,
                o_bias: floats(&gpu, &layer.o_bias)?,
                sinks: floats(&gpu, &layer.sinks)?,
                ffn_norm: floats(&gpu, &layer.ffn_norm)?,
                experts,
            });
        }
        let output = GpuMatrix::new(&gpu, &model.output)?;
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

    /// Commits `batch` and waits on the proactor until the GPU has finished.
    fn run(&self, batch: Batch<'_>) -> Result<Vec<Buffer>, GpuError> {
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
                done.gpu_time?;
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

    #[allow(clippy::too_many_lines)]
    fn pass(
        &self,
        session: &mut GpuSession,
        tokens: &[u32],
        which: Option<Logits>,
    ) -> Result<Vec<Vec<f32>>, GpuError> {
        let c = &self.model.config;
        let n = tokens.len();
        let start = session.len;
        let (hidden, q_width, kv_width) = (c.hidden, c.heads * c.head_dim, c.kv_heads * c.head_dim);
        let mut buffers = std::mem::take(&mut session.buffers);

        // The embedding rows, and the rotary table for these positions.
        for (p, &token) in tokens.iter().enumerate() {
            self.model
                .embed(token, &mut buffers[H].as_f32_mut()[p * hidden..][..hidden]);
        }
        {
            let half = c.head_dim / 2;
            let table = buffers[TABLE].as_f32_mut();
            for p in 0..n {
                for (i, &freq) in self.model.inv_freq.iter().enumerate() {
                    let (sin, cos) = ((start + p) as f64 * freq).sin_cos();
                    table[(p * half + i) * 2] = (cos * self.model.rope_scale) as f32;
                    table[(p * half + i) * 2 + 1] = (sin * self.model.rope_scale) as f32;
                }
            }
        }

        for (l, layer) in self.layers.iter().enumerate() {
            let slots = session.slots[l];
            let (kc, vc) = (CACHES + 2 * l, CACHES + 2 * l + 1);
            let mut batch = self.gpu.batch(buffers, Dispatch::Serial)?;
            batch.copy_rows(
                (f(H, 0, hidden, n), hidden),
                (f(X, 0, hidden, n), hidden),
                n,
                hidden,
            )?;
            let norm = batch.attach(&layer.attn_norm);
            batch.rmsnorm_rows((f(X, 0, hidden, n), norm), n, (hidden, hidden), c.rms_eps)?;
            layer
                .q
                .mul(&mut batch, f(X, 0, hidden, n), f(Q, 0, q_width, n), n)?;
            layer
                .k
                .mul(&mut batch, f(X, 0, hidden, n), f(K, 0, kv_width, n), n)?;
            layer
                .v
                .mul(&mut batch, f(X, 0, hidden, n), f(V, 0, kv_width, n), n)?;
            for (buffer, bias, width) in [
                (Q, &layer.q_bias, q_width),
                (K, &layer.k_bias, kv_width),
                (V, &layer.v_bias, kv_width),
            ] {
                let bias = batch.attach(bias);
                batch.add_rows(f(buffer, 0, width, n), (bias, 0), n, width)?;
            }
            let table = f(TABLE, 0, c.head_dim, n);
            batch.rotate_halves(f(Q, 0, q_width, n), table, n, (c.heads, c.head_dim))?;
            batch.rotate_halves(f(K, 0, kv_width, n), table, n, (c.kv_heads, c.head_dim))?;
            // The new rows into the ring: positions start.. go to rows (start + i) % slots,
            // in at most two runs.
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
                        f(Q, 0, q_width, n),
                        f(kc, 0, kv_width, rows),
                        f(vc, 0, kv_width, rows),
                    ),
                    sinks,
                    f(ATT, 0, q_width, n),
                    shape,
                )?;
            }
            layer
                .o
                .mul(&mut batch, f(ATT, 0, q_width, n), f(TMP, 0, hidden, n), n)?;
            let o_bias = batch.attach(&layer.o_bias);
            batch.add_rows(f(TMP, 0, hidden, n), (o_bias, 0), n, hidden)?;
            batch.add_rows(
                f(H, 0, hidden, n),
                (f(TMP, 0, hidden, n), hidden),
                n,
                hidden,
            )?;
            batch.copy_rows(
                (f(H, 0, hidden, n), hidden),
                (f(X, 0, hidden, n), hidden),
                n,
                hidden,
            )?;
            let norm = batch.attach(&layer.ffn_norm);
            batch.rmsnorm_rows((f(X, 0, hidden, n), norm), n, (hidden, hidden), c.rms_eps)?;
            buffers = self.run(batch)?;

            // Route on the CPU from the normalized rows; gather them by expert.
            let cpu_layer = &self.model.layers[l];
            let routes: Vec<Vec<(usize, f32)>> = {
                let x = buffers[X].as_f32();
                route_all(&self.model, cpu_layer, x, n, hidden)
            };
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
                if rows.is_empty() {
                    continue;
                }
                let m = rows.len();
                plan.push((e, base, m));
                base += if m >= 32 { m.next_multiple_of(32) } else { m };
            }
            {
                let (x, xe) = two(&mut buffers, X, XE);
                let (x, xe) = (x.as_f32(), xe.as_f32_mut());
                for &(e, at, _) in &plan {
                    for (i, &(p, _)) in by_expert[e].iter().enumerate() {
                        xe[(at + i) * hidden..][..hidden]
                            .copy_from_slice(&x[p * hidden..][..hidden]);
                    }
                }
            }
            let eh = c.expert_hidden;
            let mut batch = self.gpu.batch(buffers, Dispatch::Serial)?;
            for &(e, at, m) in &plan {
                let expert = &layer.experts[e];
                let run = if m >= 32 { m.next_multiple_of(32) } else { m };
                expert
                    .gate
                    .mul(&mut batch, f(XE, at, hidden, run), f(GATE, at, eh, run), m)?;
                expert
                    .up
                    .mul(&mut batch, f(XE, at, hidden, run), f(UP, at, eh, run), m)?;
                let (gb, ub) = (
                    batch.attach(&expert.gate_bias),
                    batch.attach(&expert.up_bias),
                );
                batch.clamped_swiglu(
                    (f(GATE, at, eh, m), f(UP, at, eh, m)),
                    (gb, ub),
                    f(HIDDEN, at, eh, m),
                    m,
                    eh,
                    (7.0, 1.702),
                )?;
                expert.down.mul(
                    &mut batch,
                    f(HIDDEN, at, eh, run),
                    f(YE, at, hidden, run),
                    m,
                )?;
                let db = batch.attach(&expert.down_bias);
                batch.add_rows(f(YE, at, hidden, m), (db, 0), m, hidden)?;
            }
            buffers = self.run(batch)?;
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
        }

        // Final norm and logits, for the last position or all of them, at most 32 rows
        // per submission.
        let wanted = match which {
            None => 0..0,
            Some(Logits::Last) => n - 1..n,
            Some(Logits::All) => 0..n,
        };
        let vocab = c.vocab;
        let mut out = Vec::with_capacity(wanted.len());
        for first in wanted.clone().step_by(32) {
            let rows = (wanted.end - first).min(32);
            let mut batch = self.gpu.batch(buffers, Dispatch::Serial)?;
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
                &mut batch,
                f(X, 0, hidden, rows),
                f(LOGITS, 0, vocab, rows),
                rows,
            )?;
            buffers = self.run(batch)?;
            let logits = buffers[LOGITS].as_f32();
            out.extend((0..rows).map(|r| logits[r * vocab..][..vocab].to_vec()));
        }
        session.buffers = buffers;
        session.len += n;
        Ok(out)
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
    let threads = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(n.max(1));
    let per = n.div_ceil(threads);
    std::thread::scope(|scope| {
        let parts: Vec<_> = (0..n)
            .step_by(per.max(1))
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
