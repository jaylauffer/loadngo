//! macOS sources: Mach host statistics for CPU and memory, `statfs` for disk, the I/O
//! Registry for GPU utilisation and the Neural Engine's identity, and IOReport's
//! "Energy Model" channels for CPU, GPU, Neural Engine and DRAM power.
//!
//! Everything here works without root. IOReport (`/usr/lib/libIOReport.dylib`) is a
//! private Apple interface: it is what sudo-free monitors use, it has been stable across
//! Apple silicon releases, and if it is missing or changes, power reads as `None` and
//! nothing else is affected. macOS publishes no Neural Engine utilisation, so its power
//! draw is the activity signal. There is no public CPU clock, fan speed or temperature
//! on Apple silicon; thermal pressure comes from `loadngo-thermal`.

use super::{CpuTimes, DiskUsage, MemoryUsage, PowerDraw, SystemSample, SystemSampler};
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::Path;
use std::ptr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type CFTypeRef = *const c_void;
type IoObject = u32;

const UTF8: u32 = 0x0800_0100; // kCFStringEncodingUTF8
const SINT64: isize = 4; // kCFNumberSInt64Type
const MAIN_PORT: u32 = 0; // kIOMainPortDefault

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(alloc: CFTypeRef, s: *const c_char, encoding: u32) -> CFTypeRef;
    fn CFStringGetCString(s: CFTypeRef, buf: *mut c_char, len: isize, encoding: u32) -> bool;
    fn CFRelease(cf: CFTypeRef);
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFArrayGetTypeID() -> usize;
    fn CFDictionaryGetValue(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    fn CFDictionaryCreateMutableCopy(
        alloc: CFTypeRef,
        capacity: isize,
        dict: CFTypeRef,
    ) -> CFTypeRef;
    fn CFNumberGetValue(number: CFTypeRef, kind: isize, out: *mut c_void) -> bool;
    fn CFArrayGetCount(array: CFTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CFTypeRef, index: isize) -> CFTypeRef;
    fn CFArrayCreateMutable(
        alloc: CFTypeRef,
        capacity: isize,
        callbacks: *const c_void,
    ) -> CFTypeRef;
    fn CFArrayAppendValue(array: CFTypeRef, value: CFTypeRef);
    fn CFDictionarySetValue(dict: CFTypeRef, key: CFTypeRef, value: CFTypeRef);
    static kCFTypeArrayCallBacks: c_void;
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOServiceMatching(name: *const c_char) -> CFTypeRef;
    fn IOServiceGetMatchingService(main_port: u32, matching: CFTypeRef) -> IoObject;
    fn IORegistryEntryCreateCFProperty(
        entry: IoObject,
        key: CFTypeRef,
        alloc: CFTypeRef,
        options: u32,
    ) -> CFTypeRef;
    fn IOObjectRelease(object: IoObject) -> i32;
}

#[link(name = "IOReport")]
extern "C" {
    fn IOReportCopyChannelsInGroup(
        group: CFTypeRef,
        subgroup: CFTypeRef,
        a: u64,
        b: u64,
        c: u64,
    ) -> CFTypeRef;
    fn IOReportCreateSubscription(
        allocator: CFTypeRef,
        channels: CFTypeRef,
        subscribed: *mut CFTypeRef,
        id: u64,
        options: CFTypeRef,
    ) -> CFTypeRef;
    fn IOReportCreateSamples(
        subscription: CFTypeRef,
        channels: CFTypeRef,
        options: CFTypeRef,
    ) -> CFTypeRef;
    fn IOReportCreateSamplesDelta(
        earlier: CFTypeRef,
        later: CFTypeRef,
        options: CFTypeRef,
    ) -> CFTypeRef;
    fn IOReportChannelGetChannelName(channel: CFTypeRef) -> CFTypeRef;
    fn IOReportChannelGetUnitLabel(channel: CFTypeRef) -> CFTypeRef;
    fn IOReportSimpleGetIntegerValue(channel: CFTypeRef, index: i32) -> i64;
}

/// A Core Foundation object this module owns (released on drop); null when absent.
#[derive(Debug)]
struct Owned(CFTypeRef);

impl Owned {
    fn string(text: &str) -> Self {
        let text = CString::new(text).expect("constant keys have no NUL");
        // SAFETY: a NUL-terminated UTF-8 string; the result is owned (Create rule).
        Self(unsafe { CFStringCreateWithCString(ptr::null(), text.as_ptr(), UTF8) })
    }

    fn is_null(&self) -> bool {
        self.0.is_null()
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: owned by this value (Create/Copy rule) and released once.
            unsafe { CFRelease(self.0) };
        }
    }
}

fn is_type(cf: CFTypeRef, type_id: usize) -> bool {
    // SAFETY: `cf` is a live CF object or null (checked).
    !cf.is_null() && unsafe { CFGetTypeID(cf) } == type_id
}

/// `dict[key]` as an `i64`, when it is a number.
fn dict_i64(dict: CFTypeRef, key: &Owned) -> Option<i64> {
    // SAFETY: `dict` is a live dictionary (callers check), `key` a live string. The value
    // is borrowed (Get rule) and read into a local.
    unsafe {
        let value = CFDictionaryGetValue(dict, key.0);
        if !is_type(value, CFNumberGetTypeID()) {
            return None;
        }
        let mut out = 0i64;
        CFNumberGetValue(value, SINT64, (&raw mut out).cast()).then_some(out)
    }
}

/// Copies a CF string into `buf` and returns it; no allocation.
fn cf_str(string: CFTypeRef, buf: &mut [u8; 64]) -> Option<&str> {
    if !is_type(string, unsafe { CFStringGetTypeID() }) {
        return None;
    }
    // SAFETY: `string` is a live CFString; `buf` is 64 writable bytes.
    let ok = unsafe { CFStringGetCString(string, buf.as_mut_ptr().cast(), 64, UTF8) };
    if !ok {
        return None;
    }
    CStr::from_bytes_until_nul(buf).ok()?.to_str().ok()
}

/// Reads one I/O Registry property of the first service of `class`.
struct Service(IoObject);

impl Service {
    fn find(class: &CStr) -> Option<Self> {
        // SAFETY: `IOServiceMatching` returns a dictionary that
        // `IOServiceGetMatchingService` consumes; the service is owned and released on drop.
        let service =
            unsafe { IOServiceGetMatchingService(MAIN_PORT, IOServiceMatching(class.as_ptr())) };
        (service != 0).then_some(Self(service))
    }

    fn property(&self, key: &Owned) -> Owned {
        // SAFETY: a live service and key; the property is returned owned (Create rule).
        Owned(unsafe { IORegistryEntryCreateCFProperty(self.0, key.0, ptr::null(), 0) })
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        // SAFETY: owned by this value and released once.
        unsafe { IOObjectRelease(self.0) };
    }
}

impl std::fmt::Debug for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Service({})", self.0)
    }
}

/// Which power slot an "Energy Model" channel feeds: CPU, GPU, Neural Engine, DRAM.
fn energy_slot(name: &str) -> Option<usize> {
    match name {
        "CPU Energy" => Some(0),
        "GPU Energy" => Some(1),
        n if n.starts_with("ANE") => Some(2),
        "DRAM" => Some(3),
        _ => None,
    }
}

/// IOReport's energy counters, sampled against the previous reading.
#[derive(Debug)]
struct Energy {
    subscription: Owned,
    channels: Owned,
    previous: Option<(Owned, Instant)>,
    key_channels: Owned,
}

impl Energy {
    fn subscribe() -> Option<Self> {
        let group = Owned::string("Energy Model");
        // SAFETY: IOReport returns an owned channel dictionary (Copy rule) or null; the
        // mutable copy and the subscription are owned too, and `subscribed` is an out
        // pointer IOReport fills with an owned dictionary.
        unsafe {
            let all = Owned(IOReportCopyChannelsInGroup(group.0, ptr::null(), 0, 0, 0));
            if all.is_null() {
                return None;
            }
            let wanted = Owned(CFDictionaryCreateMutableCopy(ptr::null(), 0, all.0));
            // Only the channels shown: the group has a few hundred (per core and per
            // cluster), and each one costs kernel time on every sample.
            let key = Owned::string("IOReportChannels");
            let list = CFDictionaryGetValue(all.0, key.0);
            if is_type(list, CFArrayGetTypeID()) {
                let kept = Owned(CFArrayCreateMutable(
                    ptr::null(),
                    0,
                    (&raw const kCFTypeArrayCallBacks).cast(),
                ));
                let mut buf = [0u8; 64];
                for i in 0..CFArrayGetCount(list) {
                    let channel = CFArrayGetValueAtIndex(list, i);
                    let name = cf_str(IOReportChannelGetChannelName(channel), &mut buf);
                    if name.and_then(energy_slot).is_some() {
                        CFArrayAppendValue(kept.0, channel);
                    }
                }
                if CFArrayGetCount(kept.0) > 0 {
                    CFDictionarySetValue(wanted.0, key.0, kept.0);
                }
            }
            let mut subscribed: CFTypeRef = ptr::null();
            let subscription = Owned(IOReportCreateSubscription(
                ptr::null(),
                wanted.0,
                &raw mut subscribed,
                0,
                ptr::null(),
            ));
            let channels = Owned(subscribed);
            (!subscription.is_null() && !channels.is_null()).then(|| Self {
                subscription,
                channels,
                previous: None,
                key_channels: Owned::string("IOReportChannels"),
            })
        }
    }

    /// Mean power since the previous call; `None` on the first.
    fn sample(&mut self) -> Option<PowerDraw> {
        // SAFETY: a live subscription and channel set; the samples are owned.
        let current = Owned(unsafe {
            IOReportCreateSamples(self.subscription.0, self.channels.0, ptr::null())
        });
        let now = Instant::now();
        if current.is_null() {
            return None;
        }
        let (earlier, then) = self.previous.replace((current, now))?;
        let seconds = now.duration_since(then).as_secs_f64();
        let later = &self.previous.as_ref()?.0;
        // SAFETY: two live sample dictionaries; the delta is owned.
        let delta = Owned(unsafe { IOReportCreateSamplesDelta(earlier.0, later.0, ptr::null()) });
        if delta.is_null() || seconds <= 0.0 {
            return None;
        }
        let mut joules = [0.0f64; 4]; // cpu, gpu, npu, dram
        let mut seen = [false; 4];
        // SAFETY: `delta` is a live dictionary; the channel array and its entries are
        // borrowed from it (Get rule) and only read while it lives.
        unsafe {
            let list = CFDictionaryGetValue(delta.0, self.key_channels.0);
            if !is_type(list, CFArrayGetTypeID()) {
                return None;
            }
            let (mut name_buf, mut unit_buf) = ([0u8; 64], [0u8; 64]);
            for i in 0..CFArrayGetCount(list) {
                let channel = CFArrayGetValueAtIndex(list, i);
                if !is_type(channel, CFDictionaryGetTypeID()) {
                    continue;
                }
                let Some(name) = cf_str(IOReportChannelGetChannelName(channel), &mut name_buf)
                else {
                    continue;
                };
                let Some(slot) = energy_slot(name) else {
                    continue;
                };
                let scale = match cf_str(IOReportChannelGetUnitLabel(channel), &mut unit_buf) {
                    Some("mJ") => 1e-3,
                    Some("uJ" | "µJ") => 1e-6,
                    Some("nJ") => 1e-9,
                    _ => continue,
                };
                joules[slot] += IOReportSimpleGetIntegerValue(channel, 0) as f64 * scale;
                seen[slot] = true;
            }
        }
        let watts = |i: usize| seen[i].then(|| (joules[i] / seconds) as f32);
        Some(PowerDraw {
            cpu_w: watts(0),
            gpu_w: watts(1),
            npu_w: watts(2),
            dram_w: watts(3),
        })
    }
}

#[derive(Debug)]
pub(super) struct Sources {
    disk: Option<CString>,
    memory_total: Option<u64>,
    boot: Option<SystemTime>,
    gpu: Option<Service>,
    key_performance: Owned,
    key_utilization: Owned,
    energy: Option<Energy>,
    pub(super) npu_name: Option<String>,
}

impl Sources {
    pub(super) fn discover(disk_path: &Path) -> Self {
        use std::os::unix::ffi::OsStrExt;
        let mut npu_name = None;
        if let Some(ane) = Service::find(c"H11ANEIn") {
            let props = ane.property(&Owned::string("DeviceProperties"));
            if is_type(props.0, unsafe { CFDictionaryGetTypeID() }) {
                let cores = dict_i64(props.0, &Owned::string("ANEDevicePropertyNumANECores"));
                npu_name = Some(match cores {
                    Some(cores) => format!("{cores}-core Neural Engine"),
                    None => "Neural Engine".to_string(),
                });
            }
        }
        Self {
            disk: CString::new(disk_path.as_os_str().as_bytes()).ok(),
            memory_total: sysctl_u64(c"hw.memsize"),
            boot: boot_time(),
            gpu: Service::find(c"IOAccelerator"),
            key_performance: Owned::string("PerformanceStatistics"),
            key_utilization: Owned::string("Device Utilization %"),
            energy: Energy::subscribe(),
            npu_name,
        }
    }
}

fn sysctl_u64(name: &CStr) -> Option<u64> {
    let mut value = 0u64;
    let mut len = std::mem::size_of::<u64>();
    // SAFETY: `value` is `len` writable bytes; sysctl writes at most that many.
    let ok = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&raw mut value).cast(),
            &raw mut len,
            ptr::null_mut(),
            0,
        )
    } == 0;
    (ok && len == std::mem::size_of::<u64>()).then_some(value)
}

fn boot_time() -> Option<SystemTime> {
    let mut boot = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let mut len = std::mem::size_of::<libc::timeval>();
    // SAFETY: `boot` is `len` writable bytes.
    let ok = unsafe {
        libc::sysctlbyname(
            c"kern.boottime".as_ptr(),
            (&raw mut boot).cast(),
            &raw mut len,
            ptr::null_mut(),
            0,
        )
    } == 0;
    let secs = u64::try_from(boot.tv_sec).ok()?;
    ok.then(|| UNIX_EPOCH + Duration::from_secs(secs))
}

impl SystemSampler {
    pub(super) fn sample_macos(&mut self, out: &mut SystemSample) {
        let parsed = read_cpu_times(&mut self.current);
        self.update_cpu(parsed, &mut out.cpu);
        let mac = &mut self.mac;
        out.memory = mac.memory_total.and_then(memory_usage);
        out.disk = mac.disk.as_deref().and_then(disk_usage);
        out.load_average = load_average();
        out.uptime = mac
            .boot
            .and_then(|boot| SystemTime::now().duration_since(boot).ok());
        out.gpu = mac.gpu.as_ref().and_then(|gpu| {
            let stats = gpu.property(&mac.key_performance);
            if !is_type(stats.0, unsafe { CFDictionaryGetTypeID() }) {
                return None;
            }
            let percent = dict_i64(stats.0, &mac.key_utilization)?;
            Some(super::GpuUsage {
                busy: (percent.clamp(0, 100) as f32) / 100.0,
            })
        });
        out.power = mac.energy.as_mut().and_then(Energy::sample);
        out.clock = None;
        out.fan = None;
        out.undervoltage = None;
    }
}

/// Per-CPU tick counters into `out`: the total first, then each CPU. macOS has no I/O
/// wait state, so `iowait` stays zero.
fn read_cpu_times(out: &mut Vec<CpuTimes>) -> bool {
    out.clear();
    let mut cpus: libc::natural_t = 0;
    let mut info: libc::processor_info_array_t = ptr::null_mut();
    let mut count: libc::mach_msg_type_number_t = 0;
    // SAFETY: out pointers to locals; on success the kernel allocates `info` in this
    // task, `count` integers long, which is deallocated below.
    unsafe {
        #[allow(deprecated)] // libc points to the mach2 crate; the call is the same
        let host = libc::mach_host_self();
        if libc::host_processor_info(
            host,
            libc::PROCESSOR_CPU_LOAD_INFO,
            &raw mut cpus,
            &raw mut info,
            &raw mut count,
        ) != libc::KERN_SUCCESS
        {
            return false;
        }
        let ticks = std::slice::from_raw_parts(info.cast::<u32>(), count as usize);
        let mut total = CpuTimes::default();
        out.push(total);
        // CPU_STATE_MAX (4) counters per CPU: user, system, idle, nice.
        const _: () = assert!(libc::CPU_STATE_MAX == 4);
        for cpu in ticks.as_chunks::<4>().0.iter().take(cpus as usize) {
            let state = |s: libc::c_int| u64::from(cpu[s as usize]);
            let busy = state(libc::CPU_STATE_USER)
                + state(libc::CPU_STATE_SYSTEM)
                + state(libc::CPU_STATE_NICE);
            let times = CpuTimes {
                busy,
                iowait: 0,
                total: busy + state(libc::CPU_STATE_IDLE),
            };
            total.busy += times.busy;
            total.total += times.total;
            out.push(times);
        }
        out[0] = total;
        #[allow(deprecated)]
        let task = libc::mach_task_self();
        libc::vm_deallocate(
            task,
            info as libc::vm_address_t,
            count as usize * std::mem::size_of::<libc::integer_t>(),
        );
    }
    out.len() > 1
}

/// "Memory used" as Activity Monitor counts it: app memory (anonymous pages that are
/// not purgeable), wired and compressed.
fn memory_usage(total: u64) -> Option<MemoryUsage> {
    let mut stats = std::mem::MaybeUninit::<libc::vm_statistics64>::zeroed();
    let mut count = libc::HOST_VM_INFO64_COUNT;
    // SAFETY: `stats` has room for HOST_VM_INFO64_COUNT integers; the kernel fills it.
    let stats = unsafe {
        #[allow(deprecated)]
        let host = libc::mach_host_self();
        if libc::host_statistics64(
            host,
            libc::HOST_VM_INFO64,
            stats.as_mut_ptr().cast(),
            &raw mut count,
        ) != libc::KERN_SUCCESS
        {
            return None;
        }
        stats.assume_init()
    };
    // SAFETY: a constant the kernel sets at start-up.
    let page = unsafe { libc::vm_page_size } as u64;
    let pages = u64::from(
        stats
            .internal_page_count
            .saturating_sub(stats.purgeable_count),
    ) + u64::from(stats.wire_count)
        + u64::from(stats.compressor_page_count);
    Some(MemoryUsage {
        total_bytes: total,
        available_bytes: total.saturating_sub(pages * page),
    })
}

fn disk_usage(path: &CStr) -> Option<DiskUsage> {
    let mut stats = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `path` is NUL-terminated and `stats` a valid out-pointer that statfs fully
    // initialises when it returns 0.
    let stats = unsafe {
        if libc::statfs(path.as_ptr(), stats.as_mut_ptr()) != 0 {
            return None;
        }
        stats.assume_init()
    };
    let block = u64::from(stats.f_bsize);
    Some(DiskUsage {
        total_bytes: stats.f_blocks * block,
        available_bytes: stats.f_bavail * block,
    })
}

fn load_average() -> Option<[f32; 3]> {
    let mut loads = [0f64; 3];
    // SAFETY: room for the three values requested.
    let n = unsafe { libc::getloadavg(loads.as_mut_ptr(), 3) };
    (n == 3).then(|| loads.map(|l| l as f32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_sample_reads_the_running_system() {
        let mut sampler = SystemSampler::new(Path::new("/"));
        let mut sample = SystemSample::default();
        sampler.sample(&mut sample);
        assert!(sample.cpu.is_none(), "usage needs two readings");
        std::thread::sleep(Duration::from_millis(200));
        sampler.sample(&mut sample);
        let cpu = sample
            .cpu
            .as_ref()
            .expect("CPU usage on the second reading");
        assert!(!cpu.cores.is_empty());
        assert!((0.0..=1.0).contains(&cpu.all.busy));
        let memory = sample.memory.expect("memory");
        assert!(memory.used_bytes() > 0 && memory.used_bytes() < memory.total_bytes);
        let disk = sample.disk.expect("disk");
        assert!(disk.total_bytes > 0);
        assert!(sample.load_average.is_some() && sample.uptime.is_some());
        if let Some(gpu) = sample.gpu {
            assert!((0.0..=1.0).contains(&gpu.busy));
        }
        if let Some(power) = sample.power {
            for watts in [power.cpu_w, power.gpu_w, power.npu_w, power.dram_w]
                .into_iter()
                .flatten()
            {
                assert!((0.0..500.0).contains(&watts), "{watts} W");
            }
        }
    }
}
