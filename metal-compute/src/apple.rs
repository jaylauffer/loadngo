//! Metal device, buffers and batches. The only unsafe code in the crate.

use std::ops::Range;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};
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
    bf16: [Pipeline; 3],
    mxfp4: [Pipeline; 3],
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
        for p in bf16.iter().chain(&mxfp4) {
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
            bf16,
            mxfp4,
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
            .newBufferWithLength_options(len, MTLResourceOptions::StorageModeShared)
            .ok_or(Error::Alloc(len))?;
        Ok(Buffer { raw, len })
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

/// Bytes `offset..offset + len` of the batch's buffer number `buffer`.
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

impl Batch<'_> {
    pub fn dispatches(&self) -> usize {
        self.dispatches
    }

    fn check(&self, what: &str, slice: &Slice, len: usize, align: usize) -> Result<(), Error> {
        let Some(buffer) = self.buffers.get(slice.buffer) else {
            return Err(Error::Dispatch(format!(
                "{what}: no buffer {} (batch has {})",
                slice.buffer,
                self.buffers.len()
            )));
        };
        if slice.len != len {
            return Err(Error::Dispatch(format!(
                "{what}: {} bytes, the shape needs {len}",
                slice.len
            )));
        }
        if slice.range().end > buffer.len() {
            return Err(Error::Dispatch(format!(
                "{what}: bytes {:?} outside a {}-byte buffer",
                slice.range(),
                buffer.len()
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

    fn check_output(y: &Slice, inputs: &[&Slice]) -> Result<(), Error> {
        if inputs.iter().any(|input| y.overlaps(input)) {
            return Err(Error::Dispatch("output overlaps an input".into()));
        }
        Ok(())
    }

    fn encode(&mut self, pipeline: &Pipeline, slices: &[Slice], args: &GemvArgs, rows: usize) {
        let per_group = SIMDGROUPS_PER_GROUP * self.gpu.rows.count();
        let enc = &self.encoder;
        enc.setComputePipelineState(pipeline);
        for (index, slice) in slices.iter().enumerate() {
            // SAFETY: the slice was checked against its buffer, which this batch owns (and
            // the command buffer retains) until the GPU has finished.
            unsafe {
                enc.setBuffer_offset_atIndex(
                    Some(&self.buffers[slice.buffer].raw),
                    slice.offset,
                    index,
                );
            }
        }
        // SAFETY: `GemvArgs` is `repr(C)` and matches the kernel's `constant GemvArgs &`.
        unsafe {
            enc.setBytes_length_atIndex(
                NonNull::from(args).cast(),
                std::mem::size_of::<GemvArgs>(),
                slices.len(),
            );
        }
        enc.dispatchThreadgroups_threadsPerThreadgroup(
            MTLSize {
                width: rows.div_ceil(per_group),
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
        Self::check_output(&y, &[&w, &x])?;
        let pipeline = self.gpu.bf16[self.gpu.rows.index()].clone();
        self.encode(&pipeline, &[w, x, y], &args, rows);
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
        Self::check_output(&y, &[&elements, &scales, &x])?;
        let pipeline = self.gpu.mxfp4[self.gpu.rows.index()].clone();
        self.encode(&pipeline, &[elements, scales, x, y], &args, rows);
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
        let pending = Mutex::new(Some((buffers, on_done, proactor.clone())));
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
                if let Some((buffers, on_done, proactor)) = taken {
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
