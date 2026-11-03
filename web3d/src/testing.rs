//! Test support: a recording mock of [`Gl`], and an allocation counter for proving that a frame
//! allocates nothing.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use crate::gl::{self, Attrib, Gl, LogLevel, TextureUpload};
use crate::pack;

#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    ProgramCreate(u32),
    ProgramCheck(u32),
    BindBlock(u32, String, u32),
    BindSampler(u32, String, u32),
    ProgramDelete(u32),
    UseProgram(u32),
    BufferCreate {
        target: u32,
        len: usize,
        data: Vec<u8>,
    },
    BufferAlloc {
        target: u32,
        size: u32,
    },
    BufferUpdate {
        buffer: u32,
        offset: u32,
        len: usize,
    },
    BufferDelete(u32),
    BindRange {
        index: u32,
        buffer: u32,
        offset: u32,
        size: u32,
    },
    VaoCreate(u32),
    VaoBind(u32),
    VaoDelete(u32),
    Attrib(u32, Attrib),
    IndexBuffer(u32),
    TextureCreate(TextureUpload),
    TextureBind(u32, u32),
    TextureDelete(u32),
    Enable(u32),
    Disable(u32),
    FrontFace(u32),
    DepthMask(bool),
    BlendFunc,
    Viewport(u32, u32),
    ClearColor([f32; 4]),
    Clear(u32),
    Draw {
        count: u32,
        ty: u32,
    },
    InvalidateDepth,
    Flush,
    QueryCreate(u32),
    QueryDelete(u32),
    QueryBegin(u32),
    QueryEnd,
    Disjoint,
}

/// Records every call (unless `record` is off) and plays a configurable driver.
pub struct MockGl {
    pub calls: Vec<Call>,
    pub record: bool,
    next: u32,
    pub timer: bool,
    pub query_ready: bool,
    pub query_ms: f64,
    pub disjoint: bool,
    pub max_texture_size: i32,
    pub ubo_alignment: i32,
    pub fail_program: bool,
    pub fail_texture: bool,
    /// A uniform the driver places somewhere else.
    pub layout_override: Option<(&'static str, i32)>,
    /// Extra bytes the driver reports for every block.
    pub block_size_extra: i32,
    /// The clock: each reading is 0.25 ms after the last.
    pub clock_ms: f64,
}

impl MockGl {
    pub fn new() -> Self {
        Self {
            calls: Vec::new(),
            record: true,
            next: 0,
            timer: true,
            query_ready: true,
            query_ms: 1.0,
            disjoint: false,
            max_texture_size: 4096,
            ubo_alignment: 64,
            fail_program: false,
            fail_texture: false,
            layout_override: None,
            block_size_extra: 0,
            clock_ms: 0.0,
        }
    }

    fn push(&mut self, call: impl FnOnce() -> Call) {
        if self.record {
            self.calls.push(call());
        }
    }

    fn handle(&mut self) -> u32 {
        self.next += 1;
        self.next
    }

    /// The bytes of the `n`th buffer created since the calls were last cleared.
    pub fn buffer_data(&self, n: usize) -> Option<&[u8]> {
        self.calls
            .iter()
            .filter_map(|c| match c {
                Call::BufferCreate { data, .. } => Some(data.as_slice()),
                _ => None,
            })
            .nth(n)
    }
}

impl Gl for MockGl {
    fn get_integer(&mut self, pname: u32) -> i32 {
        match pname {
            gl::MAX_TEXTURE_SIZE => self.max_texture_size,
            gl::UNIFORM_BUFFER_OFFSET_ALIGNMENT => self.ubo_alignment,
            gl::MAX_UNIFORM_BLOCK_SIZE => 16384,
            _ => 0,
        }
    }

    fn timer_query_supported(&mut self) -> bool {
        self.timer
    }

    fn program_create(&mut self, vertex: &str, fragment: &str) -> u32 {
        assert!(vertex.starts_with("#version 300 es") && fragment.starts_with("#version 300 es"));
        let h = self.handle();
        self.push(|| Call::ProgramCreate(h));
        h
    }

    fn program_check(&mut self, program: u32) -> bool {
        self.push(|| Call::ProgramCheck(program));
        !self.fail_program
    }

    fn program_bind_block(&mut self, program: u32, block: &str, binding: u32) -> bool {
        self.push(|| Call::BindBlock(program, block.to_string(), binding));
        pack::BLOCK_LAYOUTS.iter().any(|b| b.name == block)
    }

    fn program_bind_sampler(&mut self, program: u32, sampler: &str, unit: u32) -> bool {
        self.push(|| Call::BindSampler(program, sampler.to_string(), unit));
        true
    }

    fn program_block_size(&mut self, _program: u32, block: &str) -> i32 {
        pack::BLOCK_LAYOUTS
            .iter()
            .find(|b| b.name == block)
            .map_or(-1, |b| b.size as i32 + self.block_size_extra)
    }

    fn program_uniform_offset(&mut self, _program: u32, uniform: &str) -> i32 {
        if let Some((name, offset)) = self.layout_override {
            if name == uniform {
                return offset;
            }
        }
        pack::BLOCK_LAYOUTS
            .iter()
            .flat_map(|b| b.uniforms.iter())
            .find(|(name, _)| *name == uniform)
            .map_or(-1, |(_, offset)| *offset as i32)
    }

    fn program_delete(&mut self, program: u32) {
        self.push(|| Call::ProgramDelete(program));
    }

    fn use_program(&mut self, program: u32) {
        self.push(|| Call::UseProgram(program));
    }

    fn buffer_create(&mut self, target: u32, data: &[u8], _usage: u32) -> u32 {
        self.push(|| Call::BufferCreate {
            target,
            len: data.len(),
            data: data.to_vec(),
        });
        self.handle()
    }

    fn buffer_alloc(&mut self, target: u32, size: u32, _usage: u32) -> u32 {
        self.push(|| Call::BufferAlloc { target, size });
        self.handle()
    }

    fn buffer_update(&mut self, buffer: u32, _target: u32, offset: u32, data: &[u8]) {
        self.push(|| Call::BufferUpdate {
            buffer,
            offset,
            len: data.len(),
        });
    }

    fn buffer_delete(&mut self, buffer: u32) {
        self.push(|| Call::BufferDelete(buffer));
    }

    fn bind_buffer_range(&mut self, _target: u32, index: u32, buffer: u32, offset: u32, size: u32) {
        self.push(|| Call::BindRange {
            index,
            buffer,
            offset,
            size,
        });
    }

    fn vertex_array_create(&mut self) -> u32 {
        let h = self.handle();
        self.push(|| Call::VaoCreate(h));
        h
    }

    fn vertex_array_bind(&mut self, vao: u32) {
        self.push(|| Call::VaoBind(vao));
    }

    fn vertex_array_delete(&mut self, vao: u32) {
        self.push(|| Call::VaoDelete(vao));
    }

    fn vertex_attrib(&mut self, buffer: u32, attrib: &Attrib) {
        let a = *attrib;
        self.push(|| Call::Attrib(buffer, a));
    }

    fn index_buffer_bind(&mut self, buffer: u32) {
        self.push(|| Call::IndexBuffer(buffer));
    }

    fn texture_create_staged(&mut self, upload: &TextureUpload) -> u32 {
        let u = *upload;
        self.push(|| Call::TextureCreate(u));
        if self.fail_texture {
            0
        } else {
            self.handle()
        }
    }

    fn texture_bind(&mut self, unit: u32, texture: u32) {
        self.push(|| Call::TextureBind(unit, texture));
    }

    fn texture_delete(&mut self, texture: u32) {
        self.push(|| Call::TextureDelete(texture));
    }

    fn enable(&mut self, cap: u32) {
        self.push(|| Call::Enable(cap));
    }

    fn disable(&mut self, cap: u32) {
        self.push(|| Call::Disable(cap));
    }

    fn front_face(&mut self, mode: u32) {
        self.push(|| Call::FrontFace(mode));
    }

    fn depth_mask(&mut self, write: bool) {
        self.push(|| Call::DepthMask(write));
    }

    fn blend_func_separate(&mut self, _: u32, _: u32, _: u32, _: u32) {
        self.push(|| Call::BlendFunc);
    }

    fn viewport(&mut self, _x: i32, _y: i32, width: u32, height: u32) {
        self.push(|| Call::Viewport(width, height));
    }

    fn clear_color(&mut self, r: f32, g: f32, b: f32, a: f32) {
        self.push(|| Call::ClearColor([r, g, b, a]));
    }

    fn clear(&mut self, mask: u32) {
        self.push(|| Call::Clear(mask));
    }

    fn draw_elements(&mut self, _mode: u32, count: u32, ty: u32, _offset: u32) {
        self.push(|| Call::Draw { count, ty });
    }

    fn invalidate_depth(&mut self) {
        self.push(|| Call::InvalidateDepth);
    }

    fn flush(&mut self) {
        self.push(|| Call::Flush);
    }

    fn query_create(&mut self) -> u32 {
        let h = self.handle();
        self.push(|| Call::QueryCreate(h));
        h
    }

    fn query_delete(&mut self, query: u32) {
        self.push(|| Call::QueryDelete(query));
    }

    fn query_begin(&mut self, query: u32) {
        self.push(|| Call::QueryBegin(query));
    }

    fn query_end(&mut self) {
        self.push(|| Call::QueryEnd);
    }

    fn query_available(&mut self, _query: u32) -> bool {
        self.query_ready
    }

    fn query_result_ns(&mut self, _query: u32) -> f64 {
        self.query_ms * 1e6
    }

    fn gpu_disjoint(&mut self) -> bool {
        self.push(|| Call::Disjoint);
        self.disjoint
    }

    fn now_ms(&mut self) -> f64 {
        self.clock_ms += 0.25;
        self.clock_ms
    }

    fn log(&mut self, _level: LogLevel, _message: &str) {}
}

thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

/// Counts allocations per thread, so tests running in parallel do not disturb each other.
struct CountingAllocator;

fn count_one() {
    // `try_with`: the thread-local may already be gone while a thread shuts down.
    let _ = ALLOCATIONS.try_with(|c| c.set(c.get() + 1));
}

// SAFETY: every method forwards to the system allocator unchanged; counting touches only a
// const-initialised thread-local `Cell`, which never allocates.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_one();
        // SAFETY: same contract as ours.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count_one();
        // SAFETY: same contract as ours.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count_one();
        // SAFETY: same contract as ours.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: same contract as ours.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

/// Allocations made on this thread so far.
pub fn allocations() -> usize {
    ALLOCATIONS.with(Cell::get)
}

#[test]
fn the_counter_sees_allocations() {
    let before = allocations();
    let v: Vec<u64> = Vec::with_capacity(10);
    assert!(allocations() > before);
    drop(v);
}
