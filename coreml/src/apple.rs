//! Audited Objective-C boundary. Public macOS 15+ Core ML APIs only.
//! Objects are retained across native callbacks; only owned Rust tensors/errors
//! cross the caller completion boundary. One prediction in flight per model.
use crate::model::DenseShape;
use block2::RcBlock;
use loadngo_inference::compute::{
    ComputePolicy, DeviceKind, Placement, PredictionCallback, PreparedCompute, Tensor,
};
use objc2::{
    rc::{autoreleasepool, Retained},
    runtime::{AnyObject, ProtocolObject},
    AnyThread, ClassType,
};
use objc2_core_ml::*;
use objc2_foundation::{
    NSArray, NSDictionary, NSError, NSNumber, NSObjectProtocol, NSOperatingSystemVersion,
    NSProcessInfo, NSString, NSURL,
};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

fn error(e: Retained<NSError>) -> String {
    e.to_string()
}

fn supported_os() -> bool {
    NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(NSOperatingSystemVersion {
        majorVersion: 15,
        minorVersion: 0,
        patchVersion: 0,
    })
}

unsafe fn device_kind(device: &ProtocolObject<dyn MLComputeDeviceProtocol>) -> DeviceKind {
    if device.isKindOfClass(MLNeuralEngineComputeDevice::class()) {
        DeviceKind::Npu
    } else if device.isKindOfClass(MLCPUComputeDevice::class()) {
        DeviceKind::Cpu
    } else if device.isKindOfClass(MLGPUComputeDevice::class()) {
        DeviceKind::Gpu
    } else {
        DeviceKind::Unknown
    }
}

pub fn available_devices() -> Vec<DeviceKind> {
    if !supported_os() {
        return Vec::new();
    }
    // SAFETY: read-only public class API, objects retained during iteration.
    unsafe {
        MLModel::availableComputeDevices()
            .iter()
            .map(|d| device_kind(&d))
            .collect()
    }
}

pub struct CompiledModel {
    url: Retained<NSURL>,
}
impl CompiledModel {
    pub fn compile(path: &Path) -> Result<Self, String> {
        if !supported_os() {
            return Err("Core ML probe requires macOS 15+".into());
        }
        let path = path.canonicalize().map_err(|e| e.to_string())?;
        let path = path.to_str().ok_or("model path is not UTF-8")?;
        let url = NSURL::fileURLWithPath(&NSString::from_str(path));
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let callback = RcBlock::new(move |url: *mut NSURL, err: *mut NSError| {
            // SAFETY: retain the callback-scoped URL before handing it off.
            let result = unsafe { Retained::retain(url) }.ok_or_else(|| unsafe {
                err.as_ref()
                    .map_or_else(|| "compilation failed".into(), ToString::to_string)
            });
            let _ = tx.send(result);
        });
        // SAFETY: Core ML copies the completion block and owns its callback args.
        unsafe {
            MLModel::compileModelAtURL_completionHandler(&url, &callback);
        }
        let url = rx
            .recv_timeout(Duration::from_secs(60))
            .map_err(|e| e.to_string())??;
        Ok(Self { url })
    }
    pub fn load(&self, policy: ComputePolicy, shape: DenseShape) -> Result<CoreMlModel, String> {
        // SAFETY: configuration is private to this load; the model owns its graph.
        unsafe {
            let config = MLModelConfiguration::new();
            config.setComputeUnits(match policy {
                ComputePolicy::CpuOnly => MLComputeUnits::CPUOnly,
                ComputePolicy::CpuAndNpu => MLComputeUnits::CPUAndNeuralEngine,
            });
            let model = MLModel::modelWithContentsOfURL_configuration_error(&self.url, &config)
                .map_err(error)?;
            let placements = plan(&self.url, &config)?;
            Ok(CoreMlModel {
                model,
                shape,
                placements,
                busy: Arc::new(AtomicBool::new(false)),
            })
        }
    }
}

pub(crate) unsafe fn plan(
    url: &NSURL,
    config: &MLModelConfiguration,
) -> Result<Vec<Placement>, String> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let callback = RcBlock::new(move |plan: *mut MLComputePlan, err: *mut NSError| {
        // SAFETY: Core ML owns these nullable pointers for the callback duration.
        let result = unsafe {
            (|| {
                let Some(plan) = plan.as_ref() else {
                    return Err(err
                        .as_ref()
                        .map_or_else(|| "compute plan unavailable".into(), ToString::to_string));
                };
                let network = plan
                    .modelStructure()
                    .neuralNetwork()
                    .ok_or("not a neural-network model")?;
                let mut result = Vec::new();
                for layer in network.layers().iter() {
                    if let Some(usage) = plan.computeDeviceUsageForNeuralNetworkLayer(&layer) {
                        result.push(Placement {
                            operation: layer.name().to_string(),
                            preferred: device_kind(&usage.preferredComputeDevice()),
                            supported: usage
                                .supportedComputeDevices()
                                .iter()
                                .map(|d| device_kind(&d))
                                .collect(),
                        });
                    }
                }
                Ok(result)
            })()
        };
        let _ = tx.send(result);
    });
    MLComputePlan::loadContentsOfURL_configuration_completionHandler(url, config, &callback);
    rx.recv_timeout(Duration::from_secs(60))
        .map_err(|e| e.to_string())?
}

pub struct CoreMlModel {
    model: Retained<MLModel>,
    shape: DenseShape,
    pub placements: Vec<Placement>,
    busy: Arc<AtomicBool>,
}

unsafe fn input_features(input: &Tensor) -> Result<Retained<MLDictionaryFeatureProvider>, String> {
    let dims = NSArray::from_retained_slice(
        &input
            .shape()
            .iter()
            .map(|&n| NSNumber::new_usize(n))
            .collect::<Vec<_>>(),
    );
    let array = MLMultiArray::initWithShape_dataType_error(
        MLMultiArray::alloc(),
        &dims,
        MLMultiArrayDataType::Float32,
    )
    .map_err(error)?;
    // Validate the callback's actual strides and size; its pointer is valid only
    // inside the block. Copy initializes all elements before prediction.
    if array.count() as usize != input.values().len() {
        return Err("input allocation size mismatch".into());
    }
    let copied = std::cell::Cell::new(false);
    let fill = RcBlock::new(
        |ptr: std::ptr::NonNull<std::ffi::c_void>,
         size: isize,
         strides: std::ptr::NonNull<NSArray<NSNumber>>| {
            let strides: Vec<usize> = strides.as_ref().iter().map(|n| n.as_usize()).collect();
            let mut expected = 1;
            if size < 0
                || (size as usize) < std::mem::size_of_val(input.values())
                || strides.len() != input.shape().len()
            {
                return;
            }
            for (&dim, &stride) in input.shape().iter().zip(&strides).rev() {
                if dim > 1 && stride != expected {
                    return;
                }
                expected *= dim;
            }
            std::ptr::copy_nonoverlapping(
                input.values().as_ptr(),
                ptr.as_ptr().cast::<f32>(),
                input.values().len(),
            );
            copied.set(true);
        },
    );
    array.getMutableBytesWithHandler(&fill);
    if !copied.get() {
        return Err("unsupported input buffer layout".into());
    }
    let feature = MLFeatureValue::featureValueWithMultiArray(&array);
    let key = NSString::from_str("x");
    let object: &AnyObject = &feature;
    let dict = NSDictionary::from_slices(&[&*key], &[object]);
    MLDictionaryFeatureProvider::initWithDictionary_error(
        MLDictionaryFeatureProvider::alloc(),
        &dict,
    )
    .map_err(error)
}

unsafe fn output_tensor(
    provider: &ProtocolObject<dyn MLFeatureProvider>,
    shape: DenseShape,
) -> Result<Tensor, String> {
    let feature = provider
        .featureValueForName(&NSString::from_str("y"))
        .ok_or("missing y")?;
    let array = feature.multiArrayValue().ok_or("output is not an array")?;
    let expected = shape.output_shape();
    let actual: Vec<usize> = array
        .shape()
        .iter()
        .map(|n| n.unsignedIntegerValue())
        .collect();
    if actual != expected {
        return Err(format!("output shape {actual:?} != {expected:?}"));
    }
    if array.dataType() != MLMultiArrayDataType::Float32 {
        return Err("expected float32 output".into());
    }
    // Copy through the supported buffer API, not one Objective-C NSNumber call
    // per scalar. Respect runtime strides and validate the entire byte span.
    let result = std::cell::RefCell::new(Err("output buffer callback did not run".to_string()));
    let copy = RcBlock::new(|ptr: std::ptr::NonNull<std::ffi::c_void>, size: isize| {
        let strides: Vec<usize> = array.strides().iter().map(|n| n.as_usize()).collect();
        let values = (|| {
            if size < 0 || strides.len() != 4 {
                return Err("invalid output buffer layout".into());
            }
            let last = (shape.outputs - 1)
                .checked_mul(strides[1])
                .and_then(|a| {
                    (shape.positions - 1)
                        .checked_mul(strides[3])
                        .and_then(|b| a.checked_add(b))
                })
                .and_then(|n| n.checked_add(1))
                .and_then(|n| n.checked_mul(4))
                .ok_or("output stride overflow")?;
            if last > size as usize {
                return Err("output stride exceeds buffer".into());
            }
            let mut values = Vec::with_capacity(shape.outputs * shape.positions);
            let data = ptr.as_ptr().cast::<f32>();
            for output in 0..shape.outputs {
                for position in 0..shape.positions {
                    values.push(
                        data.add(output * strides[1] + position * strides[3])
                            .read_unaligned(),
                    );
                }
            }
            Ok(values)
        })();
        *result.borrow_mut() = values;
    });
    array.getBytesWithHandler(&copy);
    drop(copy);
    let values = result.into_inner()?;
    Tensor::new(expected, values)
}

impl PreparedCompute for CoreMlModel {
    fn is_busy(&self) -> bool {
        self.busy.load(Ordering::Acquire)
    }
    fn submit(&mut self, input: Tensor, done: PredictionCallback) -> Result<(), String> {
        if input.shape() != self.shape.input_shape() {
            return Err("input shape mismatch".into());
        }
        if self.busy.swap(true, Ordering::AcqRel) {
            return Err("model is busy (one prediction in flight)".into());
        }
        // SAFETY: MLModel's documented async prediction retains/copies the block.
        // We additionally retain model/provider in that block through completion.
        // No caller tensor is borrowed after submission, and the completion sees
        // Rust-owned data only. ObjC objects never escape to caller worker threads.
        unsafe {
            let provider = match input_features(&input) {
                Ok(p) => p,
                Err(e) => {
                    self.busy.store(false, Ordering::Release);
                    return Err(e);
                }
            };
            let held_provider = provider.clone();
            let held_model = self.model.clone();
            let busy = self.busy.clone();
            let shape = self.shape;
            let done = Mutex::new(Some(done));
            let callback = RcBlock::new(
                move |output: *mut ProtocolObject<dyn MLFeatureProvider>, err: *mut NSError| {
                    let _keep_alive = (&held_model, &held_provider);
                    let result = autoreleasepool(|_| {
                        if let Some(output) = output.as_ref() {
                            output_tensor(output, shape)
                        } else {
                            Err(err.as_ref().map_or_else(
                                || "prediction failed without error".into(),
                                ToString::to_string,
                            ))
                        }
                    });
                    busy.store(false, Ordering::Release);
                    if let Some(done) = done.lock().expect("completion lock poisoned").take() {
                        done(result);
                    }
                },
            );
            self.model.predictionFromFeatures_completionHandler(
                ProtocolObject::from_ref(&*provider),
                &callback,
            );
        }
        Ok(())
    }
}
