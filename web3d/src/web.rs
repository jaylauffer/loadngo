//! The wasm32 boundary.
//!
//! **Imports** come from `js/gl.js` as module `web3d_gl`: plain functions over numbers. Byte
//! slices cross as a pointer and a length into this module's memory, which the shim reads in
//! place (`bufferSubData(target, offset, heap, ptr, len)`), so a frame allocates nothing on
//! either side.
//!
//! **Exports** are C-ABI functions taking a `*mut Web3d` from [`web3d_create`]. `js/web3d.js`
//! wraps them in a `Renderer` class. Matrices and material values go through a fixed exchange
//! area ([`web3d_io`]) that the JS side writes through a `Float32Array` view, and per-frame
//! numbers come back through another ([`web3d_stats`]). Functions that create return a handle (a
//! double: handles are wider than 32 bits, see `slots`),
//! 0 on failure; others return 0 for success and 1 for failure. Either way the reason is in
//! [`web3d_error_ptr`] / [`web3d_error_len`] (UTF-8).

use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::cell::RefCell;
use std::fmt::Write;
use std::sync::Once;

use crate::gl::{Attrib, Gl, LogLevel, TextureUpload};
use crate::math::Mat4;
use crate::renderer::{AlphaMode, Error, MaterialDesc, MeshInput, Renderer, Result, TextureDesc};
use crate::slots;

mod import {
    #[link(wasm_import_module = "web3d_gl")]
    unsafe extern "C" {
        pub fn get_integer(pname: u32) -> i32;
        pub fn timer_query_supported() -> u32;
        pub fn program_create(vs: *const u8, vs_len: usize, fs: *const u8, fs_len: usize) -> u32;
        pub fn program_check(program: u32) -> u32;
        pub fn program_bind_block(program: u32, name: *const u8, len: usize, binding: u32) -> u32;
        pub fn program_bind_sampler(program: u32, name: *const u8, len: usize, unit: u32) -> u32;
        pub fn program_block_size(program: u32, name: *const u8, len: usize) -> i32;
        pub fn program_uniform_offset(program: u32, name: *const u8, len: usize) -> i32;
        pub fn program_delete(program: u32);
        pub fn use_program(program: u32);
        pub fn buffer_create(target: u32, data: *const u8, len: usize, usage: u32) -> u32;
        pub fn buffer_alloc(target: u32, size: u32, usage: u32) -> u32;
        pub fn buffer_update(buffer: u32, target: u32, offset: u32, data: *const u8, len: usize);
        pub fn buffer_delete(buffer: u32);
        pub fn bind_buffer_range(target: u32, index: u32, buffer: u32, offset: u32, size: u32);
        pub fn vertex_array_create() -> u32;
        pub fn vertex_array_bind(vao: u32);
        pub fn vertex_array_delete(vao: u32);
        pub fn vertex_attrib(
            location: u32,
            buffer: u32,
            size: u32,
            ty: u32,
            normalized: u32,
            stride: u32,
            offset: u32,
        );
        pub fn index_buffer_bind(buffer: u32);
        pub fn texture_create_staged(
            internal_format: u32,
            width: u32,
            height: u32,
            levels: u32,
            wrap_s: u32,
            wrap_t: u32,
            min_filter: u32,
            mag_filter: u32,
        ) -> u32;
        pub fn texture_bind(unit: u32, texture: u32);
        pub fn texture_delete(texture: u32);
        pub fn enable(cap: u32);
        pub fn disable(cap: u32);
        pub fn front_face(mode: u32);
        pub fn depth_mask(write: u32);
        pub fn blend_func_separate(src_rgb: u32, dst_rgb: u32, src_alpha: u32, dst_alpha: u32);
        pub fn viewport(x: i32, y: i32, width: u32, height: u32);
        pub fn clear_color(r: f32, g: f32, b: f32, a: f32);
        pub fn clear(mask: u32);
        pub fn draw_elements(mode: u32, count: u32, ty: u32, offset: u32);
        pub fn invalidate_depth();
        pub fn flush();
        pub fn query_create() -> u32;
        pub fn query_delete(query: u32);
        pub fn query_begin(query: u32);
        pub fn query_end();
        pub fn query_available(query: u32) -> u32;
        pub fn query_result_ns(query: u32) -> f64;
        pub fn gpu_disjoint() -> u32;
        pub fn log(level: u32, message: *const u8, len: usize);
        /// `performance.now`, bound to `performance`. Each reading is still a number V8 boxes
        /// (measured: 20.8 sampled bytes a frame for the two, `evidence/phase1/cpu-timing-allocation/`),
        /// which is why CPU timing is off by default.
        pub fn now() -> f64;
    }
}

/// [`Gl`] over the imports. Every method is a direct call; nothing is buffered.
pub struct WebGl;

// SAFETY (for every `unsafe` block below): the imports take only numbers; pointer and length
// pairs always describe a live slice of this module's memory, borrowed for the duration of the
// call, which the shim reads and does not keep.
impl Gl for WebGl {
    fn get_integer(&mut self, pname: u32) -> i32 {
        unsafe { import::get_integer(pname) }
    }
    fn timer_query_supported(&mut self) -> bool {
        unsafe { import::timer_query_supported() != 0 }
    }
    fn program_create(&mut self, vertex: &str, fragment: &str) -> u32 {
        unsafe {
            import::program_create(
                vertex.as_ptr(),
                vertex.len(),
                fragment.as_ptr(),
                fragment.len(),
            )
        }
    }
    fn program_check(&mut self, program: u32) -> bool {
        unsafe { import::program_check(program) != 0 }
    }
    fn program_bind_block(&mut self, program: u32, block: &str, binding: u32) -> bool {
        unsafe { import::program_bind_block(program, block.as_ptr(), block.len(), binding) != 0 }
    }
    fn program_bind_sampler(&mut self, program: u32, sampler: &str, unit: u32) -> bool {
        unsafe { import::program_bind_sampler(program, sampler.as_ptr(), sampler.len(), unit) != 0 }
    }
    fn program_block_size(&mut self, program: u32, block: &str) -> i32 {
        unsafe { import::program_block_size(program, block.as_ptr(), block.len()) }
    }
    fn program_uniform_offset(&mut self, program: u32, uniform: &str) -> i32 {
        unsafe { import::program_uniform_offset(program, uniform.as_ptr(), uniform.len()) }
    }
    fn program_delete(&mut self, program: u32) {
        unsafe { import::program_delete(program) }
    }
    fn use_program(&mut self, program: u32) {
        unsafe { import::use_program(program) }
    }
    fn buffer_create(&mut self, target: u32, data: &[u8], usage: u32) -> u32 {
        unsafe { import::buffer_create(target, data.as_ptr(), data.len(), usage) }
    }
    fn buffer_alloc(&mut self, target: u32, size: u32, usage: u32) -> u32 {
        unsafe { import::buffer_alloc(target, size, usage) }
    }
    fn buffer_update(&mut self, buffer: u32, target: u32, offset: u32, data: &[u8]) {
        unsafe { import::buffer_update(buffer, target, offset, data.as_ptr(), data.len()) }
    }
    fn buffer_delete(&mut self, buffer: u32) {
        unsafe { import::buffer_delete(buffer) }
    }
    fn bind_buffer_range(&mut self, target: u32, index: u32, buffer: u32, offset: u32, size: u32) {
        unsafe { import::bind_buffer_range(target, index, buffer, offset, size) }
    }
    fn vertex_array_create(&mut self) -> u32 {
        unsafe { import::vertex_array_create() }
    }
    fn vertex_array_bind(&mut self, vao: u32) {
        unsafe { import::vertex_array_bind(vao) }
    }
    fn vertex_array_delete(&mut self, vao: u32) {
        unsafe { import::vertex_array_delete(vao) }
    }
    fn vertex_attrib(&mut self, buffer: u32, a: &Attrib) {
        unsafe {
            import::vertex_attrib(
                a.location,
                buffer,
                a.size,
                a.ty,
                u32::from(a.normalized),
                a.stride,
                a.offset,
            )
        }
    }
    fn index_buffer_bind(&mut self, buffer: u32) {
        unsafe { import::index_buffer_bind(buffer) }
    }
    fn texture_create_staged(&mut self, u: &TextureUpload) -> u32 {
        unsafe {
            import::texture_create_staged(
                u.internal_format,
                u.width,
                u.height,
                u.levels,
                u.wrap_s,
                u.wrap_t,
                u.min_filter,
                u.mag_filter,
            )
        }
    }
    fn texture_bind(&mut self, unit: u32, texture: u32) {
        unsafe { import::texture_bind(unit, texture) }
    }
    fn texture_delete(&mut self, texture: u32) {
        unsafe { import::texture_delete(texture) }
    }
    fn enable(&mut self, cap: u32) {
        unsafe { import::enable(cap) }
    }
    fn disable(&mut self, cap: u32) {
        unsafe { import::disable(cap) }
    }
    fn front_face(&mut self, mode: u32) {
        unsafe { import::front_face(mode) }
    }
    fn depth_mask(&mut self, write: bool) {
        unsafe { import::depth_mask(u32::from(write)) }
    }
    fn blend_func_separate(&mut self, src_rgb: u32, dst_rgb: u32, src_alpha: u32, dst_alpha: u32) {
        unsafe { import::blend_func_separate(src_rgb, dst_rgb, src_alpha, dst_alpha) }
    }
    fn viewport(&mut self, x: i32, y: i32, width: u32, height: u32) {
        unsafe { import::viewport(x, y, width, height) }
    }
    fn clear_color(&mut self, r: f32, g: f32, b: f32, a: f32) {
        unsafe { import::clear_color(r, g, b, a) }
    }
    fn clear(&mut self, mask: u32) {
        unsafe { import::clear(mask) }
    }
    fn draw_elements(&mut self, mode: u32, count: u32, ty: u32, offset: u32) {
        unsafe { import::draw_elements(mode, count, ty, offset) }
    }
    fn invalidate_depth(&mut self) {
        unsafe { import::invalidate_depth() }
    }
    fn flush(&mut self) {
        unsafe { import::flush() }
    }
    fn query_create(&mut self) -> u32 {
        unsafe { import::query_create() }
    }
    fn query_delete(&mut self, query: u32) {
        unsafe { import::query_delete(query) }
    }
    fn query_begin(&mut self, query: u32) {
        unsafe { import::query_begin(query) }
    }
    fn query_end(&mut self) {
        unsafe { import::query_end() }
    }
    fn query_available(&mut self, query: u32) -> bool {
        unsafe { import::query_available(query) != 0 }
    }
    fn query_result_ns(&mut self, query: u32) -> f64 {
        unsafe { import::query_result_ns(query) }
    }
    fn gpu_disjoint(&mut self) -> bool {
        unsafe { import::gpu_disjoint() != 0 }
    }
    fn now_ms(&mut self) -> f64 {
        unsafe { import::now() }
    }
    fn log(&mut self, level: LogLevel, message: &str) {
        unsafe { import::log(level as u32, message.as_ptr(), message.len()) }
    }
}

/// GPU readings older than this many frames are reported as none (NaN), so a stalled timer does
/// not leave an old, low reading standing.
pub const GPU_STALE_FRAMES: u64 = 30;

/// Floats in the exchange area: two matrices, or a material's values, or a look-at camera.
pub const IO_FLOATS: usize = 64;
/// Numbers in the stats area; see [`web3d_stats`].
pub const STATS_LEN: usize = 8;

/// A renderer with its exchange areas and last error.
pub struct Web3d {
    renderer: Renderer<WebGl>,
    io: [f32; IO_FLOATS],
    stats: [f64; STATS_LEN],
    error: String,
}

impl Web3d {
    fn fail(&mut self, error: impl std::fmt::Display) {
        self.error.clear();
        let _ = write!(self.error, "{error}");
    }

    fn status(&mut self, result: Result<()>) -> u32 {
        match result {
            Ok(()) => 0,
            Err(e) => {
                self.fail(e);
                1
            }
        }
    }

    /// A handle for JS, as an exact double (every handle is below 2^53), or 0 on failure.
    fn handle(&mut self, result: Result<u64>) -> f64 {
        match result {
            Ok(h) => h as f64,
            Err(e) => {
                self.fail(e);
                0.0
            }
        }
    }

    fn matrix(&self, at: usize) -> Mat4 {
        let mut m = [0.0; 16];
        m.copy_from_slice(&self.io[at..at + 16]);
        m
    }
}

/// The renderer behind a pointer from [`web3d_create`], or `None` for a null pointer, which every
/// export refuses (a handle-returning call returns 0, a status call 1, the rest do nothing) instead
/// of treating address 0 as a renderer. `js/web3d.js` passes 0 once a renderer is disposed, and
/// throws before it gets that far.
///
/// # Safety
/// `w` is null, or came from [`web3d_create`] and has not been destroyed; the JS side never calls
/// in re-entrantly, so this is the only reference.
unsafe fn web3d<'a>(w: *mut Web3d) -> Option<&'a mut Web3d> {
    // SAFETY: as above; `as_mut` checks for null.
    unsafe { w.as_mut() }
}

/// A handle from JS: a whole number from 1 to below 2^53, else 0, which names nothing. Handles
/// cross as doubles because they are wider than 32 bits ([`crate::slots`]).
fn handle_in(h: f64) -> u64 {
    if h >= 1.0 && h < slots::HANDLE_LIMIT as f64 && h.fract() == 0.0 {
        h as u64
    } else {
        0
    }
}

/// What [`web3d_error_ptr`] and [`web3d_error_len`] report for a null renderer.
const NULL_RENDERER: &str = "no renderer (a null pointer: disposed, or never created)";

thread_local! {
    /// Every live staging allocation from [`web3d_alloc`], as (address, bytes). An array the JS side
    /// passes in must lie inside one of them, and only these may be freed, so a wrong pointer or
    /// length from JS is refused instead of becoming a slice past an allocation or a bad free.
    static STAGED: RefCell<Vec<(usize, usize)>> = const { RefCell::new(Vec::new()) };
}

/// A staged array: `len` values of `T` at `ptr`, which must lie inside one live staging
/// allocation and be aligned for `T`. `Ok(None)` for a null pointer (an absent array).
fn staged<'a, T>(ptr: *const T, len: usize) -> std::result::Result<Option<&'a [T]>, Error> {
    const OUTSIDE: Error =
        Error::Mesh("an array's pointer and length do not lie inside one staging allocation");
    if ptr.is_null() {
        return Ok(None);
    }
    let start = ptr as usize;
    let end = len
        .checked_mul(size_of::<T>())
        .and_then(|bytes| start.checked_add(bytes))
        .ok_or(OUTSIDE)?;
    let inside = STAGED.with(|s| {
        s.borrow()
            .iter()
            .any(|&(at, bytes)| start >= at && end <= at + bytes)
    });
    if !inside || !start.is_multiple_of(align_of::<T>()) {
        return Err(OUTSIDE);
    }
    // SAFETY: the range lies inside a live allocation from `web3d_alloc`, which zeroes it (so every
    // byte is initialised), it is aligned for `T`, and nothing else in this module touches staging
    // memory during the call.
    Ok(Some(unsafe { std::slice::from_raw_parts(ptr, len) }))
}

static PANIC_HOOK: Once = Once::new();

/// Sends panic messages to the console before the module traps.
fn install_panic_hook() {
    PANIC_HOOK.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            let message = info.to_string();
            WebGl.log(LogLevel::Error, &message);
        }));
    });
}

const ALIGN: usize = 16;

/// Allocates `len` bytes (16-aligned, zeroed) for the JS side to stage data in; free with
/// [`web3d_free`]. Null if it cannot.
#[unsafe(no_mangle)]
pub extern "C" fn web3d_alloc(len: usize) -> *mut u8 {
    let Ok(layout) = Layout::from_size_align(len.max(1), ALIGN) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the layout is non-zero in size.
    let ptr = unsafe { alloc_zeroed(layout) };
    if !ptr.is_null() {
        STAGED.with(|s| s.borrow_mut().push((ptr as usize, layout.size())));
    }
    ptr
}

/// Frees a staging allocation. A pointer and length that are not exactly a live allocation from
/// [`web3d_alloc`] are ignored, so this is safe to call with any numbers.
#[unsafe(no_mangle)]
// Safe for any arguments: only a pointer found in the live-allocation list is freed.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn web3d_free(ptr: *mut u8, len: usize) {
    let Ok(layout) = Layout::from_size_align(len.max(1), ALIGN) else {
        return;
    };
    let live = STAGED.with(|s| {
        let mut s = s.borrow_mut();
        let found = s
            .iter()
            .position(|&entry| entry == (ptr as usize, layout.size()));
        found.map(|i| s.swap_remove(i)).is_some()
    });
    if live {
        // SAFETY: `ptr` came from `web3d_alloc` with this layout and is freed once (its entry is
        // gone now).
        unsafe { dealloc(ptr, layout) }
    }
}

/// A renderer that has made no GL calls yet; call [`web3d_init`] next. Every handle it issues
/// carries `serial` (below 2^20). `js/web3d.js` gives each renderer a page its own, from 1 up and
/// never twice, so renderers refuse each other's handles and never issue the same one; a raw
/// caller that passes nothing gets serial 0. Null if the serial is out of range.
#[unsafe(no_mangle)]
pub extern "C" fn web3d_create(serial: u32) -> *mut Web3d {
    install_panic_hook();
    let Some(renderer) = Renderer::with_serial(WebGl, serial) else {
        return std::ptr::null_mut();
    };
    Box::into_raw(Box::new(Web3d {
        renderer,
        io: [0.0; IO_FLOATS],
        stats: [f64::NAN; STATS_LEN],
        error: String::new(),
    }))
}

/// # Safety
/// `w` is null (ignored) or came from [`web3d_create`]; it is invalid afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn web3d_destroy(w: *mut Web3d) {
    if !w.is_null() {
        // SAFETY: as above; dropping releases every GL object.
        drop(unsafe { Box::from_raw(w) });
    }
}

/// # Safety
/// `w` is null (refused) or came from [`web3d_create`] and has not been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn web3d_init(w: *mut Web3d, width: u32, height: u32) -> u32 {
    // SAFETY: see the function's contract.
    let Some(w) = (unsafe { web3d(w) }) else {
        return 1;
    };
    let result = w.renderer.init(width, height);
    w.status(result)
}

/// # Safety
/// `w` is null (refused) or came from [`web3d_create`] and has not been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn web3d_error_ptr(w: *mut Web3d) -> *const u8 {
    // SAFETY: see the function's contract.
    unsafe { web3d(w) }.map_or(NULL_RENDERER.as_ptr(), |w| w.error.as_ptr())
}

/// # Safety
/// `w` is null (refused) or came from [`web3d_create`] and has not been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn web3d_error_len(w: *mut Web3d) -> usize {
    // SAFETY: see the function's contract.
    unsafe { web3d(w) }.map_or(NULL_RENDERER.len(), |w| w.error.len())
}

/// The exchange area, [`IO_FLOATS`] floats, fixed for the renderer's life.
///
/// # Safety
/// `w` is null (refused) or came from [`web3d_create`] and has not been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn web3d_io(w: *mut Web3d) -> *mut f32 {
    // SAFETY: see the function's contract.
    unsafe { web3d(w) }.map_or(std::ptr::null_mut(), |w| w.io.as_mut_ptr())
}

/// The stats area, [`STATS_LEN`] doubles, rewritten by every [`web3d_frame`]: CPU milliseconds
/// of that frame (timed in wasm with `performance.now` when [`web3d_set_cpu_timing`] is on, NaN
/// otherwise), GPU milliseconds (NaN without a reading in the last [`GPU_STALE_FRAMES`] frames),
/// the frame that GPU time measured, draws, triangles, culled objects, program binds and material
/// binds. The JS side copies it whole with `Float64Array.prototype.set`, so reading it boxes no
/// numbers.
///
/// # Safety
/// `w` is null (refused) or came from [`web3d_create`] and has not been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn web3d_stats(w: *mut Web3d) -> *const f64 {
    // SAFETY: see the function's contract.
    unsafe { web3d(w) }.map_or(std::ptr::null(), |w| w.stats.as_ptr())
}

/// Bit 0 of [`web3d_caps`]: GPU timer queries work.
pub const CAP_TIMER: u32 = 1;

/// The largest texture side, shifted left 1, with [`CAP_TIMER`] in bit 0; 0 for a null renderer.
///
/// # Safety
/// `w` is null (refused) or came from [`web3d_create`] and has not been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn web3d_caps(w: *mut Web3d) -> u32 {
    // SAFETY: see the function's contract.
    let Some(w) = (unsafe { web3d(w) }) else {
        return 0;
    };
    let caps = w.renderer.caps();
    caps.max_texture_size << 1 | if caps.timer_query { CAP_TIMER } else { 0 }
}

/// Turns CPU timing of each frame on (1) or off (0); off by default. See
/// [`Renderer::set_cpu_timing`].
///
/// # Safety
/// `w` is null (refused) or came from [`web3d_create`] and has not been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn web3d_set_cpu_timing(w: *mut Web3d, on: u32) {
    // SAFETY: see the function's contract.
    if let Some(w) = unsafe { web3d(w) } {
        w.renderer.set_cpu_timing(on != 0);
    }
}

/// # Safety
/// `w` is null (refused) or came from [`web3d_create`] and has not been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn web3d_resize(w: *mut Web3d, width: u32, height: u32) -> u32 {
    // SAFETY: see the function's contract.
    let Some(w) = (unsafe { web3d(w) }) else {
        return 1;
    };
    let result = w.renderer.resize(width, height);
    w.status(result)
}

/// Uploads a mesh; each array is a pointer and an element count (pointer 0 for an absent
/// optional array), staged in memory from [`web3d_alloc`]. An array that does not lie inside one
/// live staging allocation is refused. Morph deltas are all targets back to back.
///
/// # Safety
/// `w` is null (refused) or came from [`web3d_create`] and has not been destroyed.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn web3d_mesh_create(
    w: *mut Web3d,
    positions: *const f32,
    positions_len: usize,
    normals: *const f32,
    normals_len: usize,
    uvs: *const f32,
    uvs_len: usize,
    indices: *const u32,
    indices_len: usize,
    joints: *const u16,
    joints_len: usize,
    weights: *const f32,
    weights_len: usize,
    morph_positions: *const f32,
    morph_len: usize,
    morph_targets: usize,
) -> f64 {
    // SAFETY: see the function's contract.
    let Some(w) = (unsafe { web3d(w) }) else {
        return 0.0;
    };
    let input = (|| {
        Ok(MeshInput {
            positions: staged(positions, positions_len)?.unwrap_or(&[]),
            normals: staged(normals, normals_len)?,
            uvs: staged(uvs, uvs_len)?,
            indices: staged(indices, indices_len)?.unwrap_or(&[]),
            joints: staged(joints, joints_len)?,
            weights: staged(weights, weights_len)?,
            morph_positions: staged(morph_positions, morph_len)?,
            morph_targets,
        })
    })();
    let result = input.and_then(|input| w.renderer.create_mesh(&input));
    w.handle(result)
}

/// # Safety
/// `w` is null (refused) or came from [`web3d_create`] and has not been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn web3d_mesh_destroy(w: *mut Web3d, mesh: f64) -> u32 {
    // SAFETY: see the function's contract.
    let Some(w) = (unsafe { web3d(w) }) else {
        return 1;
    };
    let result = w.renderer.destroy_mesh(handle_in(mesh));
    w.status(result)
}

/// Creates a texture from the image source the JS side staged just before the call.
///
/// # Safety
/// `w` is null (refused) or came from [`web3d_create`] and has not been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn web3d_texture_create(
    w: *mut Web3d,
    width: u32,
    height: u32,
    flags: u32,
) -> f64 {
    // SAFETY: see the function's contract.
    let Some(w) = (unsafe { web3d(w) }) else {
        return 0.0;
    };
    // Flags as `TextureDesc::from_flags` reads them; an unknown wrap mode is refused.
    let result =
        TextureDesc::from_flags(width, height, flags).and_then(|d| w.renderer.create_texture(&d));
    w.handle(result)
}

/// # Safety
/// `w` is null (refused) or came from [`web3d_create`] and has not been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn web3d_texture_destroy(w: *mut Web3d, texture: f64) -> u32 {
    // SAFETY: see the function's contract.
    let Some(w) = (unsafe { web3d(w) }) else {
        return 1;
    };
    let result = w.renderer.destroy_texture(handle_in(texture));
    w.status(result)
}

/// Creates a material. The exchange area holds the linear base colour (4 floats), then the
/// alpha cutoff, roughness and metallic.
///
/// # Safety
/// `w` is null (refused) or came from [`web3d_create`] and has not been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn web3d_material_create(
    w: *mut Web3d,
    texture: f64,
    alpha_mode: u32,
    double_sided: u32,
) -> f64 {
    // SAFETY: see the function's contract.
    let Some(w) = (unsafe { web3d(w) }) else {
        return 0.0;
    };
    let Some(alpha_mode) = AlphaMode::from_u32(alpha_mode) else {
        w.fail("material refused: alpha mode must be 0 (opaque), 1 (mask) or 2 (blend)");
        return 0.0;
    };
    // 0 is "no texture"; anything else must be a texture handle.
    let texture = if texture == 0.0 {
        0
    } else {
        handle_in(texture).max(1)
    };
    let io = &w.io;
    let desc = MaterialDesc {
        base_color: [io[0], io[1], io[2], io[3]],
        texture,
        alpha_mode,
        alpha_cutoff: io[4],
        double_sided: double_sided != 0,
        roughness: io[5],
        metallic: io[6],
    };
    let result = w.renderer.create_material(&desc);
    w.handle(result)
}

/// # Safety
/// `w` is null (refused) or came from [`web3d_create`] and has not been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn web3d_material_destroy(w: *mut Web3d, material: f64) -> u32 {
    // SAFETY: see the function's contract.
    let Some(w) = (unsafe { web3d(w) }) else {
        return 1;
    };
    let result = w.renderer.destroy_material(handle_in(material));
    w.status(result)
}

/// Places a mesh with a material; the model matrix is in the exchange area's first 16 floats.
///
/// # Safety
/// `w` is null (refused) or came from [`web3d_create`] and has not been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn web3d_object_create(w: *mut Web3d, mesh: f64, material: f64) -> f64 {
    // SAFETY: see the function's contract.
    let Some(w) = (unsafe { web3d(w) }) else {
        return 0.0;
    };
    let model = w.matrix(0);
    let result = w
        .renderer
        .create_object(handle_in(mesh), handle_in(material), &model);
    w.handle(result)
}

/// Moves an object to the model matrix in the exchange area's first 16 floats.
///
/// # Safety
/// `w` is null (refused) or came from [`web3d_create`] and has not been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn web3d_object_set_transform(w: *mut Web3d, object: f64) -> u32 {
    // SAFETY: see the function's contract.
    let Some(w) = (unsafe { web3d(w) }) else {
        return 1;
    };
    let model = w.matrix(0);
    let result = w.renderer.set_transform(handle_in(object), &model);
    w.status(result)
}

/// # Safety
/// `w` is null (refused) or came from [`web3d_create`] and has not been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn web3d_object_set_visible(w: *mut Web3d, object: f64, visible: u32) -> u32 {
    // SAFETY: see the function's contract.
    let Some(w) = (unsafe { web3d(w) }) else {
        return 1;
    };
    let result = w.renderer.set_visible(handle_in(object), visible != 0);
    w.status(result)
}

/// # Safety
/// `w` is null (refused) or came from [`web3d_create`] and has not been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn web3d_object_destroy(w: *mut Web3d, object: f64) -> u32 {
    // SAFETY: see the function's contract.
    let Some(w) = (unsafe { web3d(w) }) else {
        return 1;
    };
    let result = w.renderer.destroy_object(handle_in(object));
    w.status(result)
}

/// Sets the camera from the view matrix (floats 0-15 of the exchange area) and the projection
/// (16-31).
///
/// # Safety
/// `w` is null (refused) or came from [`web3d_create`] and has not been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn web3d_set_camera(w: *mut Web3d) -> u32 {
    // SAFETY: see the function's contract.
    let Some(w) = (unsafe { web3d(w) }) else {
        return 1;
    };
    let (view, proj) = (w.matrix(0), w.matrix(16));
    let result = w.renderer.set_camera(&view, &proj);
    w.status(result)
}

/// A look-at perspective camera: the exchange area holds eye (3), target (3), up (3), vertical
/// field of view in radians, near and far.
///
/// # Safety
/// `w` is null (refused) or came from [`web3d_create`] and has not been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn web3d_set_camera_look_at(w: *mut Web3d) -> u32 {
    // SAFETY: see the function's contract.
    let Some(w) = (unsafe { web3d(w) }) else {
        return 1;
    };
    let io = w.io;
    let result = w.renderer.set_camera_look_at(
        [io[0], io[1], io[2]],
        [io[3], io[4], io[5]],
        [io[6], io[7], io[8]],
        io[9],
        io[10],
        io[11],
    );
    w.status(result)
}

/// # Safety
/// `w` is null (refused) or came from [`web3d_create`] and has not been destroyed.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn web3d_set_key_light(
    w: *mut Web3d,
    x: f32,
    y: f32,
    z: f32,
    r: f32,
    g: f32,
    b: f32,
    intensity: f32,
) -> u32 {
    // SAFETY: see the function's contract.
    let Some(w) = (unsafe { web3d(w) }) else {
        return 1;
    };
    let result = w.renderer.set_key_light([x, y, z], [r, g, b], intensity);
    w.status(result)
}

/// # Safety
/// `w` is null (refused) or came from [`web3d_create`] and has not been destroyed.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn web3d_set_ambient(
    w: *mut Web3d,
    sky_r: f32,
    sky_g: f32,
    sky_b: f32,
    ground_r: f32,
    ground_g: f32,
    ground_b: f32,
    intensity: f32,
) -> u32 {
    // SAFETY: see the function's contract.
    let Some(w) = (unsafe { web3d(w) }) else {
        return 1;
    };
    let result = w.renderer.set_ambient(
        [sky_r, sky_g, sky_b],
        [ground_r, ground_g, ground_b],
        intensity,
    );
    w.status(result)
}

/// # Safety
/// `w` is null (refused) or came from [`web3d_create`] and has not been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn web3d_set_clear_color(w: *mut Web3d, r: f32, g: f32, b: f32) -> u32 {
    // SAFETY: see the function's contract.
    let Some(w) = (unsafe { web3d(w) }) else {
        return 1;
    };
    let result = w.renderer.set_clear_color([r, g, b]);
    w.status(result)
}

/// Draws a frame and rewrites the stats area.
///
/// # Safety
/// `w` is null (refused) or came from [`web3d_create`] and has not been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn web3d_frame(w: *mut Web3d) {
    // SAFETY: see the function's contract.
    let Some(w) = (unsafe { web3d(w) }) else {
        return;
    };
    w.renderer.frame();
    let s = w.renderer.stats();
    let fresh = s.gpu_ms.is_finite() && s.frame.saturating_sub(s.gpu_frame) <= GPU_STALE_FRAMES;
    w.stats = [
        s.cpu_ms,
        if fresh { s.gpu_ms } else { f64::NAN },
        s.gpu_frame as f64,
        f64::from(s.draws),
        f64::from(s.triangles),
        f64::from(s.culled),
        f64::from(s.program_binds),
        f64::from(s.material_binds),
    ];
}
