//! Streamed-weight dense projections through public Core ML APIs, for weights that
//! change on every call (a model streamed from disk a layer at a time, never resident).
//!
//! One compiled [`encode_dynamic_matmul`] model per (row bucket, tile) shape takes the
//! weight tile as a runtime input. Weights and activations are written, as fp16, into
//! reusable IOSurface-backed `MLMultiArray`s. Measured on the M4 Pro (2026-09-24), a
//! plain heap `MLMultiArray` weight input is copied/relaid by Core ML at about 3.4 GB/s;
//! an IOSurface-backed one is read by the Neural Engine at 24-29 GB/s, the same speed as
//! baked weights. The ANE accepts at most [`ANE_MAX_DIM`] per dimension, and throughput
//! falls for tiles above ~12288, so matrices are tiled to [`TILE`]: row tiles are
//! independent, column tiles are summed in fp32 here.
//!
//! Numerics are fp16 on the device: inputs are rounded to fp16, and a bf16 weight is
//! exact in fp16 except below fp16's normal range (flushed toward zero). A value outside
//! fp16's finite range is refused with an error, never saturated, so the caller can
//! compute that call on its own reference path instead.
//!
//! Predictions are synchronous: this is called from a compute loop that would otherwise
//! be running the same product on the CPU, never from a proactor completion handler or a
//! UI callback. `CPUAndNeuralEngine` permits CPU fallback; the compute plan's preferred
//! device for every loaded model is recorded in [`DenseStats`], so fallback is visible.
use crate::apple::plan;
use crate::model::{encode_dynamic_matmul, ANE_MAX_DIM};
use block2::RcBlock;
use half::{
    f16,
    slice::{HalfBitsSliceExt, HalfFloatSliceExt},
};
use loadngo_inference::compute::{ComputePolicy, DeviceKind};
use objc2::{
    rc::{autoreleasepool, Retained},
    runtime::{AnyObject, ProtocolObject},
    AnyThread,
};
use objc2_core_foundation::{CFDictionary, CFRetained};
use objc2_core_ml::*;
use objc2_core_video::{
    kCVPixelBufferIOSurfacePropertiesKey, kCVPixelFormatType_OneComponent16Half, CVPixelBuffer,
    CVPixelBufferCreate, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSURL};
use std::{
    collections::HashMap,
    path::PathBuf,
    ptr::NonNull,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

/// Largest tile edge. Measured: 4096..12288-wide tiles stream 24-29 GB/s, 16384 about 12-18.
pub const TILE: usize = 12288;
/// Rows per prediction; more rows are split. Row counts are padded to a power of two so a
/// handful of compiled shapes serve every sequence length.
pub const MAX_ROWS: usize = 256;
const _: () = assert!(TILE <= ANE_MAX_DIM && MAX_ROWS <= ANE_MAX_DIM);
const FP16_MAX: f32 = 65504.0;

#[derive(Debug, Clone, Default)]
pub struct DenseStats {
    /// `matmul_bf16` calls that completed on this engine.
    pub calls: u64,
    pub predictions: u64,
    /// Compiled shapes, and how many of them the compute plan did not place on the NPU.
    pub models: usize,
    pub models_not_on_npu: usize,
    pub compile_and_load_s: f64,
    /// Time spent converting weights and activations into fp16 surfaces.
    pub convert_s: f64,
    /// Time inside Core ML predictions, including reading the fp16 result back.
    pub predict_s: f64,
    /// bf16 weight bytes consumed.
    pub weight_bytes: u64,
}

struct Surface {
    buffer: CFRetained<CVPixelBuffer>,
    array: Retained<MLMultiArray>,
    rows: usize,
    cols: usize,
}

impl Surface {
    fn new(rows: usize, cols: usize) -> Result<Self, String> {
        // SAFETY: public CoreVideo/Core ML constructors. The attributes dictionary is a
        // toll-free-bridged NSDictionary that outlives the call; the created pixel buffer
        // is owned by `buffer` (+1 from Create) and retained again by the array.
        unsafe {
            let key: &NSString =
                &*(kCVPixelBufferIOSurfacePropertiesKey as *const _ as *const NSString);
            let empty = NSDictionary::<NSString, AnyObject>::new();
            let attributes = NSDictionary::from_slices(&[key], &[&*empty as &AnyObject]);
            let mut raw: *mut CVPixelBuffer = std::ptr::null_mut();
            let status = CVPixelBufferCreate(
                None,
                cols,
                rows,
                kCVPixelFormatType_OneComponent16Half,
                Some(&*(Retained::as_ptr(&attributes) as *const CFDictionary)),
                NonNull::from(&mut raw),
            );
            let raw = NonNull::new(raw)
                .filter(|_| status == 0)
                .ok_or_else(|| format!("CVPixelBufferCreate {cols}x{rows} failed: {status}"))?;
            let buffer = CFRetained::from_raw(raw);
            let shape = NSArray::from_retained_slice(&[
                NSNumber::new_usize(rows),
                NSNumber::new_usize(cols),
            ]);
            let array =
                MLMultiArray::initWithPixelBuffer_shape(MLMultiArray::alloc(), &buffer, &shape);
            Ok(Self {
                buffer,
                array,
                rows,
                cols,
            })
        }
    }

    /// Calls `row(r, dst)` for every row with that row's `cols` fp16 bit patterns.
    fn fill(&mut self, mut row: impl FnMut(usize, &mut [u16])) -> Result<(), String> {
        // SAFETY: the base address is valid for `rows * bytes_per_row` bytes while locked;
        // each row slice lies inside that span and only one exists at a time.
        unsafe {
            let status = CVPixelBufferLockBaseAddress(&self.buffer, CVPixelBufferLockFlags(0));
            if status != 0 {
                return Err(format!("CVPixelBufferLockBaseAddress failed: {status}"));
            }
            let base = CVPixelBufferGetBaseAddress(&self.buffer).cast::<u16>();
            let stride = CVPixelBufferGetBytesPerRow(&self.buffer) / 2;
            let ok = !base.is_null() && stride >= self.cols;
            if ok {
                for r in 0..self.rows {
                    row(
                        r,
                        std::slice::from_raw_parts_mut(base.add(r * stride), self.cols),
                    );
                }
            }
            CVPixelBufferUnlockBaseAddress(&self.buffer, CVPixelBufferLockFlags(0));
            if ok {
                Ok(())
            } else {
                Err("pixel buffer has no usable base address".into())
            }
        }
    }
}

struct Loaded {
    model: Retained<MLModel>,
    compiled: Retained<NSURL>,
}

pub struct DenseEngine {
    units: MLComputeUnits,
    tile: usize,
    dir: PathBuf,
    models: HashMap<(usize, usize, usize), Loaded>,
    weights: HashMap<(usize, usize), Surface>,
    inputs: HashMap<(usize, usize), Surface>,
    staging: Vec<f32>,
    stats: DenseStats,
}

static ENGINE_ID: AtomicU64 = AtomicU64::new(0);

/// bf16 words to fp16 bits with round-to-nearest-even (fp16 subnormals included).
/// Returns false, leaving `dst` partly written, when a value is not finite in fp16.
fn bf16_to_f16(dst: &mut [u16], src: &[u16], staging: &mut Vec<f32>) -> bool {
    assert!(dst.len() >= src.len());
    #[cfg(target_arch = "aarch64")]
    let done = {
        let done = src.len() / 8 * 8;
        // SAFETY: every 8-lane load/store lies inside `src[..done]` / `dst[..done]`,
        // and `dst.len() >= src.len()` is asserted above.
        if !unsafe { bf16_to_f16_neon(dst.as_mut_ptr(), src.as_ptr(), done) } {
            return false;
        }
        done
    };
    #[cfg(not(target_arch = "aarch64"))]
    let done = 0;
    let src = &src[done..];
    staging.clear();
    staging.extend(src.iter().map(|&b| f32::from_bits(u32::from(b) << 16)));
    if !staging.iter().all(|w| w.abs() <= FP16_MAX) {
        return false;
    }
    dst[done..done + src.len()]
        .reinterpret_cast_mut::<f16>()
        .convert_from_f32_slice(staging);
    true
}

/// `shll` widens bf16 to fp32 exactly (a 16-bit left shift); `fcvtn` narrows to fp16
/// under the default FPCR (round to nearest even, no flush-to-zero). A lane whose fp16
/// exponent is all ones is an overflow or a NaN, and fails the whole call.
#[cfg(target_arch = "aarch64")]
unsafe fn bf16_to_f16_neon(dst: *mut u16, src: *const u16, n: usize) -> bool {
    use std::arch::aarch64::{
        uint16x8_t, vandq_u16, vceqq_u16, vdupq_n_u16, vld1q_u16, vmaxvq_u16, vorrq_u16, vst1q_u16,
    };
    let exponent = vdupq_n_u16(0x7c00);
    let mut bad = vdupq_n_u16(0);
    let mut i = 0;
    while i < n {
        let b = vld1q_u16(src.add(i));
        let h: uint16x8_t;
        std::arch::asm!(
            "shll   {lo:v}.4s, {b:v}.4h, #16",
            "shll2  {hi:v}.4s, {b:v}.8h, #16",
            "fcvtn  {h:v}.4h, {lo:v}.4s",
            "fcvtn2 {h:v}.8h, {hi:v}.4s",
            b = in(vreg) b,
            lo = out(vreg) _,
            hi = out(vreg) _,
            h = out(vreg) h,
            options(pure, nomem, nostack),
        );
        bad = vorrq_u16(bad, vceqq_u16(vandq_u16(h, exponent), exponent));
        vst1q_u16(dst.add(i), h);
        i += 8;
    }
    vmaxvq_u16(bad) == 0
}

/// OCP MX v1.0 block size: one E8M0 scale per 32 elements.
pub const MX_BLOCK: usize = 32;

/// E2M1 code -> value, OCP MX v1.0 table (sign in bit 3).
const E2M1: [f32; 16] = [
    0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0, -0.0, -0.5, -1.0, -1.5, -2.0, -3.0, -4.0, -6.0,
];

/// Expands MXFP4 blocks to fp16 bits: `dst.len()` elements (a multiple of 32) from
/// `dst.len() / 2` packed bytes and `dst.len() / 32` scales.
fn mxfp4_to_f16(dst: &mut [u16], packed: &[u8], scales: &[u8]) -> Result<(), String> {
    let (blocks, _) = dst.as_chunks_mut::<MX_BLOCK>();
    let (bytes, _) = packed.as_chunks::<{ MX_BLOCK / 2 }>();
    for ((d, p), &s) in blocks.iter_mut().zip(bytes).zip(scales) {
        if s == 0xFF {
            return Err("MXFP4 block has a NaN scale".into());
        }
        let k = i32::from(s) - 127;
        // E2M1 magnitudes have fp16 exponent fields 14 (0.5) ..= 17 (6); adding k keeps
        // every nonzero value fp16-normal exactly when -13 <= k <= 13.
        if (-13..=13).contains(&k) {
            #[cfg(target_arch = "aarch64")]
            {
                // SAFETY: `d` is 32 u16 and `p` 16 bytes, exactly one block.
                unsafe { mxfp4_block_neon(d.as_mut_ptr(), p.as_ptr(), k) };
                continue;
            }
        }
        // s is 0..=254, so 2^k is an f32 (subnormal only for s == 0), exactly.
        let scale = 2f32.powi(k);
        for (i, slot) in d.iter_mut().enumerate() {
            let byte = p[i / 2];
            let code = if i % 2 == 0 { byte & 0x0F } else { byte >> 4 };
            let v = f16::from_f32(E2M1[usize::from(code)] * scale);
            if !v.is_finite() {
                return Err("MXFP4 value outside fp16 range".into());
            }
            *slot = v.to_bits();
        }
    }
    Ok(())
}

/// One 32-element block: `tbl` looks the E2M1 codes up as fp16 bit patterns (low and
/// high bytes separately), then the scale is added into the exponent field of every
/// nonzero lane. Signed zero stays signed zero.
#[cfg(target_arch = "aarch64")]
unsafe fn mxfp4_block_neon(dst: *mut u16, packed: *const u8, k: i32) {
    use std::arch::aarch64::{
        vaddq_u16, vandq_u16, vandq_u8, vdupq_n_u16, vdupq_n_u8, vld1q_u8, vqtbl1q_u8,
        vreinterpretq_u16_u8, vshrq_n_u8, vst1q_u16, vtstq_u16, vzip1q_u8, vzip2q_u8,
    };
    const BITS: [u16; 16] = [
        0x0000, 0x3800, 0x3C00, 0x3E00, 0x4000, 0x4200, 0x4400, 0x4600, 0x8000, 0xB800, 0xBC00,
        0xBE00, 0xC000, 0xC200, 0xC400, 0xC600,
    ];
    let lo_bytes: [u8; 16] = BITS.map(|b| b as u8);
    let hi_bytes: [u8; 16] = BITS.map(|b| (b >> 8) as u8);
    let table_lo = vld1q_u8(lo_bytes.as_ptr());
    let table_hi = vld1q_u8(hi_bytes.as_ptr());
    let bias = vdupq_n_u16((k << 10) as u16);
    let magnitude = vdupq_n_u16(0x7FFF);
    let b = vld1q_u8(packed);
    let lo = vandq_u8(b, vdupq_n_u8(0x0F));
    let hi = vshrq_n_u8::<4>(b);
    for (half, codes) in [vzip1q_u8(lo, hi), vzip2q_u8(lo, hi)]
        .into_iter()
        .enumerate()
    {
        let l = vqtbl1q_u8(table_lo, codes);
        let h = vqtbl1q_u8(table_hi, codes);
        for (quarter, v) in [vzip1q_u8(l, h), vzip2q_u8(l, h)].into_iter().enumerate() {
            let v = vreinterpretq_u16_u8(v);
            let nonzero = vtstq_u16(v, magnitude);
            let v = vaddq_u16(v, vandq_u16(bias, nonzero));
            vst1q_u16(dst.add(half * 16 + quarter * 8), v);
        }
    }
}

fn f32_to_f16(dst: &mut [u16], src: &[f32]) -> bool {
    if !src.iter().all(|v| v.abs() <= FP16_MAX) {
        return false;
    }
    dst.reinterpret_cast_mut::<f16>()
        .convert_from_f32_slice(src);
    true
}

impl DenseEngine {
    pub fn new(policy: ComputePolicy) -> Result<Self, String> {
        Self::with_tile(policy, TILE)
    }

    /// As [`Self::new`] with a smaller tile edge (1..=[`TILE`]), for tests and tuning.
    pub fn with_tile(policy: ComputePolicy, tile: usize) -> Result<Self, String> {
        if !(1..=TILE).contains(&tile) {
            return Err(format!("tile {tile} is outside 1..={TILE}"));
        }
        if crate::available_devices().is_empty() {
            return Err("Core ML dense engine requires macOS 15+".into());
        }
        let id = ENGINE_ID.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("loadngo-coreml-dense-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(Self {
            tile,
            units: match policy {
                ComputePolicy::CpuOnly => MLComputeUnits::CPUOnly,
                ComputePolicy::CpuAndNpu => MLComputeUnits::CPUAndNeuralEngine,
            },
            dir,
            models: HashMap::new(),
            weights: HashMap::new(),
            inputs: HashMap::new(),
            staging: Vec::new(),
            stats: DenseStats::default(),
        })
    }

    #[must_use]
    pub fn stats(&self) -> &DenseStats {
        &self.stats
    }

    fn model(&mut self, rows: usize, inputs: usize, outputs: usize) -> Result<&Loaded, String> {
        let key = (rows, inputs, outputs);
        if !self.models.contains_key(&key) {
            let start = Instant::now();
            let source = self
                .dir
                .join(format!("mm-{rows}x{inputs}x{outputs}.mlmodel"));
            std::fs::write(&source, encode_dynamic_matmul(rows, inputs, outputs)?)
                .map_err(|e| format!("{}: {e}", source.display()))?;
            let path = source.to_str().ok_or("temporary path is not UTF-8")?;
            // SAFETY: public Core ML compile/load/plan APIs on a file this engine wrote.
            let (loaded, placements) = unsafe {
                let url = NSURL::fileURLWithPath(&NSString::from_str(path));
                #[allow(deprecated)]
                let compiled = MLModel::compileModelAtURL_error(&url).map_err(|e| e.to_string())?;
                let config = MLModelConfiguration::new();
                config.setComputeUnits(self.units);
                let model = MLModel::modelWithContentsOfURL_configuration_error(&compiled, &config)
                    .map_err(|e| e.to_string())?;
                let placements = plan(&compiled, &config)?;
                (Loaded { model, compiled }, placements)
            };
            let _ = std::fs::remove_file(&source);
            self.stats.models += 1;
            if !placements.iter().all(|p| p.preferred == DeviceKind::Npu) {
                self.stats.models_not_on_npu += 1;
            }
            self.stats.compile_and_load_s += start.elapsed().as_secs_f64();
            self.models.insert(key, loaded);
        }
        Ok(&self.models[&key])
    }

    /// `y[r][o] = sum_i w[o][i] * x[r][i]` for `rows` rows; `w` holds bf16 words
    /// `[outputs][inputs]`, `x` is `[rows][inputs]`, `y` is `[rows][outputs]`.
    ///
    /// # Errors
    /// Returns an error, with `y` in an unspecified state, when a weight, activation or
    /// result is not finite in fp16, or Core ML fails. The caller should then compute the
    /// product on its reference path.
    pub fn matmul_bf16(
        &mut self,
        w: &[u16],
        x: &[f32],
        y: &mut [f32],
        rows: usize,
        inputs: usize,
        outputs: usize,
    ) -> Result<(), String> {
        if w.len() < inputs * outputs {
            return Err(format!("bf16 weight {} < {outputs}x{inputs}", w.len()));
        }
        let mut staging = std::mem::take(&mut self.staging);
        let result = self.matmul(x, y, rows, inputs, outputs, 1, |row, i0, dst| {
            let at = row * inputs + i0;
            if bf16_to_f16(dst, &w[at..at + dst.len()], &mut staging) {
                Ok(())
            } else {
                Err("weight outside fp16 range".into())
            }
        });
        self.staging = staging;
        result?;
        self.stats.weight_bytes += (inputs * outputs * 2) as u64;
        Ok(())
    }

    /// As [`Self::matmul_bf16`] for an OCP MX v1.0 MXFP4 matrix: `packed` is
    /// `[outputs][inputs / 2]` E2M1 codes, two per byte with the even element in the low
    /// nibble; `scales` is `[outputs][inputs / 32]` E8M0 exponents. Each 32-element
    /// block is expanded to fp16 in the weight surface (exact whenever the scaled value
    /// is fp16-normal, hardware-rounded otherwise). A NaN scale (0xFF), or a value
    /// beyond fp16's range, fails the call.
    ///
    /// # Errors
    /// As [`Self::matmul_bf16`]; also when `inputs` is not a multiple of 32.
    #[allow(clippy::too_many_arguments)]
    pub fn matmul_mxfp4(
        &mut self,
        packed: &[u8],
        scales: &[u8],
        x: &[f32],
        y: &mut [f32],
        rows: usize,
        inputs: usize,
        outputs: usize,
    ) -> Result<(), String> {
        if !inputs.is_multiple_of(MX_BLOCK)
            || packed.len() < outputs * inputs / 2
            || scales.len() < outputs * inputs / MX_BLOCK
        {
            return Err(format!(
                "MXFP4 {outputs}x{inputs}: needs inputs % 32 == 0 and {} packed / {} scale bytes, got {} / {}",
                outputs * inputs / 2,
                outputs * inputs / MX_BLOCK,
                packed.len(),
                scales.len()
            ));
        }
        self.matmul(x, y, rows, inputs, outputs, MX_BLOCK, |row, i0, dst| {
            let p = row * inputs / 2 + i0 / 2;
            let s = row * inputs / MX_BLOCK + i0 / MX_BLOCK;
            mxfp4_to_f16(
                dst,
                &packed[p..p + dst.len() / 2],
                &scales[s..s + dst.len() / MX_BLOCK],
            )
        })?;
        self.stats.weight_bytes += (inputs * outputs / 2 + outputs * inputs / MX_BLOCK) as u64;
        Ok(())
    }

    /// Tiles `y = x . W^T` and runs every tile; `fill(row, i0, dst)` writes weight row
    /// `row`, columns `i0..i0 + dst.len()`, as fp16 bits. Column tiles start on
    /// multiples of `align`.
    #[allow(clippy::too_many_arguments)]
    fn matmul(
        &mut self,
        x: &[f32],
        y: &mut [f32],
        rows: usize,
        inputs: usize,
        outputs: usize,
        align: usize,
        mut fill: impl FnMut(usize, usize, &mut [u16]) -> Result<(), String>,
    ) -> Result<(), String> {
        if rows == 0
            || inputs == 0
            || outputs == 0
            || x.len() < rows * inputs
            || y.len() < rows * outputs
        {
            return Err(format!(
                "matmul {rows}x{inputs} -> {outputs}: buffers {}/{} are too small",
                x.len(),
                y.len()
            ));
        }
        if !self.tile.is_multiple_of(align) {
            return Err(format!("tile {} is not a multiple of {align}", self.tile));
        }
        let tile = self.tile;
        for o0 in (0..outputs).step_by(tile) {
            let ot = tile.min(outputs - o0);
            for i0 in (0..inputs).step_by(tile) {
                let it = tile.min(inputs - i0);
                let start = Instant::now();
                let surface = match self.weights.entry((ot, it)) {
                    std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                    std::collections::hash_map::Entry::Vacant(e) => e.insert(Surface::new(ot, it)?),
                };
                let mut status = Ok(());
                surface.fill(|r, dst| {
                    if status.is_ok() {
                        status = fill(o0 + r, i0, dst);
                    }
                })?;
                status?;
                self.stats.convert_s += start.elapsed().as_secs_f64();
                for r0 in (0..rows).step_by(MAX_ROWS) {
                    let rt = MAX_ROWS.min(rows - r0);
                    self.predict_tile(x, y, (r0, rt), (i0, it), (o0, ot), inputs, outputs)?;
                }
            }
        }
        self.stats.calls += 1;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn predict_tile(
        &mut self,
        x: &[f32],
        y: &mut [f32],
        (r0, rt): (usize, usize),
        (i0, it): (usize, usize),
        (o0, ot): (usize, usize),
        inputs: usize,
        outputs: usize,
    ) -> Result<(), String> {
        let bucket = rt.next_power_of_two();
        let start = Instant::now();
        let input = match self.inputs.entry((bucket, it)) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => e.insert(Surface::new(bucket, it)?),
        };
        let mut finite = true;
        input.fill(|r, dst| {
            if r < rt {
                let at = (r0 + r) * inputs + i0;
                finite &= f32_to_f16(dst, &x[at..at + it]);
            } else {
                dst.fill(0);
            }
        })?;
        if !finite {
            return Err("activation outside fp16 range".into());
        }
        let x_array = input.array.clone();
        let w_array = self.weights[&(ot, it)].array.clone();
        self.stats.convert_s += start.elapsed().as_secs_f64();
        let start = Instant::now();
        let model = self.model(bucket, it, ot)?.model.clone();
        let accumulate = i0 > 0;
        // SAFETY: public Core ML prediction on arrays this engine owns; the output
        // buffer is only read inside its accessor block, honoring its strides.
        let result = autoreleasepool(|_| unsafe {
            let keys = [NSString::from_str("x"), NSString::from_str("w")];
            let x_value = MLFeatureValue::featureValueWithMultiArray(&x_array);
            let w_value = MLFeatureValue::featureValueWithMultiArray(&w_array);
            let objects: [&AnyObject; 2] = [&x_value, &w_value];
            let dict = NSDictionary::from_slices(&[&*keys[0], &*keys[1]], &objects);
            let provider = MLDictionaryFeatureProvider::initWithDictionary_error(
                MLDictionaryFeatureProvider::alloc(),
                &dict,
            )
            .map_err(|e| e.to_string())?;
            let out = model
                .predictionFromFeatures_error(ProtocolObject::from_ref(&*provider))
                .map_err(|e| e.to_string())?;
            let array = out
                .featureValueForName(&NSString::from_str("y"))
                .and_then(|v| v.multiArrayValue())
                .ok_or("prediction has no y array")?;
            let strides: Vec<usize> = array.strides().iter().map(|n| n.as_usize()).collect();
            let shape: Vec<usize> = array.shape().iter().map(|n| n.as_usize()).collect();
            let dtype = array.dataType();
            if shape != [bucket, ot] || strides.len() != 2 {
                return Err(format!("unexpected output shape {shape:?}"));
            }
            let got = std::cell::RefCell::new(Err("output accessor did not run".to_string()));
            let read = RcBlock::new(|ptr: NonNull<std::ffi::c_void>, size: isize| {
                let width = if dtype == MLMultiArrayDataType::Float16 {
                    2
                } else {
                    4
                };
                let need = ((rt - 1) * strides[0] + (ot - 1) * strides[1] + 1) * width;
                if size < 0 || (size as usize) < need {
                    *got.borrow_mut() = Err("output buffer smaller than its shape".into());
                    return;
                }
                let mut values = Vec::with_capacity(rt * ot);
                for r in 0..rt {
                    for o in 0..ot {
                        let k = r * strides[0] + o * strides[1];
                        values.push(if width == 2 {
                            f16::from_bits(ptr.as_ptr().cast::<u16>().add(k).read()).to_f32()
                        } else {
                            ptr.as_ptr().cast::<f32>().add(k).read()
                        });
                    }
                }
                *got.borrow_mut() = Ok(values);
            });
            array.getBytesWithHandler(&read);
            drop(read);
            got.into_inner()
        });
        let result = result.and_then(|values| {
            if values.iter().any(|v| !v.is_finite()) {
                return Err("result outside fp16 range".to_string());
            }
            for (r, row) in values.chunks_exact(ot).enumerate() {
                let dst = &mut y[(r0 + r) * outputs + o0..][..ot];
                if accumulate {
                    for (d, v) in dst.iter_mut().zip(row) {
                        *d += v;
                    }
                } else {
                    dst.copy_from_slice(row);
                }
            }
            Ok(())
        });
        self.stats.predict_s += start.elapsed().as_secs_f64();
        self.stats.predictions += 1;
        result
    }
}

impl Drop for DenseEngine {
    fn drop(&mut self) {
        for loaded in self.models.values() {
            // SAFETY: reading the path of an NSURL this engine owns.
            if let Some(path) = loaded.compiled.path() {
                let _ = std::fs::remove_dir_all(path.to_string());
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bf16(v: f32) -> u16 {
        (v.to_bits() >> 16) as u16
    }

    fn sample(i: usize, seed: u32) -> f32 {
        let x = (i as u32)
            .wrapping_mul(1_664_525)
            .wrapping_add(seed.wrapping_mul(1_013_904_223));
        ((x >> 16) as f32 - 32768.0) / 32768.0
    }

    fn check(policy: ComputePolicy, tile: usize, rows: usize, inputs: usize, outputs: usize) {
        let w: Vec<u16> = (0..inputs * outputs)
            .map(|i| bf16(sample(i, 7) / 16.0))
            .collect();
        let x: Vec<f32> = (0..rows * inputs).map(|i| sample(i, 3)).collect();
        let mut reference = vec![0.0_f64; rows * outputs];
        for r in 0..rows {
            for o in 0..outputs {
                reference[r * outputs + o] = (0..inputs)
                    .map(|i| {
                        f64::from(f16::from_f32(x[r * inputs + i]).to_f32())
                            * f64::from(f32::from_bits(u32::from(w[o * inputs + i]) << 16))
                    })
                    .sum();
            }
        }
        let mut engine = DenseEngine::with_tile(policy, tile).unwrap();
        let mut y = vec![f32::NAN; rows * outputs];
        engine
            .matmul_bf16(&w, &x, &mut y, rows, inputs, outputs)
            .unwrap();
        let (mut num, mut den) = (0.0, 0.0);
        for (g, r) in y.iter().zip(&reference) {
            num += (f64::from(*g) - r).powi(2);
            den += r * r;
        }
        let rel = (num / den).sqrt();
        assert!(
            rel < 2e-3,
            "{policy:?} tile {tile} {rows}x{inputs}->{outputs}: rel RMS {rel}"
        );
        // Every output tile x input tile x row chunk ran exactly once.
        let expected = outputs.div_ceil(tile) * inputs.div_ceil(tile) * rows.div_ceil(MAX_ROWS);
        assert_eq!(engine.stats().predictions, expected as u64);
    }

    #[test]
    fn untiled_product_matches_reference_on_cpu_and_npu() {
        for policy in [ComputePolicy::CpuOnly, ComputePolicy::CpuAndNpu] {
            check(policy, TILE, 5, 96, 80);
        }
    }

    #[test]
    fn tiled_rows_columns_and_row_chunks_sum_correctly() {
        // 3 output tiles x 2 input tiles, rows padded to a power of two and split once.
        check(ComputePolicy::CpuAndNpu, 64, MAX_ROWS + 3, 100, 150);
    }

    #[test]
    fn bf16_to_f16_matches_hardware_rounding_for_every_bf16_value() {
        let mut staging = Vec::new();
        let mut converted = 0;
        for bits in 0..=u16::MAX {
            let wide = f32::from_bits(u32::from(bits) << 16);
            // Eight copies take the vector path; one takes the scalar remainder.
            let mut dst = [0_u16; 9];
            let ok = bf16_to_f16(&mut dst, &[bits; 9], &mut staging);
            assert!(
                !ok || dst.iter().all(|&h| h == dst[0]),
                "{bits:#06x} lanes differ"
            );
            if wide.is_nan() || wide.abs() > FP16_MAX {
                assert!(!ok, "{bits:#06x} must be refused");
            } else {
                assert!(ok, "{bits:#06x} must convert");
                assert_eq!(dst[0], f16::from_f32(wide).to_bits(), "{bits:#06x}");
                converted += 1;
            }
        }
        // Finite in fp16: every bf16 pattern with |x| <= 65504.
        assert_eq!(converted, 36_608);
    }

    #[test]
    fn mxfp4_expansion_matches_hardware_rounding_for_every_code_and_scale() {
        for scale in 0..=254_u8 {
            // One block holding all 16 codes twice, in both nibble positions.
            let packed: Vec<u8> = (0..16_u8).map(|i| i | ((15 - i) << 4)).collect();
            let mut dst = [0_u16; MX_BLOCK];
            let k = i32::from(scale) - 127;
            let ok = mxfp4_to_f16(&mut dst, &packed, &[scale]);
            let expect: Vec<Option<u16>> = (0..MX_BLOCK)
                .map(|i| {
                    let byte = packed[i / 2];
                    let code = if i % 2 == 0 { byte & 0x0F } else { byte >> 4 };
                    let v = f16::from_f64(f64::from(E2M1[usize::from(code)]) * 2f64.powi(k));
                    v.is_finite().then(|| v.to_bits())
                })
                .collect();
            if expect.iter().all(Option::is_some) {
                ok.unwrap_or_else(|e| panic!("scale {scale}: {e}"));
                for (i, (&got, want)) in dst.iter().zip(&expect).enumerate() {
                    assert_eq!(got, want.unwrap(), "scale {scale} element {i}");
                }
            } else {
                assert!(
                    ok.is_err(),
                    "scale {scale} overflows fp16 and must be refused"
                );
            }
        }
        assert!(mxfp4_to_f16(&mut [0; MX_BLOCK], &[0; 16], &[0xFF]).is_err());
    }

    #[test]
    fn tiled_mxfp4_product_matches_reference() {
        let (rows, inputs, outputs) = (3, 192, 100);
        let packed: Vec<u8> = (0..outputs * inputs / 2)
            .map(|i| (i * 37 % 251) as u8)
            .collect();
        let scales: Vec<u8> = (0..outputs * inputs / MX_BLOCK)
            .map(|i| 120 + (i % 9) as u8)
            .collect();
        let x: Vec<f32> = (0..rows * inputs).map(|i| sample(i, 11)).collect();
        let mut reference = vec![0.0_f64; rows * outputs];
        for r in 0..rows {
            for o in 0..outputs {
                reference[r * outputs + o] = (0..inputs)
                    .map(|i| {
                        let byte = packed[(o * inputs + i) / 2];
                        let code = if i % 2 == 0 { byte & 0x0F } else { byte >> 4 };
                        let scale = 2f64.powi(i32::from(scales[(o * inputs + i) / MX_BLOCK]) - 127);
                        f64::from(E2M1[usize::from(code)])
                            * scale
                            * f64::from(f16::from_f32(x[r * inputs + i]).to_f32())
                    })
                    .sum();
            }
        }
        let mut engine = DenseEngine::with_tile(ComputePolicy::CpuAndNpu, 64).unwrap();
        let mut y = vec![f32::NAN; rows * outputs];
        engine
            .matmul_mxfp4(&packed, &scales, &x, &mut y, rows, inputs, outputs)
            .unwrap();
        let (mut num, mut den) = (0.0, 0.0);
        for (g, r) in y.iter().zip(&reference) {
            num += (f64::from(*g) - r).powi(2);
            den += r * r;
        }
        assert!((num / den).sqrt() < 2e-3, "rel RMS {}", (num / den).sqrt());
        assert_eq!(engine.stats().predictions, 2 * 3);
        // A tile edge that splits an MX block is refused rather than misread.
        let mut odd = DenseEngine::with_tile(ComputePolicy::CpuOnly, 48).unwrap();
        assert!(odd
            .matmul_mxfp4(&packed, &scales, &x, &mut y, rows, inputs, outputs)
            .is_err());
    }

    /// `cargo test --release -p loadngo-coreml -- --ignored --nocapture conversion_rate`
    #[test]
    #[ignore = "timing only"]
    fn conversion_rate() {
        let src: Vec<u16> = (0..64 << 20).map(|i| bf16(sample(i, 5) / 8.0)).collect();
        let mut dst = vec![0_u16; src.len()];
        let mut staging = Vec::new();
        assert!(bf16_to_f16(&mut dst, &src, &mut staging)); // first touch of dst
        let start = Instant::now();
        assert!(bf16_to_f16(&mut dst, &src, &mut staging));
        let s = start.elapsed().as_secs_f64();
        println!(
            "bf16->fp16: {:.1} GB/s of bf16",
            (src.len() * 2) as f64 / s / 1e9
        );
        let start = Instant::now();
        dst.copy_from_slice(&src);
        let s = start.elapsed().as_secs_f64();
        println!(
            "memcpy baseline: {:.1} GB/s",
            (src.len() * 2) as f64 / s / 1e9
        );
        let packed: Vec<u8> = (0..src.len() / 2).map(|i| (i * 37 % 251) as u8).collect();
        let scales: Vec<u8> = (0..src.len() / MX_BLOCK)
            .map(|i| 120 + (i % 9) as u8)
            .collect();
        assert!(mxfp4_to_f16(&mut dst, &packed, &scales).is_ok());
        let start = Instant::now();
        assert!(mxfp4_to_f16(&mut dst, &packed, &scales).is_ok());
        let s = start.elapsed().as_secs_f64();
        println!(
            "mxfp4->fp16: {:.1} G elements/s",
            src.len() as f64 / s / 1e9
        );
    }

    #[test]
    fn out_of_range_values_are_refused_not_saturated() {
        let mut engine = DenseEngine::new(ComputePolicy::CpuOnly).unwrap();
        let mut y = vec![0.0; 4];
        let big = vec![bf16(1.0e6); 8];
        assert!(engine
            .matmul_bf16(&big, &[1.0; 2], &mut y, 1, 2, 4)
            .is_err());
        let ok = vec![bf16(1.0); 8];
        assert!(engine
            .matmul_bf16(&ok, &[1.0e6, 0.0], &mut y, 1, 2, 4)
            .is_err());
        assert!(engine
            .matmul_bf16(&ok, &[f32::NAN, 0.0], &mut y, 1, 2, 4)
            .is_err());
        assert!(engine
            .matmul_bf16(&ok[..7], &[1.0; 2], &mut y, 1, 2, 4)
            .is_err());
        engine
            .matmul_bf16(&ok, &[1.0, 2.0], &mut y, 1, 2, 4)
            .unwrap();
        assert_eq!(y, [3.0; 4]);
    }
}
