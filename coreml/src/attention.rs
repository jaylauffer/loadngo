//! Grouped-query attention on the Neural Engine, completions through a loadngo proactor.
//!
//! One compiled [`encode_grouped_attention`] model per shape (key/value heads, group,
//! query rows, key rows, head width). Queries, keys, values and the window/causal mask
//! are fp16 in IOSurface-backed arrays, the layout the Neural Engine reads fastest (see
//! [`crate::dense`]). [`AttentionEngine::submit`] starts an asynchronous Core ML
//! prediction and returns at once; Core ML's completion handler reads the result and
//! posts it to the caller's proactor as a job, so the caller's own loop receives it like
//! any other completion and nothing blocks on the Neural Engine.
//!
//! Keys and values stay in a [`KvCache`] per attention layer, owned by the caller: a
//! pass writes only its own positions' rows, unless the cache is missing or behind (a
//! new or restored session), when it writes them all. Queries and the mask go into one
//! of two input sets per shape, so the next pass can be written while one runs; a third
//! submission of a shape while both are in flight, or a second for a cache still in
//! flight, is refused (bounded admission), never queued.
//!
//! Shapes are bucketed so a conversation needs few compiled models: queries to a power
//! of two, and keys to every slot (a sliding window's ring, from its first pass) or, for
//! a cache with many more slots than positions, to a power of two from [`MIN_KEYS`] up to
//! [`crate::model::ANE_MAX_DIM`]. A cache whose bucket grows is rebuilt once per bucket.
//!
//! Numerics: fp16 inputs and outputs, softmax on the device. A query that sees no key is
//! given uniform weights by the finite mask value; callers never ask for one.
use crate::apple::plan;
use crate::dense::{f32_to_f16, Surface};
use crate::model::{encode_grouped_attention, GroupedAttentionShape, ANE_MAX_DIM};
use block2::RcBlock;
use half::f16;
use loadngo_inference::compute::{ComputePolicy, DeviceKind};
use loadngo_proactor::{CompletionPort, ProactorHandle};
use objc2::{
    rc::{autoreleasepool, Retained},
    runtime::{AnyObject, ProtocolObject},
    AnyThread,
};
use objc2_core_ml::*;
use objc2_foundation::{NSDictionary, NSError, NSString, NSURL};
use std::{
    collections::HashMap,
    path::PathBuf,
    ptr::NonNull,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

/// The additive mask for a key a query does not see: far below any score, finite in fp16.
const HIDDEN: f32 = -30000.0;

/// The smallest key bucket for caches without wrap-around.
pub const MIN_KEYS: usize = 1024;

/// One attention pass, in the layout of loadngo-metal-compute's `attention_grouped`:
/// new position `i` (absolute `start + i`) and head `h` attend to positions
/// `max(0, start + i + 1 - window) ..= start + i`, head `h` to key/value head
/// `h / (heads / kv_heads)`; position `s` is in row `s % slots` of `k` and `v`.
pub struct AttentionPass<'a> {
    /// `[t][heads][dim]`.
    pub q: &'a [f32],
    /// `[min(slots, start + t)][kv_heads][dim]`, this pass's positions already written.
    pub k: &'a [f32],
    pub v: &'a [f32],
    pub t: usize,
    pub start: usize,
    pub heads: usize,
    pub kv_heads: usize,
    pub dim: usize,
    /// Positions each query sees, its own included; `usize::MAX` for all.
    pub window: usize,
    pub slots: usize,
}

/// A finished pass: `out [t][heads][dim]`, and the time from submission to the end of
/// Core ML's completion handler.
pub struct AttentionOutput {
    pub out: Vec<f32>,
    pub latency: Duration,
}

#[derive(Debug, Clone, Default)]
pub struct AttentionStats {
    pub models: usize,
    pub models_not_on_npu: usize,
    pub compile_and_load_s: f64,
    /// Writing queries, new key/value rows and the mask into the fp16 surfaces.
    pub fill_s: f64,
    pub submitted: u64,
    /// Key/value rows written, and how many caches were (re)written whole.
    pub kv_rows: u64,
    pub kv_rewrites: u64,
}

struct Inputs {
    q: Surface,
    mask: Surface,
    busy: AtomicBool,
}

struct KvSurfaces {
    k: Surface,
    v: Surface,
    busy: AtomicBool,
}

// SAFETY: input sets and key/value surfaces are shared between the submitting thread and
// Core ML's completion handler. The handler only stores `busy` (atomic) and releases its
// reference, and Objective-C retain/release is thread-safe. The surfaces are written only
// by the submitting thread, and only after it has itself set `busy` from false (no
// prediction holds them then); Core ML reads them only between that submission and the
// handler.
unsafe impl Send for Inputs {}
unsafe impl Sync for Inputs {}
unsafe impl Send for KvSurfaces {}
unsafe impl Sync for KvSurfaces {}

/// One attention layer's keys and values on the Neural Engine, kept between passes so a
/// pass writes only its new rows. Owned by the caller (one per layer and session); a
/// cache from another shape or session is replaced, not misread.
pub struct KvCache {
    surfaces: Arc<KvSurfaces>,
    keys: usize,
    width: usize,
    /// Positions `0..synced` are in the surfaces, each in row `position % slots`.
    synced: usize,
    slots: usize,
}

impl KvCache {
    /// Positions the cache holds.
    #[must_use]
    pub const fn synced(&self) -> usize {
        self.synced
    }

    /// Whether a prediction still reads it.
    pub fn is_busy(&self) -> bool {
        self.surfaces.busy.load(Ordering::Acquire)
    }
}

struct Loaded {
    model: Retained<MLModel>,
    compiled: Retained<NSURL>,
    /// Placement per layer: (layer, preferred device).
    placements: Vec<(String, DeviceKind)>,
    inputs: [Arc<Inputs>; 2],
}

pub struct AttentionEngine {
    units: MLComputeUnits,
    dir: PathBuf,
    models: HashMap<GroupedAttentionShape, Loaded>,
    stats: AttentionStats,
}

static ENGINE_ID: AtomicU64 = AtomicU64::new(0);

impl AttentionEngine {
    /// # Errors
    /// When the temporary model directory cannot be created.
    pub fn new(policy: ComputePolicy) -> Result<Self, String> {
        let dir = std::env::temp_dir().join(format!(
            "loadngo-coreml-attention-{}-{}",
            std::process::id(),
            ENGINE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(Self {
            units: match policy {
                ComputePolicy::CpuOnly => MLComputeUnits::CPUOnly,
                ComputePolicy::CpuAndNpu => MLComputeUnits::CPUAndNeuralEngine,
            },
            dir,
            models: HashMap::new(),
            stats: AttentionStats::default(),
        })
    }

    pub fn stats(&self) -> &AttentionStats {
        &self.stats
    }

    /// The compute plan's preferred device per layer of `shape`'s model, once loaded.
    pub fn placements(&self, shape: GroupedAttentionShape) -> Option<&[(String, DeviceKind)]> {
        self.models.get(&shape).map(|l| l.placements.as_slice())
    }

    /// The model shape a pass runs in, or `None` when it does not fit the Neural Engine:
    /// queries padded to a power of two; keys every slot when positions wrap or the
    /// slots are within twice the bucket, otherwise the positions so far rounded up to a
    /// power of two from [`MIN_KEYS`].
    #[must_use]
    pub fn shape_of(pass: &AttentionPass<'_>) -> Option<GroupedAttentionShape> {
        let end = pass.start + pass.t;
        let bucket = end.next_power_of_two().max(MIN_KEYS);
        // The whole ring once it is within twice the bucket (a sliding layer's 1,536
        // slots from its first pass), so the cache is not rebuilt as it fills.
        let keys = if pass.slots <= 2 * bucket {
            pass.slots
        } else {
            bucket
        };
        let shape = GroupedAttentionShape {
            kv_heads: pass.kv_heads,
            group: pass.heads.checked_div(pass.kv_heads)?,
            queries: pass.t.next_power_of_two(),
            keys,
            dim: pass.dim,
        };
        let widest = shape.heads() * shape.dim;
        (keys <= ANE_MAX_DIM && shape.queries <= ANE_MAX_DIM && widest <= ANE_MAX_DIM)
            .then_some(shape)
    }

    /// Compiles and loads `shape`'s model if it is not loaded yet.
    ///
    /// # Errors
    /// Core ML compile/load failures.
    pub fn prepare(&mut self, shape: GroupedAttentionShape) -> Result<(), String> {
        if self.models.contains_key(&shape) {
            return Ok(());
        }
        let start = Instant::now();
        let source = self.dir.join(format!(
            "attn-{}x{}x{}x{}x{}.mlmodel",
            shape.kv_heads, shape.group, shape.queries, shape.keys, shape.dim
        ));
        std::fs::write(&source, encode_grouped_attention(shape)?)
            .map_err(|e| format!("{}: {e}", source.display()))?;
        let path = source.to_str().ok_or("temporary path is not UTF-8")?;
        // SAFETY: public Core ML compile/load/plan APIs on a file this engine wrote.
        let (model, compiled, placements) = unsafe {
            let url = NSURL::fileURLWithPath(&NSString::from_str(path));
            #[allow(deprecated)]
            let compiled = MLModel::compileModelAtURL_error(&url).map_err(|e| e.to_string())?;
            let config = MLModelConfiguration::new();
            config.setComputeUnits(self.units);
            let model = MLModel::modelWithContentsOfURL_configuration_error(&compiled, &config)
                .map_err(|e| e.to_string())?;
            let placements = plan(&compiled, &config)?;
            (model, compiled, placements)
        };
        let _ = std::fs::remove_file(&source);
        let set = || -> Result<Arc<Inputs>, String> {
            Ok(Arc::new(Inputs {
                q: Surface::new(shape.queries, shape.heads() * shape.dim)?,
                mask: Surface::new(shape.queries, shape.keys)?,
                busy: AtomicBool::new(false),
            }))
        };
        let placements: Vec<(String, DeviceKind)> = placements
            .into_iter()
            .map(|p| (p.operation, p.preferred))
            .collect();
        self.stats.models += 1;
        if placements
            .iter()
            .any(|(_, device)| *device != DeviceKind::Npu)
        {
            self.stats.models_not_on_npu += 1;
        }
        self.stats.compile_and_load_s += start.elapsed().as_secs_f64();
        self.models.insert(
            shape,
            Loaded {
                model,
                compiled,
                placements,
                inputs: [set()?, set()?],
            },
        );
        Ok(())
    }

    /// Starts `pass` on the Neural Engine and returns at once. `cache` is the layer's
    /// keys and values on the Neural Engine: created or replaced when missing or of
    /// another shape, otherwise brought up to date with this pass's rows. `on_done` runs
    /// later on `proactor`'s loop with the result (or the error), exactly once.
    ///
    /// # Errors
    /// A pass that does not fit the Neural Engine, a value outside fp16, Core ML failures,
    /// the cache still in flight, or both input sets of the shape in flight. Nothing was
    /// submitted then, and `on_done` is dropped without running; the cache is left as it
    /// was, or emptied if it was being written.
    pub fn submit<P, F>(
        &mut self,
        pass: &AttentionPass<'_>,
        cache: &mut Option<KvCache>,
        proactor: &ProactorHandle<P>,
        on_done: F,
    ) -> Result<(), String>
    where
        P: CompletionPort,
        ProactorHandle<P>: Send,
        F: FnOnce(Result<AttentionOutput, String>) + Send + 'static,
    {
        let (t, heads, kvh, dim) = (pass.t, pass.heads, pass.kv_heads, pass.dim);
        if t == 0 || kvh == 0 || heads % kvh != 0 || pass.slots == 0 || pass.window == 0 {
            return Err("unsupported attention pass".into());
        }
        let end = pass.start + t;
        let rows = pass.slots.min(end);
        let row = kvh * dim;
        if pass.q.len() != t * heads * dim || pass.k.len() < rows * row || pass.v.len() < rows * row
        {
            return Err("attention pass buffers do not match its shape".into());
        }
        if pass.slots < end && pass.slots < pass.window.saturating_add(t) {
            return Err("the ring is smaller than the window plus the pass".into());
        }
        let shape = Self::shape_of(pass).ok_or("the pass does not fit the Neural Engine")?;
        if cache.as_ref().is_some_and(KvCache::is_busy) {
            return Err("the layer's keys and values are still in flight".into());
        }
        self.prepare(shape)?;
        let fill_start = Instant::now();
        let kv = match self.write_kv(pass, shape, cache) {
            Ok(kv) => kv,
            Err(e) => {
                *cache = None;
                return Err(e);
            }
        };
        let loaded = &self.models[&shape];
        let Some(inputs) = loaded
            .inputs
            .iter()
            .find(|set| !set.busy.swap(true, Ordering::AcqRel))
            .cloned()
        else {
            return Err("both input sets are in flight".into());
        };
        let filled = fill(&inputs, pass, shape);
        self.stats.fill_s += fill_start.elapsed().as_secs_f64();
        if let Err(e) = filled {
            inputs.busy.store(false, Ordering::Release);
            return Err(e);
        }
        kv.busy.store(true, Ordering::Release);
        let submitted = Instant::now();
        let model = loaded.model.clone();
        let proactor = proactor.clone();
        let done = Mutex::new(Some((on_done, proactor)));
        let (held_inputs, held_kv) = (Arc::clone(&inputs), Arc::clone(&kv));
        // SAFETY: public Core ML async prediction. The block keeps the model, the input
        // set and the key/value surfaces (which back the provider's arrays) alive until
        // it runs; the output is read only inside the array's accessor; the result
        // crosses to the caller as owned Rust data through the proactor.
        let started = autoreleasepool(|_| unsafe {
            let names = ["q", "k", "v", "mask"].map(NSString::from_str);
            let values = [
                &inputs.q.array,
                &kv.k.array,
                &kv.v.array,
                &inputs.mask.array,
            ]
            .map(|a| MLFeatureValue::featureValueWithMultiArray(a));
            let objects: Vec<&AnyObject> = values.iter().map(|v| &**v as &AnyObject).collect();
            let keys: Vec<&NSString> = names.iter().map(|n| &**n).collect();
            let dict = NSDictionary::from_slices(&keys, &objects);
            let provider = MLDictionaryFeatureProvider::initWithDictionary_error(
                MLDictionaryFeatureProvider::alloc(),
                &dict,
            )
            .map_err(|e| e.to_string())?;
            let held_model = model.clone();
            let held_provider = provider.clone();
            let callback = RcBlock::new(
                move |output: *mut ProtocolObject<dyn MLFeatureProvider>, err: *mut NSError| {
                    let _keep = (&held_model, &held_provider);
                    let result = autoreleasepool(|_| {
                        output.as_ref().map_or_else(
                            || {
                                Err(err.as_ref().map_or_else(
                                    || "prediction failed without an error".into(),
                                    ToString::to_string,
                                ))
                            },
                            |output| read_output(output, shape, t),
                        )
                    });
                    held_inputs.busy.store(false, Ordering::Release);
                    held_kv.busy.store(false, Ordering::Release);
                    let result = result.map(|out| AttentionOutput {
                        out,
                        latency: submitted.elapsed(),
                    });
                    let taken = done.lock().ok().and_then(|mut slot| slot.take());
                    if let Some((on_done, proactor)) = taken {
                        let _ = proactor.enqueue_work(move |_| on_done(result));
                    }
                },
            );
            model.predictionFromFeatures_completionHandler(
                ProtocolObject::from_ref(&*provider),
                &callback,
            );
            Ok::<(), String>(())
        });
        if let Err(e) = started {
            inputs.busy.store(false, Ordering::Release);
            kv.busy.store(false, Ordering::Release);
            return Err(e);
        }
        self.stats.submitted += 1;
        Ok(())
    }

    /// Writes `pass`'s key/value rows into `cache` without predicting, for a pass whose
    /// attention ran elsewhere (a decoded token on the GPU), so the cache keeps up.
    ///
    /// # Errors
    /// A pass that does not fit the Neural Engine, the cache in flight, or a value outside
    /// fp16; the cache is emptied then (the next pass rewrites it).
    pub fn sync(
        &mut self,
        pass: &AttentionPass<'_>,
        cache: &mut Option<KvCache>,
    ) -> Result<(), String> {
        let Some(shape) = Self::shape_of(pass) else {
            *cache = None;
            return Err("the pass does not fit the Neural Engine".into());
        };
        if cache.as_ref().is_some_and(KvCache::is_busy) {
            *cache = None;
            return Err("the layer's keys and values are still in flight".into());
        }
        let started = Instant::now();
        let written = self.write_kv(pass, shape, cache);
        self.stats.fill_s += started.elapsed().as_secs_f64();
        written.map(|_| ()).inspect_err(|_| *cache = None)
    }

    /// Brings `cache` up to `pass` (only its rows when the cache holds every earlier
    /// position, all of them otherwise) and returns its surfaces.
    fn write_kv(
        &mut self,
        pass: &AttentionPass<'_>,
        shape: GroupedAttentionShape,
        cache: &mut Option<KvCache>,
    ) -> Result<Arc<KvSurfaces>, String> {
        let width = shape.kv_heads * shape.dim;
        let end = pass.start + pass.t;
        let fits = cache
            .as_ref()
            .is_some_and(|c| c.keys == shape.keys && c.width == width && c.slots == pass.slots);
        if !fits {
            let surfaces = Arc::new(KvSurfaces {
                k: Surface::new(shape.keys, width)?,
                v: Surface::new(shape.keys, width)?,
                busy: AtomicBool::new(false),
            });
            // Rows past the positions are read (and multiplied by zero): keep them finite.
            surfaces.k.fill(|_, dst| dst.fill(0))?;
            surfaces.v.fill(|_, dst| dst.fill(0))?;
            *cache = Some(KvCache {
                surfaces,
                keys: shape.keys,
                width,
                synced: 0,
                slots: pass.slots,
            });
        }
        let c = cache.as_mut().expect("just set");
        let first = if c.synced == pass.start {
            pass.start
        } else {
            self.stats.kv_rewrites += 1;
            end.saturating_sub(pass.slots)
        };
        let rows: Vec<usize> = (first..end).map(|p| p % pass.slots).collect();
        let mut finite = true;
        for (surface, src) in [(&c.surfaces.k, pass.k), (&c.surfaces.v, pass.v)] {
            surface.fill_rows(rows.iter().copied(), |r, dst| {
                finite &= f32_to_f16(dst, &src[r * width..(r + 1) * width]);
            })?;
        }
        if !finite {
            c.synced = 0;
            return Err("a key or value is outside fp16 range".into());
        }
        self.stats.kv_rows += rows.len() as u64;
        c.synced = end;
        Ok(Arc::clone(&c.surfaces))
    }
}

/// Writes `pass`'s queries and mask into `inputs` in `shape`'s layout.
fn fill(
    inputs: &Inputs,
    pass: &AttentionPass<'_>,
    shape: GroupedAttentionShape,
) -> Result<(), String> {
    let (t, heads, dim) = (pass.t, pass.heads, pass.dim);
    let end = pass.start + t;
    let rows = pass.slots.min(end);
    let width = heads * dim;
    let mut finite = true;
    inputs.q.fill(|i, dst| {
        if i < t {
            finite &= f32_to_f16(dst, &pass.q[i * width..(i + 1) * width]);
        } else {
            dst.fill(0);
        }
    })?;
    if !finite {
        return Err("a query is outside fp16 range".into());
    }
    // The position each key row holds, or none.
    let last = end - 1;
    let position = |j: usize| -> Option<usize> {
        if j >= rows {
            return None;
        }
        if pass.slots >= end {
            return Some(j);
        }
        let back = (last % pass.slots + pass.slots - j) % pass.slots;
        last.checked_sub(back)
    };
    let positions: Vec<Option<usize>> = (0..shape.keys).map(position).collect();
    let (seen, hidden) = (
        f16::from_f32(0.0).to_bits(),
        f16::from_f32(HIDDEN).to_bits(),
    );
    inputs.mask.fill(|i, dst| {
        let p = pass.start + i.min(t - 1);
        let first = (p + 1).saturating_sub(pass.window);
        for (d, pos) in dst.iter_mut().zip(&positions) {
            *d = match pos {
                Some(s) if *s >= first && *s <= p => seen,
                _ => hidden,
            };
        }
    })?;
    Ok(())
}

/// The `o` output back in `[t][heads][dim]` order.
unsafe fn read_output(
    provider: &ProtocolObject<dyn MLFeatureProvider>,
    shape: GroupedAttentionShape,
    t: usize,
) -> Result<Vec<f32>, String> {
    let array = provider
        .featureValueForName(&NSString::from_str("o"))
        .and_then(|v| v.multiArrayValue())
        .ok_or("prediction has no o array")?;
    let strides: Vec<usize> = array.strides().iter().map(|n| n.as_usize()).collect();
    let dims: Vec<usize> = array.shape().iter().map(|n| n.as_usize()).collect();
    let width = shape.heads() * shape.dim;
    if dims != [shape.queries, width] || strides.len() != 2 {
        return Err(format!("unexpected output shape {dims:?}"));
    }
    let dtype = array.dataType();
    let got = std::cell::RefCell::new(Err("output accessor did not run".to_string()));
    let read = RcBlock::new(|ptr: NonNull<std::ffi::c_void>, size: isize| {
        let bytes = if dtype == MLMultiArrayDataType::Float16 {
            2
        } else {
            4
        };
        let need = ((t - 1) * strides[0] + (width - 1) * strides[1] + 1) * bytes;
        if size < 0 || (size as usize) < need {
            *got.borrow_mut() = Err("output buffer smaller than its shape".into());
            return;
        }
        let mut out = vec![0.0_f32; t * width];
        for (i, dst) in out.chunks_exact_mut(width).enumerate() {
            for (d, o) in dst.iter_mut().enumerate() {
                let k = i * strides[0] + d * strides[1];
                *o = if bytes == 2 {
                    f16::from_bits(ptr.as_ptr().cast::<u16>().add(k).read()).to_f32()
                } else {
                    ptr.as_ptr().cast::<f32>().add(k).read()
                };
            }
        }
        *got.borrow_mut() = Ok(out);
    });
    array.getBytesWithHandler(&read);
    drop(read);
    let out = got.into_inner()?;
    if out.iter().any(|v| !v.is_finite()) {
        return Err("attention result is not finite".into());
    }
    Ok(out)
}

impl Drop for AttentionEngine {
    fn drop(&mut self) {
        for loaded in self.models.values() {
            if let Some(path) = loaded.compiled.path() {
                let _ = std::fs::remove_dir_all(path.to_string());
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
