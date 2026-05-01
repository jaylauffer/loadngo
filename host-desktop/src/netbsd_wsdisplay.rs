use std::{ffi::CString, io, mem, os::fd::RawFd, ptr, time::Duration};

use loadngo_proactor::{CompletionKind, KqueuePort, Proactor};

const WSDISPLAY_GROUP: u8 = b'W';
const IOC_OUT: u64 = 0x4000_0000;
const IOC_IN: u64 = 0x8000_0000;
const IOC_INOUT: u64 = IOC_IN | IOC_OUT;
const IOCPARM_MASK: usize = 0x1fff;

const WSDISPLAYIO_MODE_EMUL: u32 = 0;
const WSDISPLAYIO_MODE_MAPPED: u32 = 1;
const WSDISPLAYIO_MODE_DUMBFB: u32 = 2;
const WSFB_RGB: u32 = 0;

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct WsdisplayFbInfo {
    height: libc::c_uint,
    width: libc::c_uint,
    depth: libc::c_uint,
    cmsize: libc::c_uint,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct WsdisplayioFbInfo {
    fb_size: u64,
    fb_offset: u64,
    width: u32,
    height: u32,
    stride: u32,
    bits_per_pixel: u32,
    pixel_type: u32,
    red_offset: u32,
    red_size: u32,
    green_offset: u32,
    green_size: u32,
    blue_offset: u32,
    blue_size: u32,
    alpha_offset: u32,
    alpha_size: u32,
    flags: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct WsdisplayInfo {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub bits_per_pixel: u32,
    pub fb_size: usize,
    pub fb_offset: usize,
    pub pixel_type: u32,
    pub red_offset: u32,
    pub red_size: u32,
    pub green_offset: u32,
    pub green_size: u32,
    pub blue_offset: u32,
    pub blue_size: u32,
    pub alpha_offset: u32,
    pub alpha_size: u32,
}

impl WsdisplayInfo {
    pub fn bytes_per_pixel(self) -> usize {
        ((self.bits_per_pixel.max(8) + 7) / 8) as usize
    }

    pub fn visible_len(self) -> usize {
        self.stride as usize * self.height as usize
    }

    pub fn pack_rgb(self, red: u8, green: u8, blue: u8) -> u32 {
        pack_rgb(self, red, green, blue)
    }
}

pub fn probe(device_path: &str) -> Result<WsdisplayInfo, String> {
    let fd = open_wsdisplay(device_path)?;
    let result = probe_fd(fd);
    close_fd(fd);
    result
}

pub fn paint_test_pattern(device_path: &str, hold: Duration) -> Result<WsdisplayInfo, String> {
    let mut surface = WsdisplaySurface::open(device_path)?;
    let info = surface.info();
    paint_pattern(surface.framebuffer_mut(), info);
    hold_with_proactor(hold)?;
    Ok(info)
}

pub struct WsdisplaySurface {
    fd: RawFd,
    mapping: *mut libc::c_void,
    map_len: usize,
    info: WsdisplayInfo,
}

impl WsdisplaySurface {
    pub fn open(device_path: &str) -> Result<Self, String> {
        let fd = open_wsdisplay(device_path)?;
        let result = (|| {
            let info = probe_fd(fd)?;
            set_mode(fd, WSDISPLAYIO_MODE_DUMBFB)
                .or_else(|_| set_mode(fd, WSDISPLAYIO_MODE_MAPPED))?;

            let map_len = info.fb_size.max(info.fb_offset + info.visible_len());
            if map_len == 0 {
                return Err("wsdisplay reported an empty framebuffer".to_string());
            }

            let mapping = unsafe {
                libc::mmap(
                    ptr::null_mut(),
                    map_len,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    fd,
                    0,
                )
            };
            if mapping == libc::MAP_FAILED {
                return Err(format!(
                    "mmap wsdisplay framebuffer failed: {}",
                    last_error()
                ));
            }

            Ok(Self {
                fd,
                mapping,
                map_len,
                info,
            })
        })();

        if result.is_err() {
            let _ = set_mode(fd, WSDISPLAYIO_MODE_EMUL);
            close_fd(fd);
        }

        result
    }

    pub fn info(&self) -> WsdisplayInfo {
        self.info
    }

    pub fn framebuffer_mut(&mut self) -> &mut [u8] {
        let visible = unsafe { (self.mapping as *mut u8).add(self.info.fb_offset) };
        unsafe { std::slice::from_raw_parts_mut(visible, self.info.visible_len()) }
    }

    pub fn present(&mut self, frame: &[u8]) -> Result<(), String> {
        let framebuffer = self.framebuffer_mut();
        if frame.len() != framebuffer.len() {
            return Err(format!(
                "wsdisplay present length mismatch: frame={} framebuffer={}",
                frame.len(),
                framebuffer.len()
            ));
        }
        framebuffer.copy_from_slice(frame);
        Ok(())
    }
}

impl Drop for WsdisplaySurface {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.mapping, self.map_len);
        }
        let _ = set_mode(self.fd, WSDISPLAYIO_MODE_EMUL);
        close_fd(self.fd);
    }
}

fn hold_with_proactor(hold: Duration) -> Result<(), String> {
    let proactor = Proactor::new(KqueuePort::new().map_err(|err| {
        format!("failed to create NetBSD kqueue proactor for display hold: {err}")
    })?);
    let handle = proactor.handle();
    let stop_handle = handle.clone();
    handle
        .defer_for(hold, CompletionKind::Timer, 0, move |_| {
            let _ = stop_handle.stop();
        })
        .map_err(|err| format!("failed to schedule display hold timer: {err}"))?;
    proactor
        .run_until_stopped()
        .map_err(|err| format!("display hold proactor failed: {err}"))
}

fn probe_fd(fd: RawFd) -> Result<WsdisplayInfo, String> {
    let modern = get_modern_fb_info(fd);
    if let Ok(info) = modern {
        return Ok(info);
    }

    let legacy = get_legacy_fb_info(fd)?;
    let stride = get_linebytes(fd).unwrap_or_else(|_| {
        let bytes_per_pixel = ((legacy.depth.max(8) + 7) / 8).max(1);
        legacy.width * bytes_per_pixel
    });

    Ok(WsdisplayInfo {
        width: legacy.width,
        height: legacy.height,
        stride,
        bits_per_pixel: legacy.depth,
        fb_size: (stride as usize).saturating_mul(legacy.height as usize),
        fb_offset: 0,
        pixel_type: WSFB_RGB,
        red_offset: 16,
        red_size: 8,
        green_offset: 8,
        green_size: 8,
        blue_offset: 0,
        blue_size: 8,
        alpha_offset: 24,
        alpha_size: 0,
    })
}

fn get_modern_fb_info(fd: RawFd) -> Result<WsdisplayInfo, String> {
    let mut raw = WsdisplayioFbInfo::default();
    ioctl_ref(
        fd,
        iowr(WSDISPLAY_GROUP, 104, mem::size_of::<WsdisplayioFbInfo>()),
        &mut raw,
    )
    .map_err(|err| format!("WSDISPLAYIO_GET_FBINFO failed: {err}"))?;

    if raw.width == 0 || raw.height == 0 || raw.stride == 0 || raw.bits_per_pixel == 0 {
        return Err("WSDISPLAYIO_GET_FBINFO returned incomplete geometry".to_string());
    }

    Ok(WsdisplayInfo {
        width: raw.width,
        height: raw.height,
        stride: raw.stride,
        bits_per_pixel: raw.bits_per_pixel,
        fb_size: raw.fb_size as usize,
        fb_offset: raw.fb_offset as usize,
        pixel_type: raw.pixel_type,
        red_offset: raw.red_offset,
        red_size: raw.red_size,
        green_offset: raw.green_offset,
        green_size: raw.green_size,
        blue_offset: raw.blue_offset,
        blue_size: raw.blue_size,
        alpha_offset: raw.alpha_offset,
        alpha_size: raw.alpha_size,
    })
}

fn get_legacy_fb_info(fd: RawFd) -> Result<WsdisplayFbInfo, String> {
    let mut raw = WsdisplayFbInfo::default();
    ioctl_ref(
        fd,
        ior(WSDISPLAY_GROUP, 65, mem::size_of::<WsdisplayFbInfo>()),
        &mut raw,
    )
    .map_err(|err| format!("WSDISPLAYIO_GINFO failed: {err}"))?;
    Ok(raw)
}

fn get_linebytes(fd: RawFd) -> Result<u32, String> {
    let mut linebytes: libc::c_uint = 0;
    ioctl_ref(
        fd,
        ior(WSDISPLAY_GROUP, 95, mem::size_of::<libc::c_uint>()),
        &mut linebytes,
    )
    .map_err(|err| format!("WSDISPLAYIO_LINEBYTES failed: {err}"))?;
    Ok(linebytes)
}

fn set_mode(fd: RawFd, mode: u32) -> Result<(), String> {
    let mut mode = mode as libc::c_uint;
    ioctl_ref(
        fd,
        iow(WSDISPLAY_GROUP, 76, mem::size_of::<libc::c_uint>()),
        &mut mode,
    )
    .map_err(|err| format!("WSDISPLAYIO_SMODE({mode}) failed: {err}"))
}

fn paint_pattern(framebuffer: &mut [u8], info: WsdisplayInfo) {
    let bytes_per_pixel = info.bytes_per_pixel();
    for y in 0..info.height as usize {
        let row_start = y * info.stride as usize;
        for x in 0..info.width as usize {
            let r = ((x * 255) / (info.width.max(1) as usize)) as u8;
            let g = ((y * 255) / (info.height.max(1) as usize)) as u8;
            let checker = (((x / 32) ^ (y / 32)) & 1) as u8;
            let b = if checker == 0 { 0x30 } else { 0xd8 };
            let pixel = pack_rgb(info, r, g, b);
            let dst = row_start + x * bytes_per_pixel;
            write_pixel(&mut framebuffer[dst..], bytes_per_pixel, pixel);
        }
    }
}

fn pack_rgb(info: WsdisplayInfo, red: u8, green: u8, blue: u8) -> u32 {
    if info.pixel_type != WSFB_RGB {
        return u32::from(red);
    }

    pack_channel(red, info.red_offset, info.red_size)
        | pack_channel(green, info.green_offset, info.green_size)
        | pack_channel(blue, info.blue_offset, info.blue_size)
        | if info.alpha_size > 0 {
            pack_channel(0xff, info.alpha_offset, info.alpha_size)
        } else {
            0
        }
}

fn pack_channel(value: u8, offset: u32, size: u32) -> u32 {
    if size == 0 {
        return 0;
    }
    let max = if size >= 32 {
        u32::MAX
    } else {
        (1u32 << size) - 1
    };
    ((u32::from(value) * max + 127) / 255) << offset
}

fn write_pixel(dst: &mut [u8], bytes_per_pixel: usize, pixel: u32) {
    let bytes = pixel.to_ne_bytes();
    match bytes_per_pixel {
        1 => dst[0] = bytes[0],
        2 => dst[..2].copy_from_slice(&bytes[..2]),
        3 => dst[..3].copy_from_slice(&bytes[..3]),
        _ => dst[..4].copy_from_slice(&bytes),
    }
}

fn open_wsdisplay(device_path: &str) -> Result<RawFd, String> {
    let path = CString::new(device_path)
        .map_err(|_| format!("wsdisplay path contains an interior NUL: {device_path:?}"))?;
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR) };
    if fd < 0 {
        return Err(format!("failed to open {device_path}: {}", last_error()));
    }
    Ok(fd)
}

fn close_fd(fd: RawFd) {
    unsafe {
        libc::close(fd);
    }
}

fn ioctl_ref<T>(fd: RawFd, request: libc::c_ulong, value: &mut T) -> io::Result<()> {
    let rc = unsafe { libc::ioctl(fd, request, value as *mut T) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn ior(group: u8, number: u8, len: usize) -> libc::c_ulong {
    ioc(IOC_OUT, group, number, len)
}

fn iow(group: u8, number: u8, len: usize) -> libc::c_ulong {
    ioc(IOC_IN, group, number, len)
}

fn iowr(group: u8, number: u8, len: usize) -> libc::c_ulong {
    ioc(IOC_INOUT, group, number, len)
}

fn ioc(direction: u64, group: u8, number: u8, len: usize) -> libc::c_ulong {
    (direction | (((len & IOCPARM_MASK) as u64) << 16) | ((group as u64) << 8) | number as u64)
        as libc::c_ulong
}

fn last_error() -> io::Error {
    io::Error::last_os_error()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn netbsd_ioctl_numbers_match_headers() {
        assert_eq!(
            ior(WSDISPLAY_GROUP, 65, mem::size_of::<WsdisplayFbInfo>()),
            0x4010_5741
        );
        assert_eq!(
            iow(WSDISPLAY_GROUP, 76, mem::size_of::<libc::c_uint>()),
            0x8004_574c
        );
        assert_eq!(
            ior(WSDISPLAY_GROUP, 95, mem::size_of::<libc::c_uint>()),
            0x4004_575f
        );
        assert_eq!(
            iowr(WSDISPLAY_GROUP, 104, mem::size_of::<WsdisplayioFbInfo>()),
            0xc048_5768
        );
    }
}
