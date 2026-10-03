//! Metal device, buffers and batches. The only unsafe code in the crate.

use std::ops::Range;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use block2::RcBlock;
use loadngo_proactor::{CompletionPort, ProactorHandle};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBarrierScope, MTLBuffer, MTLCommandBuffer, MTLCommandBufferStatus, MTLCommandEncoder,
    MTLCommandQueue, MTLCompileOptions, MTLComputeCommandEncoder, MTLComputePipelineState,
    MTLCreateSystemDefaultDevice, MTLDevice, MTLDispatchType, MTLLibrary, MTLMathMode,
    MTLResourceOptions, MTLSize,
};

use crate::Error;

const SOURCE: &str = include_str!("kernels.metal");

/// Threads per threadgroup: eight simdgroups of 32.
const THREADS_PER_GROUP: usize = 256;
const SIMDGROUPS_PER_GROUP: usize = THREADS_PER_GROUP / 32;

type Pipeline = Retained<ProtocolObject<dyn MTLComputePipelineState>>;

/// Rows each simdgroup computes. More rows reuse each loaded slice of `x` more often;
/// fewer rows spread a small matrix over more of the GPU.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Rows {
    One,
    Two,
    #[default]
    Four,
}

impl Rows {
    pub const ALL: [Rows; 3] = [Rows::One, Rows::Two, Rows::Four];

    pub const fn count(self) -> usize {
        match self {
            Rows::One => 1,
            Rows::Two => 2,
            Rows::Four => 4,
        }
    }

    const fn index(self) -> usize {
        match self {
            Rows::One => 0,
            Rows::Two => 1,
            Rows::Four => 2,
        }
    }
}

/// Whether a batch's dispatches run in order or may overlap.
///
/// `Concurrent` lets independent products (for example the experts of one layer) share
/// the GPU; a later dispatch that reads an earlier one's output needs [`Batch::barrier`]
/// between them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dispatch {
    Serial,
    Concurrent,
}

/// The system's Metal GPU, its command queue and the compiled kernels.
pub struct Gpu {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    /// The arena residents are carved from now, and how much of it is used.
    arena: Mutex<Option<(Arc<Arena>, usize)>>,
    /// Lock new arenas in physical memory; bytes locked; arenas that could not be.
    wire: bool,
    wired: AtomicUsize,
    wire_failures: AtomicUsize,
    bf16: [Pipeline; 3],
    mxfp4: [Pipeline; 3],
    q8_0: [Pipeline; 3],
    gemm_q8_0: Pipeline,
    gemm_q8_0_tiled: Pipeline,
    moe: Moe,
    gemm_bf16: Pipeline,
    gemm_mxfp4: Pipeline,
    gemm_mxfp4_tiled: Pipeline,
    gemm_bf16_tiled: Pipeline,
    attention: Pipeline,
    attention_wide: Pipeline,
    attention_grouped: Pipeline,
    attention_grouped_narrow: Pipeline,
    attention_grouped_tiled: Pipeline,
    attention_tiled: Pipeline,
    recurrence: Pipeline,
    glue: Glue,
    rows: Rows,
}

impl Gpu {
    /// Opens the default Metal device and compiles the kernels.
    pub fn new() -> Result<Self, Error> {
        let device = MTLCreateSystemDefaultDevice().ok_or(Error::NoDevice)?;
        let queue = device.newCommandQueue().ok_or(Error::NoDevice)?;
        let options = MTLCompileOptions::new();
        // Fast math may assume no NaN or infinity; the E8M0 NaN scale must survive.
        options.setMathMode(MTLMathMode::Safe);
        let library = device
            .newLibraryWithSource_options_error(&NSString::from_str(SOURCE), Some(&options))
            .map_err(|e| Error::Compile(e.localizedDescription().to_string()))?;
        let pipeline = |name: &str| -> Result<Pipeline, Error> {
            let function = library
                .newFunctionWithName(&NSString::from_str(name))
                .ok_or_else(|| Error::Compile(format!("kernel {name} missing")))?;
            device
                .newComputePipelineStateWithFunction_error(&function)
                .map_err(|e| Error::Compile(format!("{name}: {}", e.localizedDescription())))
        };
        let bf16 = [
            pipeline("gemv_bf16_r1")?,
            pipeline("gemv_bf16_r2")?,
            pipeline("gemv_bf16_r4")?,
        ];
        let mxfp4 = [
            pipeline("gemv_mxfp4_r1")?,
            pipeline("gemv_mxfp4_r2")?,
            pipeline("gemv_mxfp4_r4")?,
        ];
        let q8_0 = [
            pipeline("gemv_q8_0_r1")?,
            pipeline("gemv_q8_0_r2")?,
            pipeline("gemv_q8_0_r4")?,
        ];
        let moe = Moe {
            gemv_f32: pipeline("gemv_f32")?,
            topk: pipeline("topk_softmax")?,
            selected: [
                pipeline("gemv_mxfp4_selected_r1")?,
                pipeline("gemv_mxfp4_selected_r2")?,
                pipeline("gemv_mxfp4_selected_r4")?,
            ],
            swiglu: pipeline("clamped_swiglu_selected")?,
            combine: pipeline("moe_combine")?,
        };
        let gemm_q8_0 = pipeline("gemm_q8_0")?;
        let gemm_q8_0_tiled = pipeline("gemm_q8_0_tiled")?;
        let gemm_bf16 = pipeline("gemm_bf16")?;
        let gemm_mxfp4 = pipeline("gemm_mxfp4")?;
        let gemm_mxfp4_tiled = pipeline("gemm_mxfp4_tiled")?;
        let gemm_bf16_tiled = pipeline("gemm_bf16_tiled")?;
        let attention = pipeline("attention_split_key")?;
        let attention_wide = pipeline("attention_split_key_wide")?;
        let attention_grouped = pipeline("attention_grouped")?;
        let attention_grouped_narrow = pipeline("attention_grouped_narrow")?;
        let attention_grouped_tiled = pipeline("attention_grouped_tiled")?;
        let attention_tiled = pipeline("attention_split_key_tiled")?;
        let recurrence = pipeline("delta_rule_recurrence")?;
        let glue = Glue {
            conv: pipeline("causal_conv_silu")?,
            conv_history: pipeline("causal_conv_history")?,
            l2norm: pipeline("l2norm_rows")?,
            rmsnorm_gated: pipeline("rmsnorm_gated_rows")?,
            decay: pipeline("softplus_decay")?,
            sigmoid: pipeline("sigmoid_in_place")?,
            silu_mul: pipeline("silu_mul")?,
            rmsnorm: pipeline("rmsnorm_rows")?,
            copy_rows: pipeline("copy_rows")?,
            add_rows: pipeline("add_rows")?,
            rotate_halves: pipeline("rotate_halves")?,
            clamped_swiglu: pipeline("clamped_swiglu")?,
        };
        for p in bf16
            .iter()
            .chain(&mxfp4)
            .chain(&q8_0)
            .chain([
                &gemm_q8_0,
                &gemm_q8_0_tiled,
                &gemm_bf16,
                &gemm_mxfp4,
                &gemm_mxfp4_tiled,
                &gemm_bf16_tiled,
                &attention,
                &attention_wide,
                &attention_grouped,
                &attention_grouped_narrow,
                &attention_grouped_tiled,
                &attention_tiled,
                &recurrence,
            ])
            .chain(glue.all())
            .chain(moe.all())
        {
            if p.maxTotalThreadsPerThreadgroup() < THREADS_PER_GROUP {
                return Err(Error::Compile(format!(
                    "kernels need {THREADS_PER_GROUP} threads per threadgroup, the device allows {}",
                    p.maxTotalThreadsPerThreadgroup()
                )));
            }
        }
        Ok(Self {
            device,
            queue,
            arena: Mutex::new(None),
            wire: false,
            wired: AtomicUsize::new(0),
            wire_failures: AtomicUsize::new(0),
            bf16,
            mxfp4,
            q8_0,
            gemm_q8_0,
            gemm_q8_0_tiled,
            moe,
            gemm_bf16,
            gemm_mxfp4,
            gemm_mxfp4_tiled,
            gemm_bf16_tiled,
            attention,
            attention_wide,
            attention_grouped,
            attention_grouped_narrow,
            attention_grouped_tiled,
            attention_tiled,
            recurrence,
            glue,
            rows: Rows::default(),
        })
    }

    /// The device's name, for example "Apple M4 Pro".
    pub fn name(&self) -> String {
        self.device.name().to_string()
    }

    /// How much memory Metal recommends a process keep resident on this device.
    pub fn recommended_working_set(&self) -> u64 {
        self.device.recommendedMaxWorkingSetSize()
    }

    pub fn rows(&self) -> Rows {
        self.rows
    }

    /// Rows per simdgroup for batches created from now on.
    pub fn set_rows(&mut self, rows: Rows) {
        self.rows = rows;
    }

    /// A `len`-byte buffer in memory shared by the CPU and GPU. Its contents are
    /// unspecified until written.
    pub fn buffer(&self, len: usize) -> Result<Buffer, Error> {
        if len == 0 {
            return Err(Error::Alloc(0));
        }
        let raw = self
            .device
            .newBufferWithLength_options(
                len,
                // Untracked: ordering comes from ownership (a batch owns its buffers until
                // the GPU is done; residents are never written) and, within a concurrent
                // batch, from `Batch::barrier`. Metal's own tracking cost ~20 ms per
                // submission of a few hundred dispatches.
                MTLResourceOptions::StorageModeShared
                    | MTLResourceOptions::HazardTrackingModeUntracked,
            )
            .ok_or(Error::Alloc(len))?;
        Ok(Buffer { raw, len })
    }

    /// Read-only memory shared by the CPU and GPU, filled once from `bytes`: model weights
    /// that many batches read at the same time.
    /// Locks the memory of residents made from now on in physical memory (`mlock`), so
    /// the system never compresses or swaps it out. Weights are read by the GPU at
    /// unpredictable times (a mixture-of-experts model touches a few experts per token),
    /// and under memory pressure the cold ones were compressed and had to be
    /// decompressed in the middle of a token, for seconds. Arenas that cannot be locked
    /// (the system's wire limit) still work, unlocked, and are counted in
    /// [`Gpu::wire_failures`]. Unlocked when the arena is freed.
    pub fn set_wire_residents(&mut self, wire: bool) {
        self.wire = wire;
    }

    /// Bytes of resident memory locked by [`Gpu::set_wire_residents`] so far.
    pub fn wired_bytes(&self) -> usize {
        self.wired.load(Ordering::Relaxed)
    }

    /// Arenas that could not be locked.
    pub fn wire_failures(&self) -> usize {
        self.wire_failures.load(Ordering::Relaxed)
    }

    pub fn resident(&self, bytes: &[u8]) -> Result<Arc<Resident>, Error> {
        self.carve(bytes.len(), |dst| dst.copy_from_slice(bytes))
    }

    /// As [`Gpu::resident`] for 16-bit words (for example bfloat16), stored little-endian.
    pub fn resident_words(&self, words: &[u16]) -> Result<Arc<Resident>, Error> {
        self.carve(words.len() * 2, |dst| {
            for (pair, word) in dst.as_chunks_mut::<2>().0.iter_mut().zip(words) {
                pair.copy_from_slice(&word.to_le_bytes());
            }
        })
    }

    /// `len` bytes from the current arena (a new one when it is full), filled by `fill`.
    /// Residents share arenas so a batch reading thousands of weights names a few dozen
    /// Metal buffers: every buffer a command buffer names costs time at each submission
    /// (measured: ~17 ms per step with one buffer per weight). An arena is freed when
    /// its last resident is.
    fn carve(&self, len: usize, fill: impl FnOnce(&mut [u8])) -> Result<Arc<Resident>, Error> {
        if len == 0 {
            return Err(Error::Alloc(0));
        }
        let mut current = self.arena.lock().unwrap_or_else(PoisonError::into_inner);
        let fits = |(arena, used): &(Arc<Arena>, usize)| used + len <= arena.len;
        let (arena, offset) = match current.as_mut().filter(|c| fits(c)) {
            Some((arena, used)) => {
                let offset = *used;
                *used = (offset + len).div_ceil(ARENA_ALIGN) * ARENA_ALIGN;
                (Arc::clone(arena), offset)
            }
            None => {
                let buffer = self.buffer(len.max(ARENA_BYTES))?;
                // SAFETY: the buffer's shared-storage contents are mapped for `len` bytes
                // for as long as the buffer lives; the arena unlocks them before release.
                let wired = self.wire
                    && unsafe { libc::mlock(buffer.raw.contents().as_ptr().cast(), buffer.len) }
                        == 0;
                if wired {
                    self.wired.fetch_add(buffer.len, Ordering::Relaxed);
                } else if self.wire {
                    self.wire_failures.fetch_add(1, Ordering::Relaxed);
                }
                let arena = Arc::new(Arena {
                    raw: buffer.raw,
                    len: buffer.len,
                    wired,
                });
                if len < ARENA_BYTES {
                    *current = Some((Arc::clone(&arena), len.div_ceil(ARENA_ALIGN) * ARENA_ALIGN));
                }
                (arena, 0)
            }
        };
        drop(current);
        // SAFETY: `offset..offset + len` was just carved and is handed out once: no other
        // resident, and so no GPU work, refers to it yet.
        let dst = unsafe {
            std::slice::from_raw_parts_mut(
                arena.raw.contents().as_ptr().cast::<u8>().add(offset),
                len,
            )
        };
        fill(dst);
        Ok(Arc::new(Resident { arena, offset, len }))
    }

    /// Starts recording a batch that owns `buffers` until it completes. Dispatches refer
    /// to them by position in this vector.
    pub fn batch(&self, buffers: Vec<Buffer>, dispatch: Dispatch) -> Result<Batch<'_>, Error> {
        let command = self
            .queue
            .commandBuffer()
            .ok_or_else(|| Error::Gpu("no command buffer".into()))?;
        let encoder = command
            .computeCommandEncoderWithDispatchType(match dispatch {
                Dispatch::Serial => MTLDispatchType::Serial,
                Dispatch::Concurrent => MTLDispatchType::Concurrent,
            })
            .ok_or_else(|| Error::Gpu("no compute encoder".into()))?;
        Ok(Batch {
            gpu: self,
            buffers,
            command,
            encoder,
            arenas: Vec::new(),
            dispatches: 0,
            ended: false,
        })
    }
}

/// Memory the CPU and GPU share. The GPU only touches a buffer while a [`Batch`] owns
/// it, and a batch hands its buffers back only after the GPU has finished, so a
/// `Buffer` in hand is never being written by the GPU.
pub struct Buffer {
    raw: Retained<ProtocolObject<dyn MTLBuffer>>,
    len: usize,
}

// SAFETY: Metal buffers are thread-safe objects; access to their contents is serialized by
// ownership (see the type's documentation).
unsafe impl Send for Buffer {}

impl Buffer {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_bytes(&self) -> &[u8] {
        // SAFETY: `contents` is valid for `len` bytes for the buffer's lifetime, and no GPU
        // work references this buffer while the CPU holds it.
        unsafe { std::slice::from_raw_parts(self.raw.contents().as_ptr().cast(), self.len) }
    }

    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as for `as_bytes`, and `&mut self` makes this the only view.
        unsafe { std::slice::from_raw_parts_mut(self.raw.contents().as_ptr().cast(), self.len) }
    }

    /// The buffer as `f32`s (contents are page-aligned; a trailing partial float is left out).
    pub fn as_f32(&self) -> &[f32] {
        // SAFETY: page alignment satisfies `f32`; every bit pattern is a valid `f32`.
        unsafe { std::slice::from_raw_parts(self.raw.contents().as_ptr().cast(), self.len / 4) }
    }

    pub fn as_f32_mut(&mut self) -> &mut [f32] {
        // SAFETY: as for `as_f32`, and `&mut self` makes this the only view.
        unsafe { std::slice::from_raw_parts_mut(self.raw.contents().as_ptr().cast(), self.len / 4) }
    }
}

/// Bytes per arena that residents are carved from; a larger resident gets its own.
const ARENA_BYTES: usize = 1 << 30;
/// Start alignment of each resident within its arena.
const ARENA_ALIGN: usize = 256;

/// One Metal buffer holding many residents.
struct Arena {
    raw: Retained<ProtocolObject<dyn MTLBuffer>>,
    len: usize,
    /// Locked in physical memory with `mlock` (see [`Gpu::set_wire_residents`]).
    wired: bool,
}

impl Drop for Arena {
    fn drop(&mut self) {
        if self.wired {
            // SAFETY: the range was locked when the arena was made and is still mapped:
            // the Metal buffer is released only after this.
            unsafe { libc::munlock(self.raw.contents().as_ptr().cast(), self.len) };
        }
    }
}

// SAFETY: each byte range of an arena is written once, when its resident is carved and
// before anything else can refer to it, and only read afterwards; Metal buffers are
// thread-safe objects.
unsafe impl Send for Arena {}
// SAFETY: as above.
unsafe impl Sync for Arena {}

/// Memory the CPU and GPU share that neither writes after it is filled, so any number of
/// batches may read it while the CPU does too. Attach it to a batch with
/// [`Batch::attach`].
pub struct Resident {
    arena: Arc<Arena>,
    offset: usize,
    len: usize,
}

impl Resident {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn start(&self) -> *const u8 {
        // SAFETY: `offset` lies within the arena.
        unsafe {
            self.arena
                .raw
                .contents()
                .as_ptr()
                .cast::<u8>()
                .add(self.offset)
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        // SAFETY: the arena holds `len` bytes from `start` while `self` lives (the `Arc`);
        // nothing writes them after carving.
        unsafe { std::slice::from_raw_parts(self.start(), self.len) }
    }

    /// The contents as 16-bit words (256-byte aligned; a trailing odd byte is left out).
    pub fn as_words(&self) -> &[u16] {
        // SAFETY: as for `as_bytes`; the alignment satisfies `u16`.
        unsafe { std::slice::from_raw_parts(self.start().cast(), self.len / 2) }
    }
}

/// Bytes `offset..offset + len` of the batch's buffer number `buffer`: its own buffers
/// first, then the residents it attached, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slice {
    pub buffer: usize,
    pub offset: usize,
    pub len: usize,
}

impl Slice {
    pub const fn new(buffer: usize, offset: usize, len: usize) -> Self {
        Self {
            buffer,
            offset,
            len,
        }
    }

    fn range(&self) -> Range<usize> {
        self.offset..self.offset + self.len
    }

    fn overlaps(&self, other: &Slice) -> bool {
        self.buffer == other.buffer
            && self.offset < other.offset + other.len
            && other.offset < self.offset + self.len
    }
}

/// What a finished batch hands back on the proactor: its buffers, and how long the GPU
/// spent on it or why it failed.
pub struct Completed {
    pub buffers: Vec<Buffer>,
    pub gpu_time: Result<Duration, Error>,
}

/// One command buffer being recorded.
pub struct Batch<'g> {
    gpu: &'g Gpu,
    buffers: Vec<Buffer>,
    /// Arenas of attached residents, each named once.
    arenas: Vec<Arc<Arena>>,
    command: Retained<ProtocolObject<dyn MTLCommandBuffer>>,
    encoder: Retained<ProtocolObject<dyn MTLComputeCommandEncoder>>,
    dispatches: usize,
    ended: bool,
}

impl Drop for Batch<'_> {
    /// A batch dropped without [`Batch::commit`] (for example after a dispatch error)
    /// still ends its encoder, as Metal requires; its command buffer is never committed.
    fn drop(&mut self) {
        if !self.ended {
            self.encoder.endEncoding();
        }
    }
}

#[repr(C)]
struct GemvArgs {
    rows: u32,
    cols: u32,
}

#[repr(C)]
struct GemmArgs {
    rows: u32,
    cols: u32,
    n: u32,
    x_stride: u32,
    y_stride: u32,
}

#[repr(C)]
struct AttentionArgs {
    t: u32,
    cached: u32,
    heads: u32,
    qa: u32,
    qb: u32,
    dv: u32,
    scale: f32,
}

/// New positions per threadgroup in `attention_split_key` (`ATTENTION_BLOCK` in the MSL).
const ATTENTION_BLOCK: usize = 8;

/// Below this many new positions, attention splits each position's cache walk across a
/// threadgroup's simdgroups (`attention_split_key_wide`) instead of giving each position
/// one simdgroup: a single decoding position would otherwise walk the cache alone.
const ATTENTION_WIDE_BELOW: usize = ATTENTION_BLOCK;

/// The shape of one [`Batch::attention_grouped`] dispatch.
#[derive(Clone, Copy, Debug)]
pub struct GroupedShape {
    /// New positions (queries).
    pub t: usize,
    /// The absolute position of the first of them.
    pub start: usize,
    pub heads: usize,
    /// Key/value heads; `heads` is a multiple of it.
    pub kv_heads: usize,
    /// Width of every query, key and value head: a multiple of 32, at most 512.
    pub dim: usize,
    /// Positions each query sees, its own included; `usize::MAX` for all of them.
    pub window: usize,
    /// Rows in the key and value buffers; position `s` is in row `s % slots`.
    pub slots: usize,
    pub scale: f32,
}

#[repr(C)]
struct GroupedArgs {
    t: u32,
    start: u32,
    heads: u32,
    kv_heads: u32,
    dim: u32,
    window: u32,
    slots: u32,
    scale: f32,
    sinks: u32,
}

/// The shape of one [`Batch::attention_split_key`] dispatch.
#[derive(Clone, Copy, Debug)]
pub struct AttentionShape {
    /// New positions (queries).
    pub t: usize,
    /// Positions already in the cache before them.
    pub cached: usize,
    pub heads: usize,
    /// Key dimensions stored per head, beside that head's values.
    pub qa: usize,
    /// Key dimensions shared by every head.
    pub qb: usize,
    /// Value dimensions per head.
    pub dv: usize,
    /// Score scale, usually `1 / sqrt(qa + qb)`.
    pub scale: f32,
}

#[repr(C)]
struct RecurrenceArgs {
    t: u32,
    heads: u32,
    dk: u32,
    dv: u32,
}

/// The shape of one [`Batch::delta_rule_recurrence`] dispatch.
#[derive(Clone, Copy, Debug)]
pub struct RecurrenceShape {
    /// Steps, run in order.
    pub t: usize,
    pub heads: usize,
    /// Key dimensions per head (rows of the state).
    pub dk: usize,
    /// Value dimensions per head (columns of the state).
    pub dv: usize,
}

/// The elementwise and row kernels that join products inside one batch.
struct Glue {
    conv: Pipeline,
    conv_history: Pipeline,
    l2norm: Pipeline,
    rmsnorm_gated: Pipeline,
    decay: Pipeline,
    sigmoid: Pipeline,
    silu_mul: Pipeline,
    rmsnorm: Pipeline,
    copy_rows: Pipeline,
    add_rows: Pipeline,
    rotate_halves: Pipeline,
    clamped_swiglu: Pipeline,
}

impl Glue {
    fn all(&self) -> [&Pipeline; 12] {
        [
            &self.conv,
            &self.conv_history,
            &self.l2norm,
            &self.rmsnorm_gated,
            &self.decay,
            &self.sigmoid,
            &self.silu_mul,
            &self.rmsnorm,
            &self.copy_rows,
            &self.add_rows,
            &self.rotate_halves,
            &self.clamped_swiglu,
        ]
    }
}

#[repr(C)]
struct ConvArgs {
    rows: u32,
    channels: u32,
    width: u32,
}

#[repr(C)]
struct RowArgs {
    rows: u32,
    d: u32,
    eps: f32,
    scale: f32,
}

#[repr(C)]
struct DecayArgs {
    n: u32,
    width: u32,
    d: u32,
}

#[repr(C)]
struct StridedArgs {
    rows: u32,
    d: u32,
    stride: u32,
    eps: f32,
}

/// The kernels that route positions to experts and run the chosen ones on the GPU.
struct Moe {
    gemv_f32: Pipeline,
    topk: Pipeline,
    selected: [Pipeline; 3],
    swiglu: Pipeline,
    combine: Pipeline,
}

impl Moe {
    fn all(&self) -> [&Pipeline; 7] {
        [
            &self.gemv_f32,
            &self.topk,
            &self.selected[0],
            &self.selected[1],
            &self.selected[2],
            &self.swiglu,
            &self.combine,
        ]
    }
}

#[repr(C)]
struct BiasedGemvArgs {
    rows: u32,
    cols: u32,
    bias: u32,
}

#[repr(C)]
struct TopkArgs {
    n: u32,
    experts: u32,
    k: u32,
}

#[repr(C)]
struct SelectedArgs {
    rows: u32,
    cols: u32,
    k: u32,
    x_stride: u32,
    groups_per_matrix: u32,
}

#[repr(C)]
struct CombineArgs {
    width: u32,
    k: u32,
}

#[repr(C)]
struct AddArgs {
    rows: u32,
    width: u32,
    src_stride: u32,
}

#[repr(C)]
struct RotateArgs {
    rows: u32,
    heads: u32,
    dim: u32,
}

#[repr(C)]
struct GluArgs {
    rows: u32,
    width: u32,
    limit: f32,
    alpha: f32,
}

#[repr(C)]
struct CopyArgs {
    rows: u32,
    width: u32,
    src_stride: u32,
    dst_stride: u32,
}

/// Rows per threadgroup in the row kernels: one simdgroup each.
const ROWS_PER_GROUP: usize = SIMDGROUPS_PER_GROUP;

fn narrow(n: usize) -> Result<u32, Error> {
    u32::try_from(n).map_err(|_| Error::Dispatch(format!("{n} does not fit in u32")))
}

/// Weight rows per simdgroup in the multi-position kernels (`GEMM_ROWS` in the MSL).
const GEMM_ROWS: usize = 2;

impl Batch<'_> {
    pub fn dispatches(&self) -> usize {
        self.dispatches
    }

    /// Makes `resident` readable by this batch's dispatches; returns the slice to pass
    /// for it. Residents sharing an arena share one Metal buffer in the batch.
    pub fn attach(&mut self, resident: &Arc<Resident>) -> Slice {
        let index = match self
            .arenas
            .iter()
            .position(|a| Arc::ptr_eq(a, &resident.arena))
        {
            Some(i) => i,
            None => {
                self.arenas.push(Arc::clone(&resident.arena));
                self.arenas.len() - 1
            }
        };
        Slice::new(self.buffers.len() + index, resident.offset, resident.len)
    }

    fn raw(&self, index: usize) -> Option<(&ProtocolObject<dyn MTLBuffer>, usize)> {
        match self.buffers.get(index) {
            Some(b) => Some((&*b.raw, b.len)),
            None => self
                .arenas
                .get(index - self.buffers.len())
                .map(|a| (&*a.raw, a.len)),
        }
    }

    fn check(&self, what: &str, slice: &Slice, len: usize, align: usize) -> Result<(), Error> {
        let Some((_, buffer_len)) = self.raw(slice.buffer) else {
            return Err(Error::Dispatch(format!(
                "{what}: no buffer {} (batch has {})",
                slice.buffer,
                self.buffers.len() + self.arenas.len()
            )));
        };
        if slice.len != len {
            return Err(Error::Dispatch(format!(
                "{what}: {} bytes, the shape needs {len}",
                slice.len
            )));
        }
        if slice.range().end > buffer_len {
            return Err(Error::Dispatch(format!(
                "{what}: bytes {:?} outside a {buffer_len}-byte buffer",
                slice.range()
            )));
        }
        if !slice.offset.is_multiple_of(align) {
            return Err(Error::Dispatch(format!(
                "{what}: offset {} is not a multiple of {align}",
                slice.offset
            )));
        }
        Ok(())
    }

    fn shape(rows: usize, cols: usize) -> Result<GemvArgs, Error> {
        match (u32::try_from(rows), u32::try_from(cols)) {
            (Ok(r), Ok(c)) if r > 0 && c > 0 => Ok(GemvArgs { rows: r, cols: c }),
            _ => Err(Error::Dispatch(format!("unsupported shape {rows}x{cols}"))),
        }
    }

    fn check_output(&self, y: &Slice, inputs: &[&Slice]) -> Result<(), Error> {
        if y.buffer >= self.buffers.len() {
            return Err(Error::Dispatch(
                "output is in read-only resident memory".into(),
            ));
        }
        if inputs.iter().any(|input| y.overlaps(input)) {
            return Err(Error::Dispatch("output overlaps an input".into()));
        }
        Ok(())
    }

    fn encode<A>(&mut self, pipeline: &Pipeline, slices: &[Slice], args: &A, groups: usize) {
        let enc = &self.encoder;
        enc.setComputePipelineState(pipeline);
        for (index, slice) in slices.iter().enumerate() {
            let (raw, _) = self.raw(slice.buffer).expect("slice checked");
            // SAFETY: the slice was checked against its buffer, which this batch owns or
            // holds (and the command buffer retains) until the GPU has finished.
            unsafe { enc.setBuffer_offset_atIndex(Some(raw), slice.offset, index) };
        }
        // SAFETY: `A` is one of the `repr(C)` argument structs above, like the kernel's argument struct.
        unsafe {
            enc.setBytes_length_atIndex(
                NonNull::from(args).cast(),
                std::mem::size_of::<A>(),
                slices.len(),
            );
        }
        enc.dispatchThreadgroups_threadsPerThreadgroup(
            MTLSize {
                width: groups,
                height: 1,
                depth: 1,
            },
            MTLSize {
                width: THREADS_PER_GROUP,
                height: 1,
                depth: 1,
            },
        );
        self.dispatches += 1;
    }

    /// `y = W x` for a `rows x cols` bfloat16 matrix `w` (row-major, little-endian), with
    /// `x` and `y` little-endian `f32`. `w` and `x` must start on 16-byte boundaries.
    pub fn gemv_bf16(
        &mut self,
        w: Slice,
        x: Slice,
        y: Slice,
        rows: usize,
        cols: usize,
    ) -> Result<(), Error> {
        let args = Self::shape(rows, cols)?;
        self.check("weights", &w, rows * cols * 2, 16)?;
        self.check("x", &x, cols * 4, 16)?;
        self.check("y", &y, rows * 4, 4)?;
        self.check_output(&y, &[&w, &x])?;
        let pipeline = self.gpu.bf16[self.gpu.rows.index()].clone();
        let groups = rows.div_ceil(SIMDGROUPS_PER_GROUP * self.gpu.rows.count());
        self.encode(&pipeline, &[w, x, y], &args, groups);
        Ok(())
    }

    /// `y = W x` for a `rows x cols` MXFP4 matrix: `elements` holds `ceil(cols / 2)` bytes
    /// per row (low nibble first), `scales` one E8M0 byte per 32 columns per row.
    /// `elements` and `x` must start on 16-byte boundaries.
    pub fn gemv_mxfp4(
        &mut self,
        elements: Slice,
        scales: Slice,
        x: Slice,
        y: Slice,
        rows: usize,
        cols: usize,
    ) -> Result<(), Error> {
        let args = Self::shape(rows, cols)?;
        self.check("elements", &elements, rows * cols.div_ceil(2), 16)?;
        self.check("scales", &scales, rows * cols.div_ceil(32), 1)?;
        self.check("x", &x, cols * 4, 16)?;
        self.check("y", &y, rows * 4, 4)?;
        self.check_output(&y, &[&elements, &scales, &x])?;
        let pipeline = self.gpu.mxfp4[self.gpu.rows.index()].clone();
        let groups = rows.div_ceil(SIMDGROUPS_PER_GROUP * self.gpu.rows.count());
        self.encode(&pipeline, &[elements, scales, x, y], &args, groups);
        Ok(())
    }

    /// Checks the shape of an `n`-position product: `x` holds `n` rows of `cols` floats
    /// `x_stride` apart (a multiple of 4), `y` `n` rows of `rows` floats `y_stride` apart.
    fn gemm_args(
        rows: usize,
        cols: usize,
        n: usize,
        x_stride: usize,
        y_stride: usize,
    ) -> Result<(GemmArgs, usize, usize), Error> {
        let GemvArgs { rows: r, cols: c } = Self::shape(rows, cols)?;
        if n == 0 || x_stride < cols || y_stride < rows || !x_stride.is_multiple_of(4) {
            return Err(Error::Dispatch(format!(
                "{n} positions of {rows}x{cols} with strides {x_stride}/{y_stride}"
            )));
        }
        let fit = |v: usize| u32::try_from(v).map_err(|_| Error::Dispatch("too large".into()));
        let args = GemmArgs {
            rows: r,
            cols: c,
            n: fit(n)?,
            x_stride: fit(x_stride)?,
            y_stride: fit(y_stride)?,
        };
        Ok((
            args,
            ((n - 1) * x_stride + cols) * 4,
            ((n - 1) * y_stride + rows) * 4,
        ))
    }

    /// `y[p] = W x[p]` for `n` positions with one read of the bfloat16 matrix `w` per
    /// eight positions. `x` holds `n` rows of `cols` floats `x_stride` floats apart (a
    /// multiple of 4), `y` `n` rows of `rows` floats `y_stride` apart.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_bf16(
        &mut self,
        w: Slice,
        x: Slice,
        y: Slice,
        rows: usize,
        cols: usize,
        n: usize,
        (x_stride, y_stride): (usize, usize),
    ) -> Result<(), Error> {
        let (args, x_len, y_len) = Self::gemm_args(rows, cols, n, x_stride, y_stride)?;
        self.check("weights", &w, rows * cols * 2, 16)?;
        self.check("x", &x, x_len, 16)?;
        self.check("y", &y, y_len, 4)?;
        self.check_output(&y, &[&w, &x])?;
        let pipeline = self.gpu.gemm_bf16.clone();
        let groups = rows.div_ceil(SIMDGROUPS_PER_GROUP * GEMM_ROWS);
        self.encode(&pipeline, &[w, x, y], &args, groups);
        Ok(())
    }

    /// As [`Batch::gemm_bf16`] for an MXFP4 matrix (layout as in [`Batch::gemv_mxfp4`]).
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_mxfp4(
        &mut self,
        elements: Slice,
        scales: Slice,
        x: Slice,
        y: Slice,
        rows: usize,
        cols: usize,
        n: usize,
        (x_stride, y_stride): (usize, usize),
    ) -> Result<(), Error> {
        let (args, x_len, y_len) = Self::gemm_args(rows, cols, n, x_stride, y_stride)?;
        self.check("elements", &elements, rows * cols.div_ceil(2), 16)?;
        self.check("scales", &scales, rows * cols.div_ceil(32), 1)?;
        self.check("x", &x, x_len, 16)?;
        self.check("y", &y, y_len, 4)?;
        self.check_output(&y, &[&elements, &scales, &x])?;
        let pipeline = self.gpu.gemm_mxfp4.clone();
        let groups = rows.div_ceil(SIMDGROUPS_PER_GROUP * GEMM_ROWS);
        self.encode(&pipeline, &[elements, scales, x, y], &args, groups);
        Ok(())
    }

    /// As [`Self::gemm_mxfp4`] on the matrix units, tiled 64 weight rows by 32 positions:
    /// each weight is read once per 32 positions. Needs `rows % 64 == 0`,
    /// `cols % 32 == 0` and `n % 32 == 0` (pad the positions).
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_mxfp4_tiled(
        &mut self,
        elements: Slice,
        scales: Slice,
        x: Slice,
        y: Slice,
        rows: usize,
        cols: usize,
        n: usize,
        (x_stride, y_stride): (usize, usize),
    ) -> Result<(), Error> {
        if !rows.is_multiple_of(64) || !cols.is_multiple_of(32) || !n.is_multiple_of(32) {
            return Err(Error::Dispatch(format!(
                "tiled MXFP4 product needs rows % 64, cols % 32 and n % 32 == 0: {rows} x {cols}, n {n}"
            )));
        }
        let (args, x_len, y_len) = Self::gemm_args(rows, cols, n, x_stride, y_stride)?;
        self.check("elements", &elements, rows * cols / 2, 16)?;
        self.check("scales", &scales, rows * cols / 32, 1)?;
        self.check("x", &x, x_len, 16)?;
        self.check("y", &y, y_len, 4)?;
        self.check_output(&y, &[&elements, &scales, &x])?;
        let pipeline = self.gpu.gemm_mxfp4_tiled.clone();
        let groups = rows / 64 * (n / 32);
        narrow(groups)?;
        self.encode(&pipeline, &[elements, scales, x, y], &args, groups);
        Ok(())
    }

    /// As [`Self::gemm_bf16`] on the matrix units, tiled 64 weight rows by 32 positions.
    /// Needs `rows % 64 == 0`, `cols % 32 == 0` and `n % 32 == 0` (pad the positions).
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_bf16_tiled(
        &mut self,
        w: Slice,
        x: Slice,
        y: Slice,
        rows: usize,
        cols: usize,
        n: usize,
        (x_stride, y_stride): (usize, usize),
    ) -> Result<(), Error> {
        if !rows.is_multiple_of(64) || !cols.is_multiple_of(32) || !n.is_multiple_of(32) {
            return Err(Error::Dispatch(format!(
                "tiled bf16 product needs rows % 64, cols % 32 and n % 32 == 0: {rows} x {cols}, n {n}"
            )));
        }
        let (args, x_len, y_len) = Self::gemm_args(rows, cols, n, x_stride, y_stride)?;
        self.check("weights", &w, rows * cols * 2, 16)?;
        self.check("x", &x, x_len, 16)?;
        self.check("y", &y, y_len, 4)?;
        self.check_output(&y, &[&w, &x])?;
        let pipeline = self.gpu.gemm_bf16_tiled.clone();
        let groups = rows / 64 * (n / 32);
        narrow(groups)?;
        self.encode(&pipeline, &[w, x, y], &args, groups);
        Ok(())
    }

    /// Makes every later dispatch see the buffer writes of every earlier one. Needed only
    /// in a [`Dispatch::Concurrent`] batch.
    /// Causal attention with split keys: for each of `shape.t` new positions and each
    /// head, a softmax over every cached position up to and including its own. `q` is
    /// `[t][heads][qa + qb]`, `kv` `[positions][heads][qa + dv]` (per-head keys, then
    /// values), `shared` `[positions][qb]` (the key part every head shares) and `out`
    /// `[t][heads][dv]`, all `f32`, with `positions = cached + t`. Needs `qa + qb <= 256`
    /// and `dv <= 128`. Accumulates in `f32`.
    pub fn attention_split_key(
        &mut self,
        q: Slice,
        kv: Slice,
        shared: Slice,
        out: Slice,
        shape: AttentionShape,
    ) -> Result<(), Error> {
        let AttentionShape {
            t,
            cached,
            heads,
            qa,
            qb,
            dv,
            scale,
        } = shape;
        let positions = cached + t;
        if t == 0 || heads == 0 || qa + qb == 0 || qa + qb > 256 || dv == 0 || dv > 128 {
            return Err(Error::Dispatch(format!(
                "unsupported attention shape {shape:?}"
            )));
        }
        let narrow = |n: usize| {
            u32::try_from(n).map_err(|_| Error::Dispatch(format!("{n} does not fit in u32")))
        };
        let args = AttentionArgs {
            t: narrow(t)?,
            cached: narrow(cached)?,
            heads: narrow(heads)?,
            qa: narrow(qa)?,
            qb: narrow(qb)?,
            dv: narrow(dv)?,
            scale,
        };
        let wide = t < ATTENTION_WIDE_BELOW;
        let groups = if wide {
            t * heads
        } else {
            t.div_ceil(ATTENTION_BLOCK) * heads
        };
        narrow(groups)?;
        self.check("q", &q, t * heads * (qa + qb) * 4, 4)?;
        self.check("kv", &kv, positions * heads * (qa + dv) * 4, 4)?;
        self.check("shared", &shared, positions * qb * 4, 4)?;
        self.check("out", &out, t * heads * dv * 4, 4)?;
        self.check_output(&out, &[&q, &kv, &shared])?;
        let pipeline = if wide {
            self.gpu.attention_wide.clone()
        } else {
            self.gpu.attention.clone()
        };
        self.encode(&pipeline, &[q, kv, shared, out], &args, groups);
        Ok(())
    }

    /// Causal grouped-query attention, optionally over a sliding window, with keys and
    /// values in a ring of `slots` rows (see [`GroupedShape`]): `q` and `out`
    /// `[t][heads][dim]`, `k` and `v` `[slots][kv_heads][dim]`, all float32.
    ///
    /// # Errors
    /// [`Error::Dispatch`] for an unsupported shape, a ring too small for the window, or
    /// slices that do not fit.
    pub fn attention_grouped(
        &mut self,
        q: Slice,
        k: Slice,
        v: Slice,
        out: Slice,
        shape: GroupedShape,
    ) -> Result<(), Error> {
        self.grouped(q, k, v, None, out, shape)
    }

    /// [`Self::attention_grouped`] with an attention sink per head: `sinks` holds one
    /// logit per query head that joins each softmax, taking probability without adding
    /// a value (gpt-oss).
    pub fn attention_grouped_with_sinks(
        &mut self,
        (q, k, v): (Slice, Slice, Slice),
        sinks: Slice,
        out: Slice,
        shape: GroupedShape,
    ) -> Result<(), Error> {
        self.grouped(q, k, v, Some(sinks), out, shape)
    }

    fn grouped(
        &mut self,
        q: Slice,
        k: Slice,
        v: Slice,
        sinks: Option<Slice>,
        out: Slice,
        shape: GroupedShape,
    ) -> Result<(), Error> {
        let GroupedShape {
            t,
            start,
            heads,
            kv_heads,
            dim,
            window,
            slots,
            scale,
        } = shape;
        let end = start + t;
        if t == 0
            || kv_heads == 0
            || heads % kv_heads != 0
            || dim == 0
            || dim % 32 != 0
            || dim > 512
            || window == 0
            || slots == 0
            || (slots < end && slots < window.saturating_add(t))
        {
            return Err(Error::Dispatch(format!(
                "unsupported grouped attention shape {shape:?}"
            )));
        }
        let narrow = |n: usize| {
            u32::try_from(n).map_err(|_| Error::Dispatch(format!("{n} does not fit in u32")))
        };
        let args = GroupedArgs {
            t: narrow(t)?,
            start: narrow(start)?,
            heads: narrow(heads)?,
            kv_heads: narrow(kv_heads)?,
            dim: narrow(dim)?,
            window: narrow(window.min(end))?,
            slots: narrow(slots)?,
            scale,
            sinks: u32::from(sinks.is_some()),
        };
        let groups = t * heads;
        narrow(groups)?;
        let rows = slots.min(end);
        self.check("q", &q, t * heads * dim * 4, 4)?;
        self.check("k", &k, rows * kv_heads * dim * 4, 4)?;
        self.check("v", &v, rows * kv_heads * dim * 4, 4)?;
        self.check("out", &out, t * heads * dim * 4, 4)?;
        self.check_output(&out, &[&q, &k, &v])?;
        let sinks = self.sinks(sinks, heads, &q)?;
        // Heads up to 128 wide score 32 keys per step, one per lane: far more of the
        // cache in flight when decoding at long context.
        let pipeline = if dim <= 128 {
            self.gpu.attention_grouped_narrow.clone()
        } else {
            self.gpu.attention_grouped.clone()
        };
        self.encode(&pipeline, &[q, k, v, out, sinks], &args, groups);
        Ok(())
    }

    /// The sinks slice to bind: the given one, checked, or `q` (not read) when there
    /// are none.
    fn sinks(&self, sinks: Option<Slice>, heads: usize, q: &Slice) -> Result<Slice, Error> {
        match sinks {
            Some(sinks) => {
                self.check("sinks", &sinks, heads * 4, 4)?;
                Ok(sinks)
            }
            None => Ok(Slice::new(q.buffer, q.offset, q.len)),
        }
    }

    /// [`Self::attention_grouped`] on the matrix units, 32 new positions per threadgroup
    /// and 32 keys per step: for a prompt pass. Needs `dim` a multiple of 16 and, when
    /// positions wrap (`slots < start + t`), `slots` a multiple of 32. Rows are read up to the next multiple of 32: `q` and
    /// `out` hold `t` rounded up to 32 rows, and `k` and `v` the rows of positions up to
    /// `start + t` rounded up, which must be finite (they are multiplied by zero).
    ///
    /// # Errors
    /// [`Error::Dispatch`] for an unsupported shape or slices that do not fit.
    pub fn attention_grouped_tiled(
        &mut self,
        q: Slice,
        k: Slice,
        v: Slice,
        out: Slice,
        shape: GroupedShape,
    ) -> Result<(), Error> {
        self.grouped_tiled(q, k, v, None, out, shape)
    }

    /// [`Self::attention_grouped_tiled`] with attention sinks, as
    /// [`Self::attention_grouped_with_sinks`].
    pub fn attention_grouped_tiled_with_sinks(
        &mut self,
        (q, k, v): (Slice, Slice, Slice),
        sinks: Slice,
        out: Slice,
        shape: GroupedShape,
    ) -> Result<(), Error> {
        self.grouped_tiled(q, k, v, Some(sinks), out, shape)
    }

    fn grouped_tiled(
        &mut self,
        q: Slice,
        k: Slice,
        v: Slice,
        sinks: Option<Slice>,
        out: Slice,
        shape: GroupedShape,
    ) -> Result<(), Error> {
        let GroupedShape {
            t,
            start,
            heads,
            kv_heads,
            dim,
            window,
            slots,
            scale,
        } = shape;
        let end = start + t;
        // Positions wrap only when the slots are fewer than them.
        let ring = slots < end;
        if t == 0
            || kv_heads == 0
            || heads % kv_heads != 0
            || dim == 0
            || dim % 16 != 0
            || dim > 512
            || window == 0
            || slots == 0
            || (ring && (slots % 32 != 0 || slots < window.saturating_add(t)))
        {
            return Err(Error::Dispatch(format!(
                "unsupported tiled grouped attention shape {shape:?}"
            )));
        }
        let narrow = |n: usize| {
            u32::try_from(n).map_err(|_| Error::Dispatch(format!("{n} does not fit in u32")))
        };
        let args = GroupedArgs {
            t: narrow(t)?,
            start: narrow(start)?,
            heads: narrow(heads)?,
            kv_heads: narrow(kv_heads)?,
            dim: narrow(dim)?,
            window: narrow(window.min(end))?,
            slots: narrow(slots)?,
            scale,
            sinks: u32::from(sinks.is_some()),
        };
        let groups = t.div_ceil(32) * heads;
        narrow(groups)?;
        let rows = if ring {
            slots
        } else {
            end.next_multiple_of(32)
        };
        let padded = t.next_multiple_of(32);
        self.check("q", &q, padded * heads * dim * 4, 4)?;
        self.check("k", &k, rows * kv_heads * dim * 4, 4)?;
        self.check("v", &v, rows * kv_heads * dim * 4, 4)?;
        self.check("out", &out, padded * heads * dim * 4, 4)?;
        self.check_output(&out, &[&q, &k, &v])?;
        let sinks = self.sinks(sinks, heads, &q)?;
        let pipeline = self.gpu.attention_grouped_tiled.clone();
        self.encode(&pipeline, &[q, k, v, out, sinks], &args, groups);
        Ok(())
    }

    /// [`Self::attention_split_key`] on the matrix units, 32 new positions per
    /// threadgroup and 32 cached positions per step: for many new positions at once (a
    /// prompt). Needs `qa` and `qb` multiples of 8 and `dv == 128`. Rows are read (and
    /// written) up to the next multiple of 32: `q` and `out` hold `t` rounded up to 32
    /// rows, `kv` and `shared` `cached + t` rounded up, and cache rows past
    /// `cached + t` must be finite (they are multiplied by zero).
    pub fn attention_split_key_tiled(
        &mut self,
        q: Slice,
        kv: Slice,
        shared: Slice,
        out: Slice,
        shape: AttentionShape,
    ) -> Result<(), Error> {
        let AttentionShape {
            t,
            cached,
            heads,
            qa,
            qb,
            dv,
            scale,
        } = shape;
        if t == 0
            || heads == 0
            || qa == 0
            || !qa.is_multiple_of(8)
            || !qb.is_multiple_of(8)
            || dv != 128
        {
            return Err(Error::Dispatch(format!(
                "unsupported tiled attention shape {shape:?}"
            )));
        }
        let (t_rows, rows) = (t.div_ceil(32) * 32, (cached + t).div_ceil(32) * 32);
        let args = AttentionArgs {
            t: narrow(t)?,
            cached: narrow(cached)?,
            heads: narrow(heads)?,
            qa: narrow(qa)?,
            qb: narrow(qb)?,
            dv: narrow(dv)?,
            scale,
        };
        let groups = t_rows / 32 * heads;
        narrow(groups)?;
        narrow(rows * heads * (qa + dv))?;
        self.check("q", &q, t_rows * heads * (qa + qb) * 4, 4)?;
        self.check("kv", &kv, rows * heads * (qa + dv) * 4, 4)?;
        self.check("shared", &shared, rows * qb * 4, 4)?;
        self.check("out", &out, t_rows * heads * dv * 4, 4)?;
        self.check_output(&out, &[&q, &kv, &shared])?;
        let pipeline = self.gpu.attention_tiled.clone();
        self.encode(&pipeline, &[q, kv, shared, out], &args, groups);
        Ok(())
    }

    /// The delta-rule recurrence with per-key-channel decay, `shape.t` steps in order.
    /// Per head, with state `S` `[dk][dv]`: `S = diag(alpha) S`, `u = S^T k`,
    /// `S += k (beta (v - u))^T`, `out = S^T q`. `q`, `k` and `alpha` are
    /// `[t][heads][dk]`, `v` and `out` `[t][heads][dv]`, `beta` `[t][heads]`, `state`
    /// `[heads][dk][dv]`, read and then updated in place; all `f32`. Needs `dk` even,
    /// `dk <= 128` and `dv <= 128`.
    #[allow(clippy::too_many_arguments)]
    pub fn delta_rule_recurrence(
        &mut self,
        (q, k, v, alpha, beta): (Slice, Slice, Slice, Slice, Slice),
        state: Slice,
        out: Slice,
        shape: RecurrenceShape,
    ) -> Result<(), Error> {
        let RecurrenceShape { t, heads, dk, dv } = shape;
        if t == 0 || heads == 0 || dk == 0 || dk % 2 != 0 || dk > 128 || dv == 0 || dv > 128 {
            return Err(Error::Dispatch(format!(
                "unsupported recurrence shape {shape:?}"
            )));
        }
        let narrow = |n: usize| {
            u32::try_from(n).map_err(|_| Error::Dispatch(format!("{n} does not fit in u32")))
        };
        let args = RecurrenceArgs {
            t: narrow(t)?,
            heads: narrow(heads)?,
            dk: narrow(dk)?,
            dv: narrow(dv)?,
        };
        narrow(t * heads * dk.max(dv))?;
        for (what, slice, floats) in [
            ("q", &q, t * heads * dk),
            ("k", &k, t * heads * dk),
            ("v", &v, t * heads * dv),
            ("alpha", &alpha, t * heads * dk),
            ("beta", &beta, t * heads),
            ("state", &state, heads * dk * dv),
            ("out", &out, t * heads * dv),
        ] {
            self.check(what, slice, floats * 4, 4)?;
        }
        self.check_output(&state, &[&q, &k, &v, &alpha, &beta, &out])?;
        self.check_output(&out, &[&q, &k, &v, &alpha, &beta])?;
        let pipeline = self.gpu.recurrence.clone();
        self.encode(&pipeline, &[q, k, v, alpha, beta, state, out], &args, heads);
        Ok(())
    }

    /// Depthwise causal convolution over `rows` rows of `channels` floats, then SiLU:
    /// `out[r][c] = silu(taps[c][K-1] x[r][c] + sum_h taps[c][h] input(r - (K-1) + h))`,
    /// where inputs before row 0 come from `history` (`[channels][K-1]`, oldest first).
    /// `taps` is `[channels][K]`, `K = kernel <= 8`. `history` is only read; update it
    /// with [`Self::causal_conv_history`] after a barrier.
    pub fn causal_conv_silu(
        &mut self,
        (x, taps, history): (Slice, Slice, Slice),
        out: Slice,
        rows: usize,
        channels: usize,
        kernel: usize,
    ) -> Result<(), Error> {
        let args = Self::conv_args(rows, channels, kernel)?;
        self.check("x", &x, rows * channels * 4, 4)?;
        self.check("taps", &taps, channels * kernel * 4, 4)?;
        self.check("history", &history, channels * (kernel - 1) * 4, 4)?;
        self.check("out", &out, rows * channels * 4, 4)?;
        self.check_output(&out, &[&x, &taps, &history])?;
        let pipeline = self.gpu.glue.conv.clone();
        let groups = (rows * channels).div_ceil(THREADS_PER_GROUP);
        self.encode(&pipeline, &[x, taps, history, out], &args, groups);
        Ok(())
    }

    /// Replaces `history` with each channel's last `kernel - 1` inputs of `x` (and, when
    /// `rows` is fewer, the newest of the old history before them).
    pub fn causal_conv_history(
        &mut self,
        x: Slice,
        history: Slice,
        rows: usize,
        channels: usize,
        kernel: usize,
    ) -> Result<(), Error> {
        let args = Self::conv_args(rows, channels, kernel)?;
        self.check("x", &x, rows * channels * 4, 4)?;
        self.check("history", &history, channels * (kernel - 1) * 4, 4)?;
        self.check_output(&history, &[&x])?;
        let pipeline = self.gpu.glue.conv_history.clone();
        let groups = channels.div_ceil(THREADS_PER_GROUP);
        self.encode(&pipeline, &[x, history], &args, groups);
        Ok(())
    }

    fn conv_args(rows: usize, channels: usize, kernel: usize) -> Result<ConvArgs, Error> {
        if rows == 0 || channels == 0 || !(2..=8).contains(&kernel) {
            return Err(Error::Dispatch(format!(
                "unsupported convolution: {rows} rows, {channels} channels, kernel {kernel}"
            )));
        }
        narrow(rows * channels)?;
        Ok(ConvArgs {
            rows: narrow(rows)?,
            channels: narrow(channels)?,
            width: narrow(kernel)?,
        })
    }

    /// In place over `rows` rows of `d` floats: `v = v / sqrt(sum(v^2) + eps) * scale`.
    pub fn l2norm_rows(
        &mut self,
        v: Slice,
        rows: usize,
        d: usize,
        eps: f32,
        scale: f32,
    ) -> Result<(), Error> {
        let args = Self::row_args(rows, d, eps, scale)?;
        self.check("v", &v, rows * d * 4, 4)?;
        self.check_output(&v, &[])?;
        let pipeline = self.gpu.glue.l2norm.clone();
        self.encode(&pipeline, &[v], &args, rows.div_ceil(ROWS_PER_GROUP));
        Ok(())
    }

    /// In place over `rows` rows of `d` floats:
    /// `v = w * v / sqrt(mean(v^2) + eps) * sigmoid(gate)`, `gate` shaped like `v`, `w`
    /// of `d` floats.
    pub fn rmsnorm_gated_rows(
        &mut self,
        v: Slice,
        (gate, w): (Slice, Slice),
        rows: usize,
        d: usize,
        eps: f32,
    ) -> Result<(), Error> {
        let args = Self::row_args(rows, d, eps, 1.0)?;
        self.check("v", &v, rows * d * 4, 4)?;
        self.check("gate", &gate, rows * d * 4, 4)?;
        self.check("w", &w, d * 4, 4)?;
        self.check_output(&v, &[&gate, &w])?;
        let pipeline = self.gpu.glue.rmsnorm_gated.clone();
        self.encode(
            &pipeline,
            &[v, gate, w],
            &args,
            rows.div_ceil(ROWS_PER_GROUP),
        );
        Ok(())
    }

    fn row_args(rows: usize, d: usize, eps: f32, scale: f32) -> Result<RowArgs, Error> {
        if rows == 0 || d == 0 {
            return Err(Error::Dispatch(format!("unsupported rows: {rows} x {d}")));
        }
        narrow(rows * d)?;
        Ok(RowArgs {
            rows: narrow(rows)?,
            d: narrow(d)?,
            eps,
            scale,
        })
    }

    /// `alpha[i] = exp(-exp(a_log[head]) * softplus(z[i] + bias[i % width]))` over `n`
    /// elements in rows of `width`, heads `d` wide (`a_log` has `width / d` entries).
    pub fn softplus_decay(
        &mut self,
        (z, a_log, bias): (Slice, Slice, Slice),
        alpha: Slice,
        n: usize,
        width: usize,
        d: usize,
    ) -> Result<(), Error> {
        if n == 0 || width == 0 || d == 0 || !width.is_multiple_of(d) || !n.is_multiple_of(width) {
            return Err(Error::Dispatch(format!(
                "unsupported decay: {n} elements, width {width}, heads {d} wide"
            )));
        }
        let args = DecayArgs {
            n: narrow(n)?,
            width: narrow(width)?,
            d: narrow(d)?,
        };
        self.check("z", &z, n * 4, 4)?;
        self.check("a_log", &a_log, width / d * 4, 4)?;
        self.check("bias", &bias, width * 4, 4)?;
        self.check("alpha", &alpha, n * 4, 4)?;
        self.check_output(&alpha, &[&z, &a_log, &bias])?;
        let pipeline = self.gpu.glue.decay.clone();
        let groups = n.div_ceil(THREADS_PER_GROUP);
        self.encode(&pipeline, &[z, a_log, bias, alpha], &args, groups);
        Ok(())
    }

    /// `v = sigmoid(v)` in place over `n` elements.
    pub fn sigmoid_in_place(&mut self, v: Slice, n: usize) -> Result<(), Error> {
        if n == 0 {
            return Err(Error::Dispatch("empty sigmoid".into()));
        }
        let args = DecayArgs {
            n: narrow(n)?,
            width: 1,
            d: 1,
        };
        self.check("v", &v, n * 4, 4)?;
        self.check_output(&v, &[])?;
        let pipeline = self.gpu.glue.sigmoid.clone();
        self.encode(&pipeline, &[v], &args, n.div_ceil(THREADS_PER_GROUP));
        Ok(())
    }

    /// `g = silu(g) * u` in place over `n` elements, `silu(x) = x * sigmoid(x)`.
    pub fn silu_mul(&mut self, g: Slice, u: Slice, n: usize) -> Result<(), Error> {
        if n == 0 {
            return Err(Error::Dispatch("empty silu_mul".into()));
        }
        let args = DecayArgs {
            n: narrow(n)?,
            width: 1,
            d: 1,
        };
        self.check("g", &g, n * 4, 4)?;
        self.check("u", &u, n * 4, 4)?;
        self.check_output(&g, &[&u])?;
        let pipeline = self.gpu.glue.silu_mul.clone();
        self.encode(&pipeline, &[g, u], &args, n.div_ceil(THREADS_PER_GROUP));
        Ok(())
    }

    /// In place over the first `d` floats of `rows` rows `stride` floats apart:
    /// `v = w * v / sqrt(mean(v^2) + eps)`, `w` of `d` floats.
    pub fn rmsnorm_rows(
        &mut self,
        (v, w): (Slice, Slice),
        rows: usize,
        (d, stride): (usize, usize),
        eps: f32,
    ) -> Result<(), Error> {
        if rows == 0 || d == 0 || stride < d {
            return Err(Error::Dispatch(format!(
                "unsupported rows: {rows} x {d}, stride {stride}"
            )));
        }
        let args = StridedArgs {
            rows: narrow(rows)?,
            d: narrow(d)?,
            stride: narrow(stride)?,
            eps,
        };
        narrow(rows * stride)?;
        self.check("v", &v, ((rows - 1) * stride + d) * 4, 4)?;
        self.check("w", &w, d * 4, 4)?;
        self.check_output(&v, &[&w])?;
        let pipeline = self.gpu.glue.rmsnorm.clone();
        self.encode(&pipeline, &[v, w], &args, rows.div_ceil(ROWS_PER_GROUP));
        Ok(())
    }

    /// Copies `width` floats of each of `rows` rows: `src` rows `src_stride` floats
    /// apart, `dst` rows `dst_stride` apart.
    pub fn copy_rows(
        &mut self,
        (src, src_stride): (Slice, usize),
        (dst, dst_stride): (Slice, usize),
        rows: usize,
        width: usize,
    ) -> Result<(), Error> {
        if rows == 0 || width == 0 || src_stride < width || dst_stride < width {
            return Err(Error::Dispatch(format!(
                "unsupported copy: {rows} x {width}, strides {src_stride} and {dst_stride}"
            )));
        }
        let args = CopyArgs {
            rows: narrow(rows)?,
            width: narrow(width)?,
            src_stride: narrow(src_stride)?,
            dst_stride: narrow(dst_stride)?,
        };
        narrow(rows * src_stride.max(dst_stride))?;
        self.check("src", &src, ((rows - 1) * src_stride + width) * 4, 4)?;
        self.check("dst", &dst, ((rows - 1) * dst_stride + width) * 4, 4)?;
        self.check_output(&dst, &[&src])?;
        let pipeline = self.gpu.glue.copy_rows.clone();
        let groups = (rows * width).div_ceil(THREADS_PER_GROUP);
        self.encode(&pipeline, &[src, dst], &args, groups);
        Ok(())
    }

    /// Checks a repacked `Q8_0` matrix: `codes` `rows * cols` signed bytes, `scales`
    /// one little-endian binary16 per 32 of them. Needs `cols % 32 == 0`.
    fn check_q8_0(
        &self,
        codes: &Slice,
        scales: &Slice,
        rows: usize,
        cols: usize,
    ) -> Result<(), Error> {
        if !cols.is_multiple_of(32) {
            return Err(Error::Dispatch(format!(
                "Q8_0 rows of {cols} are not whole blocks"
            )));
        }
        self.check("codes", codes, rows * cols, 16)?;
        self.check("scales", scales, rows * cols / 32 * 2, 2)
    }

    /// `y = W x` for a `rows x cols` matrix in repacked ggml `Q8_0`: `codes` holds each
    /// row's signed bytes in order, `scales` one binary16 per 32 (element `c` of row `r`
    /// is `scales[(r * cols + c) / 32] * codes[r * cols + c]`). Needs `cols % 32 == 0`;
    /// `codes` and `x` on 16-byte boundaries.
    pub fn gemv_q8_0(
        &mut self,
        (codes, scales): (Slice, Slice),
        x: Slice,
        y: Slice,
        rows: usize,
        cols: usize,
    ) -> Result<(), Error> {
        let args = Self::shape(rows, cols)?;
        self.check_q8_0(&codes, &scales, rows, cols)?;
        self.check("x", &x, cols * 4, 16)?;
        self.check("y", &y, rows * 4, 4)?;
        self.check_output(&y, &[&codes, &scales, &x])?;
        let pipeline = self.gpu.q8_0[self.gpu.rows.index()].clone();
        let groups = rows.div_ceil(SIMDGROUPS_PER_GROUP * self.gpu.rows.count());
        self.encode(&pipeline, &[codes, scales, x, y], &args, groups);
        Ok(())
    }

    /// As [`Self::gemm_mxfp4`] for a repacked `Q8_0` matrix (see [`Self::gemv_q8_0`]).
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q8_0(
        &mut self,
        (codes, scales): (Slice, Slice),
        x: Slice,
        y: Slice,
        rows: usize,
        cols: usize,
        n: usize,
        (x_stride, y_stride): (usize, usize),
    ) -> Result<(), Error> {
        let (args, x_len, y_len) = Self::gemm_args(rows, cols, n, x_stride, y_stride)?;
        self.check_q8_0(&codes, &scales, rows, cols)?;
        self.check("x", &x, x_len, 16)?;
        self.check("y", &y, y_len, 4)?;
        self.check_output(&y, &[&codes, &scales, &x])?;
        let pipeline = self.gpu.gemm_q8_0.clone();
        let groups = rows.div_ceil(SIMDGROUPS_PER_GROUP * GEMM_ROWS);
        self.encode(&pipeline, &[codes, scales, x, y], &args, groups);
        Ok(())
    }

    /// As [`Self::gemm_mxfp4_tiled`] for a repacked `Q8_0` matrix. Needs
    /// `rows % 64 == 0`, `cols % 32 == 0` and `n % 32 == 0` (pad the positions).
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q8_0_tiled(
        &mut self,
        (codes, scales): (Slice, Slice),
        x: Slice,
        y: Slice,
        rows: usize,
        cols: usize,
        n: usize,
        (x_stride, y_stride): (usize, usize),
    ) -> Result<(), Error> {
        if !rows.is_multiple_of(64) || !cols.is_multiple_of(32) || !n.is_multiple_of(32) {
            return Err(Error::Dispatch(format!(
                "tiled Q8_0 product needs rows % 64, cols % 32 and n % 32 == 0: {rows} x {cols}, n {n}"
            )));
        }
        let (args, x_len, y_len) = Self::gemm_args(rows, cols, n, x_stride, y_stride)?;
        self.check_q8_0(&codes, &scales, rows, cols)?;
        self.check("x", &x, x_len, 16)?;
        self.check("y", &y, y_len, 4)?;
        self.check_output(&y, &[&codes, &scales, &x])?;
        let pipeline = self.gpu.gemm_q8_0_tiled.clone();
        let groups = rows / 64 * (n / 32);
        narrow(groups)?;
        self.encode(&pipeline, &[codes, scales, x, y], &args, groups);
        Ok(())
    }

    /// `dst[r] += src[r * src_stride ..][..width]` for `rows` rows of `width` floats
    /// (`dst` packed); `src_stride` 0 adds one row, a bias, to every row.
    pub fn add_rows(
        &mut self,
        dst: Slice,
        (src, src_stride): (Slice, usize),
        rows: usize,
        width: usize,
    ) -> Result<(), Error> {
        if rows == 0 || width == 0 || (src_stride != 0 && src_stride < width) {
            return Err(Error::Dispatch(format!(
                "unsupported add: {rows} x {width}, source stride {src_stride}"
            )));
        }
        let args = AddArgs {
            rows: narrow(rows)?,
            width: narrow(width)?,
            src_stride: narrow(src_stride)?,
        };
        narrow(rows * width)?;
        self.check("dst", &dst, rows * width * 4, 4)?;
        self.check("src", &src, ((rows - 1) * src_stride + width) * 4, 4)?;
        self.check_output(&dst, &[&src])?;
        let pipeline = self.gpu.glue.add_rows.clone();
        let groups = (rows * width).div_ceil(THREADS_PER_GROUP);
        self.encode(&pipeline, &[dst, src], &args, groups);
        Ok(())
    }

    /// Rotary position embedding by halves, in place: `x` is `rows` rows of `heads`
    /// heads of `dim` floats; `table` holds per row `dim / 2` pairs `(cos, sin)`, and
    /// element `i` of a head's first half turns with element `i` of its second.
    pub fn rotate_halves(
        &mut self,
        x: Slice,
        table: Slice,
        rows: usize,
        (heads, dim): (usize, usize),
    ) -> Result<(), Error> {
        if rows == 0 || heads == 0 || dim == 0 || !dim.is_multiple_of(2) {
            return Err(Error::Dispatch(format!(
                "unsupported rotation: {rows} rows of {heads} x {dim}"
            )));
        }
        let args = RotateArgs {
            rows: narrow(rows)?,
            heads: narrow(heads)?,
            dim: narrow(dim)?,
        };
        narrow(rows * heads * dim)?;
        self.check("x", &x, rows * heads * dim * 4, 4)?;
        self.check("table", &table, rows * dim * 4, 8)?;
        self.check_output(&x, &[&table])?;
        let pipeline = self.gpu.glue.rotate_halves.clone();
        let groups = (rows * heads * dim / 2).div_ceil(THREADS_PER_GROUP);
        self.encode(&pipeline, &[x, table], &args, groups);
        Ok(())
    }

    /// gpt-oss's clamped SwiGLU over `rows` rows of `width`, biases broadcast:
    /// `g = min(gate + gb, limit)`, `u = clamp(up + ub, -limit, limit)`,
    /// `out = (u + 1) * g * sigmoid(alpha * g)`.
    #[allow(clippy::too_many_arguments)]
    pub fn clamped_swiglu(
        &mut self,
        (gate, up): (Slice, Slice),
        (gate_bias, up_bias): (Slice, Slice),
        out: Slice,
        rows: usize,
        width: usize,
        (limit, alpha): (f32, f32),
    ) -> Result<(), Error> {
        if rows == 0 || width == 0 {
            return Err(Error::Dispatch("empty clamped_swiglu".into()));
        }
        let args = GluArgs {
            rows: narrow(rows)?,
            width: narrow(width)?,
            limit,
            alpha,
        };
        narrow(rows * width)?;
        for (name, slice) in [("gate", &gate), ("up", &up), ("out", &out)] {
            self.check(name, slice, rows * width * 4, 4)?;
        }
        self.check("gate_bias", &gate_bias, width * 4, 4)?;
        self.check("up_bias", &up_bias, width * 4, 4)?;
        self.check_output(&out, &[&gate, &up, &gate_bias, &up_bias])?;
        let pipeline = self.gpu.glue.clamped_swiglu.clone();
        let groups = (rows * width).div_ceil(THREADS_PER_GROUP);
        self.encode(
            &pipeline,
            &[gate, up, gate_bias, up_bias, out],
            &args,
            groups,
        );
        Ok(())
    }

    /// `y = W x (+ b)` for a row-major float32 `rows x cols` matrix, one simdgroup per
    /// row: for small matrices such as a router. Needs `cols % 4 == 0`; `w` and `x` on
    /// 16-byte boundaries.
    pub fn gemv_f32(
        &mut self,
        (w, bias): (Slice, Option<Slice>),
        x: Slice,
        y: Slice,
        rows: usize,
        cols: usize,
    ) -> Result<(), Error> {
        if rows == 0 || cols == 0 || !cols.is_multiple_of(4) {
            return Err(Error::Dispatch(format!(
                "unsupported f32 product {rows} x {cols}"
            )));
        }
        let args = BiasedGemvArgs {
            rows: narrow(rows)?,
            cols: narrow(cols)?,
            bias: u32::from(bias.is_some()),
        };
        self.check("weights", &w, rows * cols * 4, 16)?;
        self.check("x", &x, cols * 4, 16)?;
        self.check("y", &y, rows * 4, 4)?;
        let b = match bias {
            Some(b) => {
                self.check("bias", &b, rows * 4, 4)?;
                b
            }
            None => x,
        };
        self.check_output(&y, &[&w, &x, &b])?;
        let pipeline = self.gpu.moe.gemv_f32.clone();
        self.encode(
            &pipeline,
            &[w, x, b, y],
            &args,
            rows.div_ceil(SIMDGROUPS_PER_GROUP),
        );
        Ok(())
    }

    /// For each of `n` rows of `experts` logits: the `k` largest (equal logits: the
    /// lower index first) into `ids` (`u32`, `n x k`) and their softmax over just those
    /// into `weights` (`n x k`). Needs `k <= 16` and `k <= experts`.
    pub fn topk_softmax(
        &mut self,
        logits: Slice,
        (ids, weights): (Slice, Slice),
        n: usize,
        (experts, k): (usize, usize),
    ) -> Result<(), Error> {
        if n == 0 || k == 0 || k > 16 || k > experts {
            return Err(Error::Dispatch(format!(
                "unsupported top-{k} of {experts} over {n} rows"
            )));
        }
        let args = TopkArgs {
            n: narrow(n)?,
            experts: narrow(experts)?,
            k: narrow(k)?,
        };
        self.check("logits", &logits, n * experts * 4, 4)?;
        self.check("ids", &ids, n * k * 4, 4)?;
        self.check("weights", &weights, n * k * 4, 4)?;
        self.check_output(&ids, &[&logits])?;
        self.check_output(&weights, &[&logits, &ids])?;
        let pipeline = self.gpu.moe.topk.clone();
        self.encode(
            &pipeline,
            &[logits, ids, weights],
            &args,
            n.div_ceil(THREADS_PER_GROUP),
        );
        Ok(())
    }

    /// `y[j] = W[ids[j]] x[j]` for `j < k`: products with MXFP4 matrices chosen on the
    /// GPU. `elements` and `scales` hold the matrices one after another, each `rows x
    /// cols` laid out as in [`Self::gemv_mxfp4`]; `ids` holds `k` `u32` indices. `x[j]`
    /// starts `j * x_stride` floats in (0: one `x` for all), `y[j]` at `j * rows`.
    /// Needs `cols % 32 == 0`. Indices are not bounds-checked on the GPU: `matrices`,
    /// how many the buffers hold, is checked here, and callers write `ids` with
    /// [`Self::topk_softmax`] over that many.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_mxfp4_selected(
        &mut self,
        (elements, scales, matrices): (Slice, Slice, usize),
        ids: Slice,
        (x, x_stride): (Slice, usize),
        y: Slice,
        (rows, cols): (usize, usize),
        k: usize,
    ) -> Result<(), Error> {
        let GemvArgs { .. } = Self::shape(rows, cols)?;
        if !cols.is_multiple_of(32) || k == 0 || matrices == 0 || (x_stride != 0 && x_stride < cols)
        {
            return Err(Error::Dispatch(format!(
                "unsupported selected MXFP4 product: {k} of {matrices} {rows} x {cols}, x stride {x_stride}"
            )));
        }
        self.check("elements", &elements, matrices * rows * cols / 2, 16)?;
        self.check("scales", &scales, matrices * rows * cols / 32, 1)?;
        self.check("ids", &ids, k * 4, 4)?;
        self.check("x", &x, ((k - 1) * x_stride + cols) * 4, 16)?;
        self.check("y", &y, k * rows * 4, 4)?;
        self.check_output(&y, &[&elements, &scales, &ids, &x])?;
        let per = SIMDGROUPS_PER_GROUP * self.gpu.rows.count();
        let groups_per_matrix = rows.div_ceil(per);
        let args = SelectedArgs {
            rows: narrow(rows)?,
            cols: narrow(cols)?,
            k: narrow(k)?,
            x_stride: narrow(x_stride)?,
            groups_per_matrix: narrow(groups_per_matrix)?,
        };
        let pipeline = self.gpu.moe.selected[self.gpu.rows.index()].clone();
        self.encode(
            &pipeline,
            &[elements, scales, ids, x, y],
            &args,
            k * groups_per_matrix,
        );
        Ok(())
    }

    /// [`Self::clamped_swiglu`] over `k` rows whose biases are rows `ids[j]` of
    /// `gate_bias` and `up_bias` (`experts x width` each).
    #[allow(clippy::too_many_arguments)]
    pub fn clamped_swiglu_selected(
        &mut self,
        (gate, up): (Slice, Slice),
        (gate_bias, up_bias, experts): (Slice, Slice, usize),
        ids: Slice,
        out: Slice,
        (k, width): (usize, usize),
        (limit, alpha): (f32, f32),
    ) -> Result<(), Error> {
        if k == 0 || width == 0 || experts == 0 {
            return Err(Error::Dispatch("empty clamped_swiglu_selected".into()));
        }
        let args = GluArgs {
            rows: narrow(k)?,
            width: narrow(width)?,
            limit,
            alpha,
        };
        for (name, slice) in [("gate", &gate), ("up", &up), ("out", &out)] {
            self.check(name, slice, k * width * 4, 4)?;
        }
        self.check("gate_bias", &gate_bias, experts * width * 4, 4)?;
        self.check("up_bias", &up_bias, experts * width * 4, 4)?;
        self.check("ids", &ids, k * 4, 4)?;
        self.check_output(&out, &[&gate, &up, &gate_bias, &up_bias, &ids])?;
        let pipeline = self.gpu.moe.swiglu.clone();
        let groups = (k * width).div_ceil(THREADS_PER_GROUP);
        self.encode(
            &pipeline,
            &[gate, up, gate_bias, up_bias, ids, out],
            &args,
            groups,
        );
        Ok(())
    }

    /// `h += sum_j weights[j] * (y[j] + bias[ids[j]])` over `k` expert outputs of
    /// `width` (`bias` is `experts x width`).
    pub fn moe_combine(
        &mut self,
        h: Slice,
        (y, bias, experts): (Slice, Slice, usize),
        (ids, weights): (Slice, Slice),
        (k, width): (usize, usize),
    ) -> Result<(), Error> {
        if k == 0 || width == 0 || experts == 0 {
            return Err(Error::Dispatch("empty moe_combine".into()));
        }
        let args = CombineArgs {
            width: narrow(width)?,
            k: narrow(k)?,
        };
        self.check("h", &h, width * 4, 4)?;
        self.check("y", &y, k * width * 4, 4)?;
        self.check("bias", &bias, experts * width * 4, 4)?;
        self.check("ids", &ids, k * 4, 4)?;
        self.check("weights", &weights, k * 4, 4)?;
        self.check_output(&h, &[&y, &bias, &ids, &weights])?;
        let pipeline = self.gpu.moe.combine.clone();
        self.encode(
            &pipeline,
            &[h, y, bias, ids, weights],
            &args,
            width.div_ceil(THREADS_PER_GROUP),
        );
        Ok(())
    }

    pub fn barrier(&mut self) {
        self.encoder
            .memoryBarrierWithScope(MTLBarrierScope::Buffers);
    }

    /// Sends the batch to the GPU. When it finishes, `on_done` runs as a job on
    /// `proactor` with the batch's buffers. Nothing blocks. If the proactor has shut down
    /// by then, the buffers are dropped instead.
    pub fn commit<P, F>(mut self, proactor: &ProactorHandle<P>, on_done: F)
    where
        P: CompletionPort,
        ProactorHandle<P>: Send,
        F: FnOnce(Completed) + Send + 'static,
    {
        self.encoder.endEncoding();
        self.ended = true;
        let buffers = std::mem::take(&mut self.buffers);
        // Residents stay referenced until the GPU is done with them.
        let residents = std::mem::take(&mut self.arenas);
        let pending = Mutex::new(Some((buffers, residents, on_done, proactor.clone())));
        let pending = Arc::new(pending);
        let handler = RcBlock::new(
            move |command: NonNull<ProtocolObject<dyn MTLCommandBuffer>>| {
                // SAFETY: Metal passes the finished command buffer, valid for this call.
                let command = unsafe { command.as_ref() };
                let gpu_time = if command.status() == MTLCommandBufferStatus::Completed {
                    let seconds = command.GPUEndTime() - command.GPUStartTime();
                    Ok(Duration::from_secs_f64(seconds.max(0.0)))
                } else {
                    Err(Error::Gpu(command.error().map_or_else(
                        || format!("status {:?}", command.status()),
                        |e| e.localizedDescription().to_string(),
                    )))
                };
                let taken = pending.lock().ok().and_then(|mut slot| slot.take());
                if let Some((buffers, residents, on_done, proactor)) = taken {
                    drop(residents);
                    let _ =
                        proactor.enqueue_work(move |_| on_done(Completed { buffers, gpu_time }));
                }
            },
        );
        // SAFETY: Metal copies the block before `addCompletedHandler` returns.
        unsafe { self.command.addCompletedHandler(RcBlock::as_ptr(&handler)) };
        self.command.commit();
    }
}
