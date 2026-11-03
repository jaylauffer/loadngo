//! The GL boundary: the calls the renderer makes, as a trait, and the WebGL2 enums it uses.
//!
//! On wasm32 [`crate::web::WebGl`] implements [`Gl`] with imports from `js/gl.js`; the host's unit
//! tests implement it with a recording mock, so no test touches a GL context. Handles are small
//! integers from the shim's tables (0 is "none"). Byte slices are views of wasm memory: the shim
//! reads them in place, so nothing is copied or allocated on the JS side.

pub const DEPTH_BUFFER_BIT: u32 = 0x0100;
pub const COLOR_BUFFER_BIT: u32 = 0x4000;
pub const TRIANGLES: u32 = 0x0004;
pub const ONE: u32 = 1;
pub const SRC_ALPHA: u32 = 0x0302;
pub const ONE_MINUS_SRC_ALPHA: u32 = 0x0303;
pub const CW: u32 = 0x0900;
pub const CCW: u32 = 0x0901;
pub const CULL_FACE: u32 = 0x0B44;
pub const DEPTH_TEST: u32 = 0x0B71;
pub const BLEND: u32 = 0x0BE2;
pub const MAX_TEXTURE_SIZE: u32 = 0x0D33;
pub const UNSIGNED_SHORT: u32 = 0x1403;
pub const UNSIGNED_INT: u32 = 0x1405;
pub const FLOAT: u32 = 0x1406;
pub const NEAREST: u32 = 0x2600;
pub const LINEAR: u32 = 0x2601;
pub const LINEAR_MIPMAP_LINEAR: u32 = 0x2703;
pub const REPEAT: u32 = 0x2901;
pub const CLAMP_TO_EDGE: u32 = 0x812F;
pub const MIRRORED_REPEAT: u32 = 0x8370;
pub const RGBA8: u32 = 0x8058;
pub const ARRAY_BUFFER: u32 = 0x8892;
pub const ELEMENT_ARRAY_BUFFER: u32 = 0x8893;
pub const STATIC_DRAW: u32 = 0x88E4;
pub const DYNAMIC_DRAW: u32 = 0x88E8;
pub const MAX_UNIFORM_BLOCK_SIZE: u32 = 0x8A30;
pub const UNIFORM_BUFFER: u32 = 0x8A11;
pub const UNIFORM_BUFFER_OFFSET_ALIGNMENT: u32 = 0x8A34;
pub const SRGB8_ALPHA8: u32 = 0x8C43;
pub const INT_2_10_10_10_REV: u32 = 0x8D9F;

/// A vertex attribute's format within a buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attrib {
    pub location: u32,
    /// Components (1 to 4).
    pub size: u32,
    pub ty: u32,
    pub normalized: bool,
    pub stride: u32,
    pub offset: u32,
}

/// A texture to create from the image source the JS side has staged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextureUpload {
    /// `SRGB8_ALPHA8` for colour, `RGBA8` for data.
    pub internal_format: u32,
    pub width: u32,
    pub height: u32,
    /// Mip levels to allocate; above 1, the shim generates them.
    pub levels: u32,
    pub wrap_s: u32,
    pub wrap_t: u32,
    pub min_filter: u32,
    pub mag_filter: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Info = 0,
    Warn = 1,
    Error = 2,
}

/// Every GL call the renderer makes. Calls that create something return a handle, 0 on failure.
pub trait Gl {
    /// `getParameter` for an integer value.
    fn get_integer(&mut self, pname: u32) -> i32;
    /// Whether `EXT_disjoint_timer_query_webgl2` is available.
    fn timer_query_supported(&mut self) -> bool;

    /// Compiles and links a program without waiting for the result, so the driver can compile
    /// several at once (`KHR_parallel_shader_compile`); [`Gl::program_check`] waits.
    fn program_create(&mut self, vertex: &str, fragment: &str) -> u32;
    /// Whether the program linked; on failure the shim logs the compiler's messages.
    fn program_check(&mut self, program: u32) -> bool;
    /// Binds a uniform block to a binding point; false if the program has no such block.
    fn program_bind_block(&mut self, program: u32, block: &str, binding: u32) -> bool;
    /// Points a sampler at a texture unit (this leaves the program in use).
    fn program_bind_sampler(&mut self, program: u32, sampler: &str, unit: u32) -> bool;
    /// `UNIFORM_BLOCK_DATA_SIZE`, or -1 if the block is missing.
    fn program_block_size(&mut self, program: u32, block: &str) -> i32;
    /// `UNIFORM_OFFSET` of a uniform in a block, or -1 if it is not active.
    fn program_uniform_offset(&mut self, program: u32, uniform: &str) -> i32;
    fn program_delete(&mut self, program: u32);
    fn use_program(&mut self, program: u32);

    /// Creates a buffer holding `data` (bound to `target` afterwards).
    fn buffer_create(&mut self, target: u32, data: &[u8], usage: u32) -> u32;
    /// Creates a buffer of `size` uninitialised bytes (bound to `target` afterwards).
    fn buffer_alloc(&mut self, target: u32, size: u32, usage: u32) -> u32;
    /// `bufferSubData`, straight from wasm memory.
    fn buffer_update(&mut self, buffer: u32, target: u32, offset: u32, data: &[u8]);
    fn buffer_delete(&mut self, buffer: u32);
    fn bind_buffer_range(&mut self, target: u32, index: u32, buffer: u32, offset: u32, size: u32);

    fn vertex_array_create(&mut self) -> u32;
    /// Binds a vertex array; 0 unbinds.
    fn vertex_array_bind(&mut self, vao: u32);
    fn vertex_array_delete(&mut self, vao: u32);
    /// Enables an attribute of the bound vertex array and points it into `buffer`.
    fn vertex_attrib(&mut self, buffer: u32, attrib: &Attrib);
    /// Attaches an index buffer to the bound vertex array.
    fn index_buffer_bind(&mut self, buffer: u32);

    /// Creates a texture from the staged image source, leaving it bound to unit 0.
    fn texture_create_staged(&mut self, upload: &TextureUpload) -> u32;
    fn texture_bind(&mut self, unit: u32, texture: u32);
    fn texture_delete(&mut self, texture: u32);

    fn enable(&mut self, cap: u32);
    fn disable(&mut self, cap: u32);
    fn front_face(&mut self, mode: u32);
    fn depth_mask(&mut self, write: bool);
    fn blend_func_separate(&mut self, src_rgb: u32, dst_rgb: u32, src_alpha: u32, dst_alpha: u32);
    fn viewport(&mut self, x: i32, y: i32, width: u32, height: u32);
    fn clear_color(&mut self, r: f32, g: f32, b: f32, a: f32);
    fn clear(&mut self, mask: u32);
    fn draw_elements(&mut self, mode: u32, count: u32, ty: u32, offset: u32);
    /// Tells a tile-based GPU it need not write the depth buffer back to memory at the end of
    /// the frame (`invalidateFramebuffer` on the default framebuffer's depth).
    fn invalidate_depth(&mut self);
    fn flush(&mut self);

    fn query_create(&mut self) -> u32;
    fn query_delete(&mut self, query: u32);
    /// Begins a `TIME_ELAPSED_EXT` query.
    fn query_begin(&mut self, query: u32);
    fn query_end(&mut self);
    fn query_available(&mut self, query: u32) -> bool;
    /// The query's result in nanoseconds, as WebGL reports it: an integer, which reaches wasm
    /// without the JS side boxing a fractional number on every frame.
    fn query_result_ns(&mut self, query: u32) -> f64;
    /// Reads and clears `GPU_DISJOINT_EXT`.
    fn gpu_disjoint(&mut self) -> bool;

    /// A monotonic clock in milliseconds (`performance.now()`), for the frame's CPU time.
    fn now_ms(&mut self) -> f64;

    fn log(&mut self, level: LogLevel, message: &str);
}
