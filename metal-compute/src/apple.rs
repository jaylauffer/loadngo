//! Metal device, buffers and batches. The only unsafe code in the crate.

use std::ops::Range;
use std::ptr::NonNull;
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
    bf16: [Pipeline; 3],
    mxfp4: [Pipeline; 3],
    gemm_bf16: Pipeline,
    gemm_mxfp4: Pipeline,
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
        let gemm_bf16 = pipeline("gemm_bf16")?;
        let gemm_mxfp4 = pipeline("gemm_mxfp4")?;
        for p in bf16.iter().chain(&mxfp4).chain([&gemm_bf16, &gemm_mxfp4]) {
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
            bf16,
            mxfp4,
            gemm_bf16,
            gemm_mxfp4,
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
                let arena = Arc::new(Arena {
                    raw: buffer.raw,
                    len: buffer.len,
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
        // SAFETY: `A` is `GemvArgs` or `GemmArgs`, `repr(C)` like the kernel's argument struct.
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

    /// Makes every later dispatch see the buffer writes of every earlier one. Needed only
    /// in a [`Dispatch::Concurrent`] batch.
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
