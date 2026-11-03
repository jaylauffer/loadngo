//! The renderer: meshes, textures, materials and objects behind handles; a camera; one
//! directional key light and a hemisphere ambient; and a frame that culls, sorts and draws.
//!
//! It is generic over [`Gl`], so all of it runs in host unit tests against a mock. A frame
//! allocates nothing: the draw list, the uniform data and the GPU timer's queries are sized when
//! things are created and reused every frame after.
//!
//! Data per draw lives in uniform buffers, at slots aligned to `UNIFORM_BUFFER_OFFSET_ALIGNMENT`:
//! `Frame` (camera and lights, rewritten each frame), `Object` (model and normal matrices,
//! rewritten when an object moves) and `Material`. A draw binds its object's and material's slots
//! with `bindBufferRange`.

use std::fmt;

use crate::gl::{self, Attrib, Gl, TextureUpload};
use crate::math::{self, Mat3, Mat4, Plane, Vec3};
use crate::pack::{self, frame_block, material_block, object_block, IndexFormat, UvFormat};
use crate::shaders::{self, Program, PROGRAMS, PROGRAM_COUNT};
use crate::slots::{self, Kind, Slots};
use crate::timer::GpuTimer;

/// "Unknown" in the state cache.
const NONE: u32 = u32::MAX;
/// The most vertices one mesh may have.
pub const MAX_VERTICES: usize = 1 << 24;
/// The largest drawing buffer accepted, a side: above any GPU's limit, and below 2^31, so a size
/// never reaches JavaScript as a negative number.
pub const MAX_DRAWING_BUFFER: u32 = 1 << 15;
/// The most morph targets one mesh may have (a face rig with 52 ARKit shapes and 15 visemes has 67).
pub const MAX_MORPH_TARGETS: usize = 256;
/// How far a vertex's four skin weights may sum from 1: room for 8-bit quantisation (glTF allows
/// weights stored as normalised bytes, a step of 1/255 each).
pub const WEIGHT_SUM_TOLERANCE: f32 = 0.02;

/// How a material treats alpha (glTF's `alphaMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlphaMode {
    Opaque = 0,
    /// Alpha-tested against a cutoff: drawn or discarded per pixel, no sorting.
    Mask = 1,
    /// Alpha-blended, drawn after everything opaque, back to front, without writing depth.
    Blend = 2,
}

impl AlphaMode {
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(AlphaMode::Opaque),
            1 => Some(AlphaMode::Mask),
            2 => Some(AlphaMode::Blend),
            _ => None,
        }
    }
}

/// Texture wrapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wrap {
    Repeat = 0,
    Clamp = 1,
    Mirror = 2,
}

impl Wrap {
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Wrap::Repeat),
            1 => Some(Wrap::Clamp),
            2 => Some(Wrap::Mirror),
            _ => None,
        }
    }

    fn gl(self) -> u32 {
        match self {
            Wrap::Repeat => gl::REPEAT,
            Wrap::Clamp => gl::CLAMP_TO_EDGE,
            Wrap::Mirror => gl::MIRRORED_REPEAT,
        }
    }
}

/// A mesh to upload: one glTF primitive, as three.js's loader leaves it in typed arrays.
#[derive(Clone, Copy, Debug, Default)]
pub struct MeshInput<'a> {
    /// x, y, z per vertex.
    pub positions: &'a [f32],
    /// x, y, z per vertex; computed (smooth) when absent.
    pub normals: Option<&'a [f32]>,
    /// u, v per vertex, glTF convention (origin at the image's top left).
    pub uvs: Option<&'a [f32]>,
    /// Three per triangle, counter-clockwise front faces.
    pub indices: &'a [u32],
    /// Reserved for phase 3 (skinning): four joint indices per vertex. Checked, not yet drawn.
    pub joints: Option<&'a [u16]>,
    /// Reserved for phase 3: four joint weights per vertex. Checked, not yet drawn.
    pub weights: Option<&'a [f32]>,
    /// Reserved for phase 3 (morph targets): position deltas, x, y, z per vertex, target after
    /// target. Checked, not yet drawn.
    pub morph_positions: Option<&'a [f32]>,
    pub morph_targets: usize,
}

/// Bits of a texture's `flags` across the wasm boundary: sRGB colour data, a mip chain, and the
/// wrap modes (0 repeat, 1 clamp, 2 mirror) for s in bits 2-3 and t in bits 4-5.
pub const TEXTURE_SRGB: u32 = 1;
pub const TEXTURE_MIPMAPS: u32 = 2;
const TEXTURE_FLAG_BITS: u32 = 6;

/// A texture to create from the image source staged on the JS side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextureDesc {
    pub width: u32,
    pub height: u32,
    /// Colour data (decoded from sRGB when sampled) rather than linear data.
    pub srgb: bool,
    pub mipmaps: bool,
    pub wrap_s: Wrap,
    pub wrap_t: Wrap,
}

impl TextureDesc {
    /// Decodes the flags the JS side passes; an unknown wrap mode or bit is refused, not read as
    /// repeat (phase 2 will pass glTF's and three's own constants through a mapping).
    pub fn from_flags(width: u32, height: u32, flags: u32) -> Result<Self> {
        let wrap = |bits: u32| {
            Wrap::from_u32(bits & 3).ok_or(Error::Texture(
                "wrap modes are 0 (repeat), 1 (clamp) or 2 (mirror)",
            ))
        };
        if flags >> TEXTURE_FLAG_BITS != 0 {
            return Err(Error::Texture("unknown texture flag bits"));
        }
        Ok(Self {
            width,
            height,
            srgb: flags & TEXTURE_SRGB != 0,
            mipmaps: flags & TEXTURE_MIPMAPS != 0,
            wrap_s: wrap(flags >> 2)?,
            wrap_t: wrap(flags >> 4)?,
        })
    }
}

/// A material: glTF's metallic-roughness model, cut down to what the lean shader uses.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaterialDesc {
    /// Linear RGBA; multiplies the texture.
    pub base_color: [f32; 4],
    /// A texture handle, or 0 for none.
    pub texture: u64,
    pub alpha_mode: AlphaMode,
    pub alpha_cutoff: f32,
    /// Draw back faces too (with their normals turned round), as glTF's `doubleSided`.
    pub double_sided: bool,
    pub roughness: f32,
    pub metallic: f32,
}

impl Default for MaterialDesc {
    /// three.js's `MeshStandardMaterial` defaults: white, rough, not metallic.
    fn default() -> Self {
        Self {
            base_color: [1.0; 4],
            texture: 0,
            alpha_mode: AlphaMode::Opaque,
            alpha_cutoff: 0.5,
            double_sided: false,
            roughness: 1.0,
            metallic: 0.0,
        }
    }
}

/// What a mesh became on the GPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeshInfo {
    pub vertices: u32,
    pub triangles: u32,
    pub index_format: IndexFormat,
    pub uv_format: Option<UvFormat>,
    /// Bytes per vertex in the attribute stream (the position stream is 12).
    pub attribute_stride: u32,
    pub skinned: bool,
    pub morph_targets: u32,
}

/// What the GPU and browser allow, read once at start-up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Caps {
    pub max_texture_size: u32,
    pub ubo_alignment: u32,
    pub max_uniform_block_size: u32,
    pub timer_query: bool,
}

/// Counts from the last frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameStats {
    /// Frames drawn so far.
    pub frame: u64,
    /// CPU milliseconds the frame took: the renderer's own work and every GL call it made. NaN
    /// unless CPU timing is on ([`Renderer::set_cpu_timing`]).
    pub cpu_ms: f64,
    pub draws: u32,
    pub triangles: u32,
    /// Visible objects skipped because their bounds were outside the view.
    pub culled: u32,
    pub program_binds: u32,
    pub material_binds: u32,
    /// The newest GPU time in milliseconds (NaN until one arrives), and the frame it measured.
    pub gpu_ms: f64,
    pub gpu_frame: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    NotReady,
    Released,
    Shader(&'static str),
    MissingBlock {
        program: &'static str,
        block: &'static str,
    },
    MissingSampler(&'static str),
    Layout {
        program: &'static str,
        block: &'static str,
        uniform: &'static str,
        expected: usize,
        found: i32,
    },
    Mesh(&'static str),
    Material(&'static str),
    TextureSize {
        width: u32,
        height: u32,
        max: u32,
    },
    TextureUpload,
    Texture(&'static str),
    GlObject(&'static str),
    Handle {
        kind: &'static str,
        handle: u64,
    },
    InUse {
        kind: &'static str,
        users: u32,
        by: &'static str,
    },
    Capacity(&'static str),
    Camera(&'static str),
    Light(&'static str),
    ClearColor,
    Transform,
    Viewport {
        width: u32,
        height: u32,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotReady => write!(f, "the renderer is not initialised"),
            Error::Released => write!(f, "the renderer has been released; create a new one"),
            Error::Shader(p) => write!(
                f,
                "the {p} shader program failed to compile or link (the browser console has the compiler log)"
            ),
            Error::MissingBlock { program, block } => {
                write!(f, "the {program} program has no uniform block {block}")
            }
            Error::MissingSampler(p) => write!(f, "the {p} program has no base-colour sampler"),
            Error::Layout {
                program,
                block,
                uniform,
                expected,
                found,
            } => write!(
                f,
                "the driver puts {block}.{uniform} at byte {found}, not {expected} (program {program})"
            ),
            Error::Mesh(why) => write!(f, "mesh refused: {why}"),
            Error::Material(why) => write!(f, "material refused: {why}"),
            Error::TextureSize { width, height, max } => write!(
                f,
                "texture is {width}x{height}; this GPU allows 1 to {max} pixels a side (resize it first, for example with createImageBitmap's resizeWidth)"
            ),
            Error::TextureUpload => write!(
                f,
                "the browser refused the texture upload (the console has the reason: a cross-origin image, a closed ImageBitmap or a lost context)"
            ),
            Error::Texture(why) => write!(f, "texture refused: {why}"),
            Error::GlObject(what) => write!(f, "WebGL could not create {what}"),
            Error::Handle { kind, handle } => {
                let an = |k: &str| if k.starts_with('o') { "an" } else { "a" };
                write!(f, "{handle} is not a valid {kind} handle")?;
                match Kind::of(*handle) {
                    Some(found) if found.name() != *kind => {
                        write!(f, ": it is {} {} handle", an(found.name()), found.name())
                    }
                    Some(_) => write!(
                        f,
                        ": not live in this renderer (destroyed, never created, or another renderer's)"
                    ),
                    None => Ok(()),
                }
            }
            Error::InUse { kind, users, by } => {
                write!(f, "the {kind} is still used by {users} {by}; destroy those first")
            }
            Error::Capacity(what) => write!(f, "too many {what}"),
            Error::Camera(why) => write!(f, "camera refused: {why}"),
            Error::Light(why) => write!(f, "light refused: {why}"),
            Error::ClearColor => write!(f, "clear colour refused: every component must be finite"),
            Error::Transform => write!(f, "transform refused: every element must be finite"),
            Error::Viewport { width, height } => write!(
                f,
                "a {width}x{height} drawing buffer is refused: at most {MAX_DRAWING_BUFFER} a side"
            ),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Pass {
    Opaque = 0,
    Mask = 1,
    Blend = 2,
}

struct Mesh {
    vao: u32,
    buffers: [u32; 3],
    index_count: u32,
    index_type: u32,
    bounds: pack::Sphere,
    info: MeshInfo,
    users: u32,
}

struct Texture {
    gl: u32,
    users: u32,
}

struct Material {
    program: Program,
    pass: Pass,
    texture: Option<usize>,
    texture_gl: u32,
    double_sided: bool,
    /// An untextured masked material whose constant alpha is under its cutoff.
    never_drawn: bool,
    users: u32,
}

struct Object {
    mesh: usize,
    material: usize,
    visible: bool,
    mirrored: bool,
    center: Vec3,
    radius: f32,
}

#[derive(Clone, Copy, Debug)]
struct DrawItem {
    key: u64,
    object: u32,
}

/// The sort key: opaque, then alpha-tested, then blended. Opaque and tested draws go by program,
/// then material (so each material is bound once a frame), then front to back within a material
/// (so early depth testing rejects hidden pixels before they are shaded); blended draws go back to
/// front, whatever their material. Depth is the view distance's `f32` bits, which order like the
/// numbers themselves for non-negative values.
fn draw_key(pass: Pass, program: Program, depth: f32, material: usize) -> u64 {
    let depth = if depth.is_finite() {
        depth.max(0.0)
    } else {
        f32::MAX
    };
    let d = u64::from(depth.to_bits());
    let material = material as u64 & 0xff_ffff;
    match pass {
        Pass::Blend => (2 << 62) | ((!d & 0xffff_ffff) << 24) | material,
        _ => ((pass as u64) << 62) | ((program as u64) << 56) | (material << 32) | d,
    }
}

struct Camera {
    view_proj: Mat4,
    view: Mat4,
    proj: Mat4,
    position: Vec3,
    forward: Vec3,
    planes: [Plane; 6],
    /// Field of view, near and far, when the projection follows the canvas's aspect.
    perspective: Option<(f32, f32, f32)>,
}

impl Camera {
    fn new() -> Self {
        let mut camera = Self {
            view_proj: math::IDENTITY,
            view: math::IDENTITY,
            proj: math::IDENTITY,
            position: [0.0; 3],
            forward: [0.0, 0.0, -1.0],
            planes: [[0.0, 0.0, 0.0, 1.0]; 6],
            perspective: Some((50f32.to_radians(), 0.1, 100.0)),
        };
        let view = math::look_at([0.0, 1.6, 4.0], [0.0, 1.0, 0.0], [0.0, 1.0, 0.0]);
        let proj = math::perspective(50f32.to_radians(), 1.0, 0.1, 100.0);
        // Neither matrix can fail; keep the defaults if one somehow did.
        let _ = camera.update(view, proj);
        camera
    }

    fn update(&mut self, view: Mat4, proj: Mat4) -> Result<()> {
        let inv = math::inverse(&view).ok_or(Error::Camera("the view matrix is not invertible"))?;
        self.view = view;
        self.proj = proj;
        self.view_proj = math::mul(&proj, &view);
        self.position = [inv[12], inv[13], inv[14]];
        self.forward = math::normalize([-inv[8], -inv[9], -inv[10]]).unwrap_or([0.0, 0.0, -1.0]);
        self.planes = math::frustum_planes(&self.view_proj);
        Ok(())
    }
}

struct Light {
    /// Unit vector from the scene towards the key light.
    key_dir: Vec3,
    /// Colour times intensity, linear.
    key: Vec3,
    sky: Vec3,
    ground: Vec3,
}

/// One uniform block per slot, in one buffer, with a CPU copy; dirty bytes go up once a frame.
struct UniformArena {
    buffer: u32,
    /// Bytes bound per slot: the block's size as the driver reports it.
    range: u32,
    stride: usize,
    slots: usize,
    data: Vec<u8>,
    dirty_lo: usize,
    dirty_hi: usize,
}

impl UniformArena {
    const fn new() -> Self {
        Self {
            buffer: 0,
            range: 0,
            stride: 0,
            slots: 0,
            data: Vec::new(),
            dirty_lo: usize::MAX,
            dirty_hi: 0,
        }
    }

    fn configure(&mut self, range: u32, alignment: u32) {
        self.range = range;
        self.stride = pack::align_up(range as usize, alignment as usize);
    }

    /// Makes room for `slots` slots, doubling; returns true if the GPU buffer was replaced.
    fn reserve<G: Gl>(&mut self, gl: &mut G, slots: usize) -> bool {
        if slots <= self.slots {
            return false;
        }
        let mut n = self.slots.max(16);
        while n < slots {
            n *= 2;
        }
        self.data.resize(n * self.stride, 0);
        if self.buffer != 0 {
            gl.buffer_delete(self.buffer);
        }
        self.buffer = gl.buffer_alloc(gl::UNIFORM_BUFFER, self.data.len() as u32, gl::DYNAMIC_DRAW);
        self.slots = n;
        self.dirty_lo = 0;
        self.dirty_hi = self.data.len();
        true
    }

    fn slot_mut(&mut self, slot: usize) -> &mut [u8] {
        let start = slot * self.stride;
        let end = start + self.range as usize;
        self.dirty_lo = self.dirty_lo.min(start);
        self.dirty_hi = self.dirty_hi.max(end);
        &mut self.data[start..end]
    }

    fn offset(&self, slot: usize) -> u32 {
        (slot * self.stride) as u32
    }

    fn upload<G: Gl>(&mut self, gl: &mut G) {
        if self.dirty_lo < self.dirty_hi {
            let (lo, hi) = (self.dirty_lo, self.dirty_hi);
            gl.buffer_update(
                self.buffer,
                gl::UNIFORM_BUFFER,
                lo as u32,
                &self.data[lo..hi],
            );
        }
        self.dirty_lo = usize::MAX;
        self.dirty_hi = 0;
    }

    fn release<G: Gl>(&mut self, gl: &mut G) {
        if self.buffer != 0 {
            gl.buffer_delete(self.buffer);
            self.buffer = 0;
        }
        self.slots = 0;
    }
}

/// What the renderer last told GL, so a frame sends only changes.
struct StateCache {
    program: u32,
    vao: u32,
    texture: u32,
    material: usize,
    blend: Option<bool>,
    cull: Option<bool>,
    front_cw: Option<bool>,
    depth_mask: Option<bool>,
}

impl StateCache {
    const fn unknown() -> Self {
        Self {
            program: NONE,
            vao: NONE,
            texture: NONE,
            material: usize::MAX,
            blend: None,
            cull: None,
            front_cw: None,
            depth_mask: None,
        }
    }
}

/// An object's transform, worked out once when it is placed.
struct Placement {
    normal: Mat3,
    mirrored: bool,
    center: Vec3,
    radius: f32,
}

impl Placement {
    fn new(model: &Mat4, bounds: pack::Sphere) -> Result<Self> {
        if !model.iter().all(|v| v.is_finite()) {
            return Err(Error::Transform);
        }
        // A singular transform (scaled to zero, a common way to hide something) keeps an
        // identity normal matrix: it covers no pixels anyway.
        let normal =
            math::normal_matrix(model).unwrap_or([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
        Ok(Self {
            normal,
            mirrored: math::determinant3(model) < 0.0,
            center: math::transform_point(model, bounds.center),
            radius: bounds.radius * math::max_scale(model),
        })
    }

    fn write(&self, slot: &mut [u8], model: &Mat4) {
        let n = &self.normal;
        pack::put_f32s(slot, object_block::MODEL, model);
        pack::put_f32s(
            slot,
            object_block::NORMAL,
            &[
                n[0], n[1], n[2], 0.0, n[3], n[4], n[5], 0.0, n[6], n[7], n[8], 0.0, 0.0, 0.0, 0.0,
                1.0,
            ],
        );
    }
}

fn finite(values: &[f32]) -> bool {
    values.iter().all(|v| v.is_finite())
}

/// Checks a mesh's arrays against each other; returns the vertex count.
fn validate_mesh(m: &MeshInput<'_>) -> Result<usize> {
    let p = m.positions;
    if p.is_empty() || !p.len().is_multiple_of(3) {
        return Err(Error::Mesh(
            "positions must hold x, y, z for at least one vertex",
        ));
    }
    let vertices = p.len() / 3;
    if vertices > MAX_VERTICES {
        return Err(Error::Mesh("more than 16,777,216 vertices"));
    }
    if !finite(p) {
        return Err(Error::Mesh("positions must be finite"));
    }
    if let Some(n) = m.normals {
        if n.len() != p.len() {
            return Err(Error::Mesh("normals must hold x, y, z for every vertex"));
        }
        if !finite(n) {
            return Err(Error::Mesh("normals must be finite"));
        }
    }
    if let Some(uvs) = m.uvs {
        if uvs.len() != vertices * 2 {
            return Err(Error::Mesh("uvs must hold u, v for every vertex"));
        }
        if !finite(uvs) {
            return Err(Error::Mesh("uvs must be finite"));
        }
    }
    if m.indices.is_empty()
        || !m.indices.len().is_multiple_of(3)
        || m.indices.len() > u32::MAX as usize
    {
        return Err(Error::Mesh("indices must list whole triangles"));
    }
    match (m.joints, m.weights) {
        (None, None) => {}
        (Some(j), Some(w)) => {
            if j.len() != vertices * 4 || w.len() != vertices * 4 {
                return Err(Error::Mesh(
                    "joints and weights need four of each per vertex",
                ));
            }
            // Joint indices are checked against the skeleton in phase 3, when skins exist.
            if !w.iter().all(|x| (0.0..=1.0).contains(x)) {
                return Err(Error::Mesh("weights must lie between 0 and 1"));
            }
            if w.as_chunks::<4>()
                .0
                .iter()
                .any(|q| (q.iter().sum::<f32>() - 1.0).abs() > WEIGHT_SUM_TOLERANCE)
            {
                return Err(Error::Mesh("each vertex's four weights must sum to 1"));
            }
        }
        _ => return Err(Error::Mesh("joints and weights come together")),
    }
    match m.morph_positions {
        None if m.morph_targets != 0 => {
            return Err(Error::Mesh("morph target count given without deltas"));
        }
        None => {}
        Some(d) => {
            if m.morph_targets == 0 || m.morph_targets > MAX_MORPH_TARGETS {
                return Err(Error::Mesh("a mesh may have 1 to 256 morph targets"));
            }
            // Checked: on wasm32 an unchecked product wraps, and a wrong count could pass.
            let expected = m
                .morph_targets
                .checked_mul(vertices)
                .and_then(|n| n.checked_mul(3));
            if expected != Some(d.len()) {
                return Err(Error::Mesh(
                    "morph deltas must hold x, y, z per vertex for each target",
                ));
            }
            if !finite(d) {
                return Err(Error::Mesh("morph deltas must be finite"));
            }
        }
    }
    Ok(vertices)
}

pub struct Renderer<G: Gl> {
    gl: G,
    ready: bool,
    released: bool,
    cpu_timing: bool,
    caps: Caps,
    programs: [u32; PROGRAM_COUNT],
    meshes: Slots<Mesh>,
    textures: Slots<Texture>,
    materials: Slots<Material>,
    objects: Slots<Object>,
    frame_ubo: u32,
    frame_data: [u8; frame_block::SIZE],
    object_ubo: UniformArena,
    material_ubo: UniformArena,
    camera: Camera,
    light: Light,
    clear_srgb: [f32; 3],
    clear_dirty: bool,
    width: u32,
    height: u32,
    viewport_dirty: bool,
    draws: Vec<DrawItem>,
    scratch_attributes: Vec<u8>,
    scratch_indices: Vec<u8>,
    scratch_normals: Vec<f32>,
    timer: GpuTimer,
    state: StateCache,
    stats: FrameStats,
}

impl<G: Gl> Renderer<G> {
    /// A renderer that has made no GL calls yet; [`Renderer::init`] sets it up. Its handles carry
    /// serial 0; see [`Renderer::with_serial`].
    pub fn new(gl: G) -> Self {
        Self::build(gl, 0)
    }

    /// As [`Renderer::new`], with the serial every handle it issues carries (below
    /// [`slots::SERIALS`], else `None`). Renderers with different serials refuse each other's
    /// handles and never issue the same one.
    pub fn with_serial(gl: G, serial: u32) -> Option<Self> {
        (serial < slots::SERIALS).then(|| Self::build(gl, serial))
    }

    fn build(gl: G, serial: u32) -> Self {
        Self {
            gl,
            ready: false,
            released: false,
            cpu_timing: false,
            caps: Caps::default(),
            programs: [0; PROGRAM_COUNT],
            meshes: Slots::new(Kind::Mesh, serial),
            textures: Slots::new(Kind::Texture, serial),
            materials: Slots::new(Kind::Material, serial),
            objects: Slots::new(Kind::Object, serial),
            frame_ubo: 0,
            frame_data: [0; frame_block::SIZE],
            object_ubo: UniformArena::new(),
            material_ubo: UniformArena::new(),
            camera: Camera::new(),
            light: Light {
                key_dir: math::normalize([0.4, 1.0, 0.6]).unwrap_or([0.0, 1.0, 0.0]),
                key: [1.0, 1.0, 1.0],
                sky: [0.3, 0.33, 0.38],
                ground: [0.1, 0.09, 0.08],
            },
            clear_srgb: [0.0; 3],
            clear_dirty: true,
            width: 0,
            height: 0,
            viewport_dirty: true,
            draws: Vec::new(),
            scratch_attributes: Vec::new(),
            scratch_indices: Vec::new(),
            scratch_normals: Vec::new(),
            timer: GpuTimer::new(),
            state: StateCache::unknown(),
            stats: FrameStats {
                gpu_ms: f64::NAN,
                cpu_ms: f64::NAN,
                ..FrameStats::default()
            },
        }
    }

    pub fn gl(&self) -> &G {
        &self.gl
    }

    pub fn gl_mut(&mut self) -> &mut G {
        &mut self.gl
    }

    pub fn caps(&self) -> Caps {
        self.caps
    }

    pub fn stats(&self) -> &FrameStats {
        &self.stats
    }

    pub fn is_ready(&self) -> bool {
        self.ready
    }

    /// Times each frame on the CPU (two clock readings a frame). Off by default: in a browser each
    /// reading is a number V8 boxes, the only allocation a frame would otherwise make; a perf
    /// readout turns it on.
    pub fn set_cpu_timing(&mut self, on: bool) {
        self.cpu_timing = on;
        if !on {
            self.stats.cpu_ms = f64::NAN;
        }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn require_ready(&self) -> Result<()> {
        if self.ready {
            Ok(())
        } else {
            Err(Error::NotReady)
        }
    }

    /// Reads the GPU's limits, compiles the programs, checks their uniform layouts against the
    /// driver's, creates the uniform buffers and sets the fixed state.
    pub fn init(&mut self, width: u32, height: u32) -> Result<()> {
        if self.released {
            return Err(Error::Released);
        }
        if self.ready {
            return Ok(());
        }
        if width > MAX_DRAWING_BUFFER || height > MAX_DRAWING_BUFFER {
            return Err(Error::Viewport { width, height });
        }
        let positive = |v: i32| u32::try_from(v).unwrap_or(0);
        self.caps = Caps {
            max_texture_size: positive(self.gl.get_integer(gl::MAX_TEXTURE_SIZE)),
            ubo_alignment: positive(self.gl.get_integer(gl::UNIFORM_BUFFER_OFFSET_ALIGNMENT))
                .max(1),
            max_uniform_block_size: positive(self.gl.get_integer(gl::MAX_UNIFORM_BLOCK_SIZE)),
            timer_query: false,
        };
        let ranges = self.compile_programs()?;

        self.frame_ubo = self
            .gl
            .buffer_alloc(gl::UNIFORM_BUFFER, ranges[0], gl::DYNAMIC_DRAW);
        if self.frame_ubo == 0 {
            return Err(Error::GlObject("the frame uniform buffer"));
        }
        self.gl.bind_buffer_range(
            gl::UNIFORM_BUFFER,
            shaders::BINDING_FRAME,
            self.frame_ubo,
            0,
            ranges[0],
        );
        self.object_ubo
            .configure(ranges[1], self.caps.ubo_alignment);
        self.material_ubo
            .configure(ranges[2], self.caps.ubo_alignment);
        self.object_ubo.reserve(&mut self.gl, 16);
        self.material_ubo.reserve(&mut self.gl, 16);

        self.gl.enable(gl::DEPTH_TEST);
        self.gl.blend_func_separate(
            gl::SRC_ALPHA,
            gl::ONE_MINUS_SRC_ALPHA,
            gl::ONE,
            gl::ONE_MINUS_SRC_ALPHA,
        );
        self.timer.init(&mut self.gl);
        self.caps.timer_query = self.timer.supported();
        self.ready = true;
        self.resize(width, height)
    }

    /// Compiles every program, then checks each; returns the bound size of each block.
    fn compile_programs(&mut self) -> Result<[u32; 3]> {
        for p in PROGRAMS {
            let vertex = shaders::vertex_source(p);
            let fragment = shaders::fragment_source(p);
            self.programs[p as usize] = self.gl.program_create(&vertex, &fragment);
        }
        let mut ranges = [0u32; 3];
        for p in PROGRAMS {
            let h = self.programs[p as usize];
            if h == 0 || !self.gl.program_check(h) {
                return Err(Error::Shader(p.name()));
            }
            for (block, binding) in shaders::BLOCK_BINDINGS {
                if !self.gl.program_bind_block(h, block, binding) {
                    return Err(Error::MissingBlock {
                        program: p.name(),
                        block,
                    });
                }
            }
            if p.textured() {
                let bound = self.gl.program_bind_sampler(
                    h,
                    shaders::SAMPLER_BASE_COLOR,
                    shaders::UNIT_BASE_COLOR,
                );
                self.state.program = h;
                if !bound {
                    return Err(Error::MissingSampler(p.name()));
                }
            }
            for (i, layout) in pack::BLOCK_LAYOUTS.iter().enumerate() {
                let block = layout.name;
                let reported = self.gl.program_block_size(h, block);
                if reported < 0 {
                    return Err(Error::MissingBlock {
                        program: p.name(),
                        block,
                    });
                }
                for (uniform, expected) in layout.uniforms {
                    let found = self.gl.program_uniform_offset(h, uniform);
                    // -1: optimised out of this program, which std140 should not allow, but
                    // a missing member cannot be misread either.
                    if found >= 0 && found as usize != *expected {
                        return Err(Error::Layout {
                            program: p.name(),
                            block,
                            uniform,
                            expected: *expected,
                            found,
                        });
                    }
                }
                ranges[i] = ranges[i].max(reported as u32).max(layout.size as u32);
            }
        }
        Ok(ranges)
    }

    /// Sets the drawing-buffer size (the canvas's `width` and `height`, in pixels). A size over
    /// [`MAX_DRAWING_BUFFER`] is refused and the old one kept.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        if width > MAX_DRAWING_BUFFER || height > MAX_DRAWING_BUFFER {
            return Err(Error::Viewport { width, height });
        }
        if (width, height) != (self.width, self.height) {
            self.width = width;
            self.height = height;
            self.viewport_dirty = true;
        }
        if let Some((fov_y, near, far)) = self.camera.perspective {
            let proj = math::perspective(fov_y, self.aspect(), near, far);
            let view = self.camera.view;
            // The view was invertible when it was set.
            let _ = self.camera.update(view, proj);
        }
        Ok(())
    }

    fn aspect(&self) -> f32 {
        if self.width == 0 || self.height == 0 {
            1.0
        } else {
            self.width as f32 / self.height as f32
        }
    }

    fn bind_vao(&mut self, vao: u32) {
        if self.state.vao != vao {
            self.gl.vertex_array_bind(vao);
            self.state.vao = vao;
        }
    }

    /// Uploads a mesh: positions in one stream, packed normals and texture coordinates in a
    /// second, indices narrowed to 16 bits where they fit. Every check comes before the first GL
    /// call, so a refused mesh leaves nothing behind.
    pub fn create_mesh(&mut self, input: &MeshInput<'_>) -> Result<u64> {
        self.require_ready()?;
        let vertices = validate_mesh(input)?;
        let mut indices = std::mem::take(&mut self.scratch_indices);
        let mut attributes = std::mem::take(&mut self.scratch_attributes);
        let mut normals = std::mem::take(&mut self.scratch_normals);
        let result = self.build_mesh(input, vertices, &mut indices, &mut attributes, &mut normals);
        self.scratch_indices = indices;
        self.scratch_attributes = attributes;
        self.scratch_normals = normals;
        result
    }

    fn build_mesh(
        &mut self,
        input: &MeshInput<'_>,
        vertices: usize,
        indices: &mut Vec<u8>,
        attributes: &mut Vec<u8>,
        computed: &mut Vec<f32>,
    ) -> Result<u64> {
        let index_format = pack::pack_indices(input.indices, vertices, indices)
            .ok_or(Error::Mesh("an index is past the last vertex"))?;
        let normals = match input.normals {
            Some(n) => n,
            None => {
                pack::compute_normals(input.positions, input.indices, computed);
                &computed[..]
            }
        };
        let layout = pack::pack_attributes(normals, input.uvs, attributes);

        // The index buffer must not land in whatever vertex array is bound.
        self.bind_vao(0);
        let gl = &mut self.gl;
        let positions = gl.buffer_create(
            gl::ARRAY_BUFFER,
            pack::f32_bytes(input.positions),
            gl::STATIC_DRAW,
        );
        let attribute_buffer = gl.buffer_create(gl::ARRAY_BUFFER, attributes, gl::STATIC_DRAW);
        let index_buffer = gl.buffer_create(gl::ELEMENT_ARRAY_BUFFER, indices, gl::STATIC_DRAW);
        let vao = gl.vertex_array_create();
        let buffers = [positions, attribute_buffer, index_buffer];
        if vao == 0 || buffers.contains(&0) {
            self.delete_mesh_objects(vao, buffers);
            return Err(Error::GlObject("a mesh's buffers"));
        }
        self.bind_vao(vao);
        let stride = layout.stride as u32;
        self.gl.vertex_attrib(
            positions,
            &Attrib {
                location: shaders::ATTRIB_POSITION,
                size: 3,
                ty: gl::FLOAT,
                normalized: false,
                stride: 12,
                offset: 0,
            },
        );
        self.gl.vertex_attrib(
            attribute_buffer,
            &Attrib {
                location: shaders::ATTRIB_NORMAL,
                size: 4,
                ty: gl::INT_2_10_10_10_REV,
                normalized: true,
                stride,
                offset: layout.normal_offset as u32,
            },
        );
        if let Some((format, offset)) = layout.uv {
            let (ty, normalized) = format.gl_type();
            self.gl.vertex_attrib(
                attribute_buffer,
                &Attrib {
                    location: shaders::ATTRIB_UV,
                    size: 2,
                    ty,
                    normalized,
                    stride,
                    offset: offset as u32,
                },
            );
        }
        self.gl.index_buffer_bind(index_buffer);
        self.bind_vao(0);

        let info = MeshInfo {
            vertices: vertices as u32,
            triangles: (input.indices.len() / 3) as u32,
            index_format,
            uv_format: layout.uv.map(|(format, _)| format),
            attribute_stride: stride,
            skinned: input.joints.is_some(),
            morph_targets: input.morph_targets as u32,
        };
        let mesh = Mesh {
            vao,
            buffers,
            index_count: input.indices.len() as u32,
            index_type: index_format.gl_type(),
            bounds: pack::bounding_sphere(input.positions),
            info,
            users: 0,
        };
        match self.meshes.insert(mesh) {
            Some((handle, _)) => Ok(handle),
            None => {
                self.delete_mesh_objects(vao, buffers);
                Err(Error::Capacity("meshes"))
            }
        }
    }

    fn delete_mesh_objects(&mut self, vao: u32, buffers: [u32; 3]) {
        if vao != 0 {
            if self.state.vao == vao {
                self.state.vao = NONE;
            }
            self.gl.vertex_array_delete(vao);
        }
        for b in buffers {
            if b != 0 {
                self.gl.buffer_delete(b);
            }
        }
    }

    pub fn mesh_info(&self, mesh: u64) -> Option<MeshInfo> {
        self.meshes.get(mesh).map(|m| m.info)
    }

    pub fn destroy_mesh(&mut self, mesh: u64) -> Result<()> {
        let m = self.meshes.get(mesh).ok_or(Error::Handle {
            kind: "mesh",
            handle: mesh,
        })?;
        if m.users > 0 {
            return Err(Error::InUse {
                kind: "mesh",
                users: m.users,
                by: "objects",
            });
        }
        if let Some(m) = self.meshes.remove(mesh) {
            self.delete_mesh_objects(m.vao, m.buffers);
        }
        Ok(())
    }

    /// Creates a texture from the image source the JS side has staged (an `ImageBitmap`,
    /// `HTMLImageElement` or canvas), so the browser has decoded it, WebP and JPEG included.
    pub fn create_texture(&mut self, desc: &TextureDesc) -> Result<u64> {
        self.require_ready()?;
        let max = self.caps.max_texture_size;
        if desc.width == 0 || desc.height == 0 || desc.width > max || desc.height > max {
            return Err(Error::TextureSize {
                width: desc.width,
                height: desc.height,
                max,
            });
        }
        let levels = if desc.mipmaps {
            32 - desc.width.max(desc.height).leading_zeros()
        } else {
            1
        };
        let upload = TextureUpload {
            internal_format: if desc.srgb {
                gl::SRGB8_ALPHA8
            } else {
                gl::RGBA8
            },
            width: desc.width,
            height: desc.height,
            levels,
            wrap_s: desc.wrap_s.gl(),
            wrap_t: desc.wrap_t.gl(),
            min_filter: if desc.mipmaps {
                gl::LINEAR_MIPMAP_LINEAR
            } else {
                gl::LINEAR
            },
            mag_filter: gl::LINEAR,
        };
        let texture = self.gl.texture_create_staged(&upload);
        // The shim leaves the new texture bound to unit 0.
        self.state.texture = if texture == 0 { NONE } else { texture };
        if texture == 0 {
            return Err(Error::TextureUpload);
        }
        match self.textures.insert(Texture {
            gl: texture,
            users: 0,
        }) {
            Some((handle, _)) => Ok(handle),
            None => {
                self.gl.texture_delete(texture);
                self.state.texture = NONE;
                Err(Error::Capacity("textures"))
            }
        }
    }

    pub fn destroy_texture(&mut self, texture: u64) -> Result<()> {
        let t = self.textures.get(texture).ok_or(Error::Handle {
            kind: "texture",
            handle: texture,
        })?;
        if t.users > 0 {
            return Err(Error::InUse {
                kind: "texture",
                users: t.users,
                by: "materials",
            });
        }
        if let Some(t) = self.textures.remove(texture) {
            if self.state.texture == t.gl {
                self.state.texture = NONE;
            }
            self.gl.texture_delete(t.gl);
        }
        Ok(())
    }

    pub fn create_material(&mut self, desc: &MaterialDesc) -> Result<u64> {
        self.require_ready()?;
        if !finite(&desc.base_color) || !finite(&[desc.alpha_cutoff, desc.roughness, desc.metallic])
        {
            return Err(Error::Material("every value must be finite"));
        }
        let texture = match desc.texture {
            0 => None,
            h => Some(self.textures.index_of(h).ok_or(Error::Handle {
                kind: "texture",
                handle: h,
            })?),
        };
        let texture_gl = texture
            .and_then(|i| self.textures.at(i))
            .map_or(0, |t| t.gl);
        let textured = texture.is_some();
        let cutoff = desc.alpha_cutoff.clamp(0.0, 1.0);
        let (program, pass) = match (desc.alpha_mode, textured) {
            (AlphaMode::Opaque | AlphaMode::Mask, false) => (Program::Plain, Pass::Opaque),
            (AlphaMode::Opaque, true) => (Program::Textured, Pass::Opaque),
            (AlphaMode::Mask, true) => (Program::TexturedMask, Pass::Mask),
            (AlphaMode::Blend, false) => (Program::Plain, Pass::Blend),
            (AlphaMode::Blend, true) => (Program::Textured, Pass::Blend),
        };
        let never_drawn =
            desc.alpha_mode == AlphaMode::Mask && !textured && desc.base_color[3] < cutoff;
        let (handle, index) = self
            .materials
            .insert(Material {
                program,
                pass,
                texture,
                texture_gl,
                double_sided: desc.double_sided,
                never_drawn,
                users: 0,
            })
            .ok_or(Error::Capacity("materials"))?;
        if self
            .material_ubo
            .reserve(&mut self.gl, self.materials.slot_count())
        {
            self.state.material = usize::MAX;
        }
        let (shininess, norm) = shaders::blinn_phong(desc.roughness);
        let slot = self.material_ubo.slot_mut(index);
        let c = desc.base_color;
        pack::put_f32s(
            slot,
            material_block::BASE_COLOR,
            &[
                c[0].max(0.0),
                c[1].max(0.0),
                c[2].max(0.0),
                c[3].clamp(0.0, 1.0),
            ],
        );
        pack::put_f32s(
            slot,
            material_block::PARAMS,
            &[shininess, desc.metallic.clamp(0.0, 1.0), cutoff, norm],
        );
        if let Some(t) = texture.and_then(|i| self.textures.at_mut(i)) {
            t.users += 1;
        }
        Ok(handle)
    }

    pub fn destroy_material(&mut self, material: u64) -> Result<()> {
        let index = self.materials.index_of(material).ok_or(Error::Handle {
            kind: "material",
            handle: material,
        })?;
        let users = self.materials.at(index).map_or(0, |m| m.users);
        if users > 0 {
            return Err(Error::InUse {
                kind: "material",
                users,
                by: "objects",
            });
        }
        if let Some(m) = self.materials.remove(material) {
            if let Some(t) = m.texture.and_then(|i| self.textures.at_mut(i)) {
                t.users = t.users.saturating_sub(1);
            }
            if self.state.material == index {
                self.state.material = usize::MAX;
            }
        }
        Ok(())
    }

    /// Places a mesh with a material in the scene.
    pub fn create_object(&mut self, mesh: u64, material: u64, model: &Mat4) -> Result<u64> {
        self.require_ready()?;
        let mesh_index = self.meshes.index_of(mesh).ok_or(Error::Handle {
            kind: "mesh",
            handle: mesh,
        })?;
        let material_index = self.materials.index_of(material).ok_or(Error::Handle {
            kind: "material",
            handle: material,
        })?;
        let bounds = self
            .meshes
            .at(mesh_index)
            .map(|m| m.bounds)
            .ok_or(Error::Handle {
                kind: "mesh",
                handle: mesh,
            })?;
        let placed = Placement::new(model, bounds)?;
        let (handle, index) = self
            .objects
            .insert(Object {
                mesh: mesh_index,
                material: material_index,
                visible: true,
                mirrored: placed.mirrored,
                center: placed.center,
                radius: placed.radius,
            })
            .ok_or(Error::Capacity("objects"))?;
        self.object_ubo
            .reserve(&mut self.gl, self.objects.slot_count());
        placed.write(self.object_ubo.slot_mut(index), model);
        if let Some(m) = self.meshes.at_mut(mesh_index) {
            m.users += 1;
        }
        if let Some(m) = self.materials.at_mut(material_index) {
            m.users += 1;
        }
        // The draw list never grows during a frame.
        let live = self.objects.len();
        if self.draws.capacity() < live {
            self.draws.reserve(live - self.draws.len());
        }
        Ok(handle)
    }

    /// Moves an object (a column-major model matrix, as three.js's `matrixWorld.elements`).
    pub fn set_transform(&mut self, object: u64, model: &Mat4) -> Result<()> {
        let index = self.objects.index_of(object).ok_or(Error::Handle {
            kind: "object",
            handle: object,
        })?;
        let mesh = self.objects.at(index).map_or(usize::MAX, |o| o.mesh);
        let bounds = self
            .meshes
            .at(mesh)
            .map(|m| m.bounds)
            .ok_or(Error::Handle {
                kind: "object",
                handle: object,
            })?;
        let placed = Placement::new(model, bounds)?;
        if let Some(o) = self.objects.at_mut(index) {
            o.mirrored = placed.mirrored;
            o.center = placed.center;
            o.radius = placed.radius;
        }
        placed.write(self.object_ubo.slot_mut(index), model);
        Ok(())
    }

    pub fn set_visible(&mut self, object: u64, visible: bool) -> Result<()> {
        let o = self.objects.get_mut(object).ok_or(Error::Handle {
            kind: "object",
            handle: object,
        })?;
        o.visible = visible;
        Ok(())
    }

    pub fn destroy_object(&mut self, object: u64) -> Result<()> {
        let o = self.objects.remove(object).ok_or(Error::Handle {
            kind: "object",
            handle: object,
        })?;
        if let Some(m) = self.meshes.at_mut(o.mesh) {
            m.users = m.users.saturating_sub(1);
        }
        if let Some(m) = self.materials.at_mut(o.material) {
            m.users = m.users.saturating_sub(1);
        }
        Ok(())
    }

    /// Sets the camera from a view matrix and a projection, as three.js's camera holds them
    /// (`matrixWorldInverse` and `projectionMatrix`). The projection is used as given.
    pub fn set_camera(&mut self, view: &Mat4, proj: &Mat4) -> Result<()> {
        if !finite(view) || !finite(proj) {
            return Err(Error::Camera("every element must be finite"));
        }
        self.camera.update(*view, *proj)?;
        self.camera.perspective = None;
        Ok(())
    }

    /// Points a perspective camera from `eye` at `target`. The projection follows the canvas's
    /// aspect from then on, through [`Renderer::resize`].
    pub fn set_camera_look_at(
        &mut self,
        eye: Vec3,
        target: Vec3,
        up: Vec3,
        fov_y: f32,
        near: f32,
        far: f32,
    ) -> Result<()> {
        if !finite(&eye) || !finite(&target) || !finite(&up) {
            return Err(Error::Camera("eye, target and up must be finite"));
        }
        if !(fov_y > 0.0 && fov_y < std::f32::consts::PI) {
            return Err(Error::Camera(
                "the field of view must be between 0 and pi radians",
            ));
        }
        if !(near > 0.0 && far > near && far.is_finite()) {
            return Err(Error::Camera("near and far must satisfy 0 < near < far"));
        }
        let view = math::look_at(eye, target, up);
        let proj = math::perspective(fov_y, self.aspect(), near, far);
        self.camera.update(view, proj)?;
        self.camera.perspective = Some((fov_y, near, far));
        Ok(())
    }

    /// The directional key light: `toward` points from the scene to the light (three.js:
    /// `light.position` minus `light.target.position`); colour is linear.
    pub fn set_key_light(&mut self, toward: Vec3, color: Vec3, intensity: f32) -> Result<()> {
        let dir = math::normalize(toward).ok_or(Error::Light("the direction must be non-zero"))?;
        if !finite(&color) || !intensity.is_finite() {
            return Err(Error::Light("colour and intensity must be finite"));
        }
        self.light.key_dir = dir;
        self.light.key = math::scale(color, intensity.max(0.0));
        Ok(())
    }

    /// The hemisphere ambient: `sky` lights upward-facing surfaces, `ground` downward ones, and
    /// sideways ones get the mix (three.js's `HemisphereLight`).
    pub fn set_ambient(&mut self, sky: Vec3, ground: Vec3, intensity: f32) -> Result<()> {
        if !finite(&sky) || !finite(&ground) || !intensity.is_finite() {
            return Err(Error::Light("colours and intensity must be finite"));
        }
        let k = intensity.max(0.0);
        self.light.sky = math::scale(sky, k);
        self.light.ground = math::scale(ground, k);
        Ok(())
    }

    /// The background, as a linear colour like every other colour here.
    pub fn set_clear_color(&mut self, color: Vec3) -> Result<()> {
        if !finite(&color) {
            return Err(Error::ClearColor);
        }
        let srgb = color.map(pack::linear_to_srgb);
        if srgb != self.clear_srgb {
            self.clear_srgb = srgb;
            self.clear_dirty = true;
        }
        Ok(())
    }

    fn set_blend(&mut self, on: bool) {
        if self.state.blend != Some(on) {
            if on {
                self.gl.enable(gl::BLEND);
            } else {
                self.gl.disable(gl::BLEND);
            }
            self.state.blend = Some(on);
        }
    }

    fn set_cull(&mut self, on: bool) {
        if self.state.cull != Some(on) {
            if on {
                self.gl.enable(gl::CULL_FACE);
            } else {
                self.gl.disable(gl::CULL_FACE);
            }
            self.state.cull = Some(on);
        }
    }

    fn set_front_face_cw(&mut self, cw: bool) {
        if self.state.front_cw != Some(cw) {
            self.gl.front_face(if cw { gl::CW } else { gl::CCW });
            self.state.front_cw = Some(cw);
        }
    }

    fn set_depth_mask(&mut self, write: bool) {
        if self.state.depth_mask != Some(write) {
            self.gl.depth_mask(write);
            self.state.depth_mask = Some(write);
        }
    }

    /// Draws a frame. Call it from `requestAnimationFrame`; it neither waits nor allocates.
    pub fn frame(&mut self) {
        if !self.ready || self.width == 0 || self.height == 0 {
            return;
        }
        let start = if self.cpu_timing {
            self.gl.now_ms()
        } else {
            f64::NAN
        };
        self.stats.frame += 1;
        let frame = self.stats.frame;
        self.timer.collect(&mut self.gl);
        self.timer.begin(&mut self.gl, frame);

        if self.viewport_dirty {
            self.gl.viewport(0, 0, self.width, self.height);
            self.viewport_dirty = false;
        }
        if self.clear_dirty {
            let c = self.clear_srgb;
            self.gl.clear_color(c[0], c[1], c[2], 1.0);
            self.clear_dirty = false;
        }
        // Clearing every attachment lets a tile-based GPU skip loading last frame's contents;
        // depth writes must be on for the depth clear to happen.
        self.set_depth_mask(true);
        self.gl.clear(gl::COLOR_BUFFER_BIT | gl::DEPTH_BUFFER_BIT);

        self.write_frame_block();
        self.gl
            .buffer_update(self.frame_ubo, gl::UNIFORM_BUFFER, 0, &self.frame_data);
        self.object_ubo.upload(&mut self.gl);
        self.material_ubo.upload(&mut self.gl);

        self.build_draw_list();
        self.draws.sort_unstable_by_key(|d| (d.key, d.object));
        self.issue_draws();

        self.gl.invalidate_depth();
        self.timer.end(&mut self.gl);
        let (ms, at) = self.timer.last();
        self.stats.gpu_ms = ms;
        self.stats.gpu_frame = at;
        if self.cpu_timing {
            self.stats.cpu_ms = self.gl.now_ms() - start;
        }
    }

    fn write_frame_block(&mut self) {
        let b = &mut self.frame_data;
        let c = &self.camera;
        let l = &self.light;
        pack::put_f32s(b, frame_block::VIEW_PROJ, &c.view_proj);
        pack::put_f32s(
            b,
            frame_block::CAMERA_POS,
            &[c.position[0], c.position[1], c.position[2], 1.0],
        );
        pack::put_f32s(
            b,
            frame_block::KEY_DIR,
            &[l.key_dir[0], l.key_dir[1], l.key_dir[2], 0.0],
        );
        pack::put_f32s(
            b,
            frame_block::KEY_COLOR,
            &[l.key[0], l.key[1], l.key[2], 1.0],
        );
        pack::put_f32s(b, frame_block::SKY, &[l.sky[0], l.sky[1], l.sky[2], 1.0]);
        pack::put_f32s(
            b,
            frame_block::GROUND,
            &[l.ground[0], l.ground[1], l.ground[2], 1.0],
        );
    }

    fn build_draw_list(&mut self) {
        self.draws.clear();
        let mut culled = 0;
        let camera = &self.camera;
        for (index, object) in self.objects.iter() {
            if !object.visible {
                continue;
            }
            let Some(material) = self.materials.at(object.material) else {
                continue;
            };
            if material.never_drawn {
                continue;
            }
            if !math::sphere_visible(&camera.planes, object.center, object.radius) {
                culled += 1;
                continue;
            }
            let depth = math::dot(math::sub(object.center, camera.position), camera.forward);
            self.draws.push(DrawItem {
                key: draw_key(material.pass, material.program, depth, object.material),
                object: index as u32,
            });
        }
        self.stats.culled = culled;
    }

    fn issue_draws(&mut self) {
        let (mut draws, mut triangles, mut program_binds, mut material_binds) = (0, 0, 0, 0);
        for i in 0..self.draws.len() {
            let object_index = self.draws[i].object as usize;
            let Some(object) = self.objects.at(object_index) else {
                continue;
            };
            let (mirrored, mesh_index, material_index) =
                (object.mirrored, object.mesh, object.material);
            let (Some(mesh), Some(material)) = (
                self.meshes.at(mesh_index),
                self.materials.at(material_index),
            ) else {
                continue;
            };
            let (vao, count, index_type, mesh_triangles) = (
                mesh.vao,
                mesh.index_count,
                mesh.index_type,
                mesh.info.triangles,
            );
            let program = self.programs[material.program as usize];
            let (pass, texture, double_sided) =
                (material.pass, material.texture_gl, material.double_sided);

            if self.state.program != program {
                self.gl.use_program(program);
                self.state.program = program;
                program_binds += 1;
            }
            if self.state.material != material_index {
                self.gl.bind_buffer_range(
                    gl::UNIFORM_BUFFER,
                    shaders::BINDING_MATERIAL,
                    self.material_ubo.buffer,
                    self.material_ubo.offset(material_index),
                    self.material_ubo.range,
                );
                self.state.material = material_index;
                material_binds += 1;
            }
            if texture != 0 && self.state.texture != texture {
                self.gl.texture_bind(shaders::UNIT_BASE_COLOR, texture);
                self.state.texture = texture;
            }
            let blend = pass == Pass::Blend;
            self.set_blend(blend);
            self.set_depth_mask(!blend);
            self.set_cull(!double_sided);
            self.set_front_face_cw(mirrored);
            self.gl.bind_buffer_range(
                gl::UNIFORM_BUFFER,
                shaders::BINDING_OBJECT,
                self.object_ubo.buffer,
                self.object_ubo.offset(object_index),
                self.object_ubo.range,
            );
            self.bind_vao(vao);
            self.gl.draw_elements(gl::TRIANGLES, count, index_type, 0);
            draws += 1;
            triangles += mesh_triangles;
        }
        self.stats.draws = draws;
        self.stats.triangles = triangles;
        self.stats.program_binds = program_binds;
        self.stats.material_binds = material_binds;
    }

    /// Deletes every GL object. The renderer is unusable afterwards (`init` refuses to run again,
    /// and every handle it issued stays refused); dropping it does this too.
    pub fn release(&mut self) {
        self.ready = false;
        self.released = true;
        self.objects.clear();
        self.materials.clear();
        let gl = &mut self.gl;
        self.textures.remove_all(|t| gl.texture_delete(t.gl));
        self.meshes.remove_all(|m| {
            gl.vertex_array_delete(m.vao);
            for b in m.buffers {
                gl.buffer_delete(b);
            }
        });
        for p in &mut self.programs {
            if *p != 0 {
                self.gl.program_delete(*p);
                *p = 0;
            }
        }
        if self.frame_ubo != 0 {
            self.gl.buffer_delete(self.frame_ubo);
            self.frame_ubo = 0;
        }
        self.object_ubo.release(&mut self.gl);
        self.material_ubo.release(&mut self.gl);
        self.timer.release(&mut self.gl);
        self.state = StateCache::unknown();
    }
}

impl<G: Gl> Drop for Renderer<G> {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{allocations, Call, MockGl};

    fn ready() -> Renderer<MockGl> {
        let mut r = Renderer::new(MockGl::new());
        r.init(800, 600).expect("init");
        r.gl_mut().calls.clear();
        r
    }

    /// A unit quad facing +Z, centred on the origin.
    fn quad() -> (Vec<f32>, Vec<f32>, Vec<f32>, Vec<u32>) {
        (
            vec![
                -0.5, -0.5, 0.0, 0.5, -0.5, 0.0, 0.5, 0.5, 0.0, -0.5, 0.5, 0.0,
            ],
            vec![0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
            vec![0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0],
            vec![0, 1, 2, 0, 2, 3],
        )
    }

    fn quad_mesh(r: &mut Renderer<MockGl>) -> u64 {
        let (p, n, uv, i) = quad();
        r.create_mesh(&MeshInput {
            positions: &p,
            normals: Some(&n),
            uvs: Some(&uv),
            indices: &i,
            ..MeshInput::default()
        })
        .expect("mesh")
    }

    #[test]
    fn handles_of_another_kind_or_another_renderer_are_refused() {
        let mut r = Renderer::with_serial(MockGl::new(), 1).unwrap();
        r.init(800, 600).unwrap();
        let mesh = quad_mesh(&mut r);
        let material = r.create_material(&MaterialDesc::default()).unwrap();
        // Arguments swapped.
        assert_eq!(
            r.create_object(material, mesh, &math::IDENTITY),
            Err(Error::Handle {
                kind: "mesh",
                handle: material
            })
        );
        assert_eq!(
            r.destroy_material(mesh).unwrap_err().to_string(),
            format!("{mesh} is not a valid material handle: it is a mesh handle")
        );
        assert!(r.set_visible(mesh, false).is_err());
        let object = r.create_object(mesh, material, &math::IDENTITY).unwrap();
        assert!(r.destroy_mesh(object).is_err());
        assert_eq!(r.objects.len(), 1);
        assert_eq!(
            Error::Handle {
                kind: "object",
                handle: 7
            }
            .to_string(),
            "7 is not a valid object handle",
            "a bare number has no kind"
        );

        let mut other = Renderer::with_serial(MockGl::new(), 2).unwrap();
        other.init(800, 600).unwrap();
        let theirs = quad_mesh(&mut other);
        let their_material = other.create_material(&MaterialDesc::default()).unwrap();
        let their_object = other
            .create_object(theirs, their_material, &math::IDENTITY)
            .unwrap();
        assert_eq!(object & 0x3f_ffff, their_object & 0x3f_ffff, "same slot");
        assert!(other.set_visible(object, false).is_err());
        assert!(other.destroy_object(object).is_err());
        assert!(r.set_transform(their_object, &math::IDENTITY).is_err());
        assert_eq!(other.objects.len(), 1);
        assert!(Renderer::with_serial(MockGl::new(), slots::SERIALS).is_none());
    }

    fn translate(x: f32, y: f32, z: f32) -> Mat4 {
        let mut m = math::IDENTITY;
        m[12] = x;
        m[13] = y;
        m[14] = z;
        m
    }

    /// The object slots of the draws in the last frame, in draw order.
    fn drawn_slots(r: &Renderer<MockGl>) -> Vec<u32> {
        let stride = r.object_ubo.stride as u32;
        r.gl()
            .calls
            .iter()
            .filter_map(|c| match c {
                Call::BindRange { index, offset, .. } if *index == shaders::BINDING_OBJECT => {
                    Some(offset / stride)
                }
                _ => None,
            })
            .collect()
    }

    fn texture(r: &mut Renderer<MockGl>) -> u64 {
        r.create_texture(&TextureDesc {
            width: 256,
            height: 128,
            srgb: true,
            mipmaps: true,
            wrap_s: Wrap::Repeat,
            wrap_t: Wrap::Clamp,
        })
        .expect("texture")
    }

    #[test]
    fn init_compiles_three_programs_and_binds_blocks_and_sampler() {
        let mut r = Renderer::new(MockGl::new());
        r.init(640, 480).unwrap();
        let calls = &r.gl().calls;
        let created = calls
            .iter()
            .filter(|c| matches!(c, Call::ProgramCreate(_)))
            .count();
        assert_eq!(created, PROGRAM_COUNT);
        // All programs are created before any is checked (parallel compile).
        let first_check = calls
            .iter()
            .position(|c| matches!(c, Call::ProgramCheck(_)))
            .unwrap();
        let last_create = calls
            .iter()
            .rposition(|c| matches!(c, Call::ProgramCreate(_)))
            .unwrap();
        assert!(last_create < first_check);
        let blocks = calls
            .iter()
            .filter(|c| matches!(c, Call::BindBlock(..)))
            .count();
        assert_eq!(blocks, PROGRAM_COUNT * 3);
        let samplers = calls
            .iter()
            .filter(|c| matches!(c, Call::BindSampler(..)))
            .count();
        assert_eq!(samplers, 2, "the two textured programs");
        assert!(calls.contains(&Call::Enable(gl::DEPTH_TEST)));
        assert!(r.caps().timer_query);
        assert_eq!(r.caps().max_texture_size, 4096);
        assert!(r.is_ready());
    }

    #[test]
    fn init_refuses_a_driver_layout_that_differs() {
        let mut gl = MockGl::new();
        gl.layout_override = Some(("u_camera_pos", 80));
        let mut r = Renderer::new(gl);
        let err = r.init(640, 480).unwrap_err();
        assert_eq!(
            err,
            Error::Layout {
                program: "plain",
                block: "Frame",
                uniform: "u_camera_pos",
                expected: 64,
                found: 80
            }
        );
        assert!(!r.is_ready());
        assert!(err
            .to_string()
            .contains("Frame.u_camera_pos at byte 80, not 64"));
    }

    #[test]
    fn init_reports_a_shader_that_fails() {
        let mut gl = MockGl::new();
        gl.fail_program = true;
        let mut r = Renderer::new(gl);
        assert_eq!(r.init(640, 480), Err(Error::Shader("plain")));
        // Nothing works before init succeeds.
        assert_eq!(r.create_mesh(&MeshInput::default()), Err(Error::NotReady));
        r.frame();
        assert!(!r.gl().calls.iter().any(|c| matches!(c, Call::Clear(_))));
    }

    #[test]
    fn uniform_slots_follow_alignment_and_driver_block_size() {
        let mut gl = MockGl::new();
        gl.ubo_alignment = 256;
        gl.block_size_extra = 16; // a driver that pads blocks
        let mut r = Renderer::new(gl);
        r.init(100, 100).unwrap();
        assert_eq!(r.object_ubo.stride, 256);
        assert_eq!(r.object_ubo.range, object_block::SIZE as u32 + 16);
        assert_eq!(r.material_ubo.range, material_block::SIZE as u32 + 16);
        let mesh = quad_mesh(&mut r);
        let mat = r.create_material(&MaterialDesc::default()).unwrap();
        for i in 0..3 {
            r.create_object(mesh, mat, &translate(i as f32 * 0.1, 0.0, 0.0))
                .unwrap();
        }
        r.gl_mut().calls.clear();
        r.frame();
        for c in &r.gl().calls {
            if let Call::BindRange { offset, size, .. } = c {
                assert_eq!(offset % 256, 0);
                assert!(*size >= 32);
            }
        }
    }

    #[test]
    fn mesh_upload_splits_positions_from_packed_attributes_and_narrows_indices() {
        let mut r = ready();
        let mesh = quad_mesh(&mut r);
        let calls = &r.gl().calls;
        let created: Vec<_> = calls
            .iter()
            .filter_map(|c| match c {
                Call::BufferCreate { target, len, .. } => Some((*target, *len)),
                _ => None,
            })
            .collect();
        assert_eq!(
            created,
            [
                (gl::ARRAY_BUFFER, 4 * 12),        // positions, f32 x 3
                (gl::ARRAY_BUFFER, 4 * 8),         // normal 2_10_10_10 + uv unorm16 x 2
                (gl::ELEMENT_ARRAY_BUFFER, 6 * 2)  // u16 indices
            ]
        );
        let attribs: Vec<_> = calls
            .iter()
            .filter_map(|c| match c {
                Call::Attrib(_, a) => Some(*a),
                _ => None,
            })
            .collect();
        assert_eq!(attribs.len(), 3);
        assert_eq!(
            (attribs[0].size, attribs[0].ty, attribs[0].stride),
            (3, gl::FLOAT, 12)
        );
        assert_eq!(
            (
                attribs[1].ty,
                attribs[1].normalized,
                attribs[1].stride,
                attribs[1].offset
            ),
            (gl::INT_2_10_10_10_REV, true, 8, 0)
        );
        assert_eq!(
            (attribs[2].ty, attribs[2].normalized, attribs[2].offset),
            (gl::UNSIGNED_SHORT, true, 4)
        );
        // The VAO is bound only after the index buffer exists, and unbound after.
        let index_created = calls
            .iter()
            .position(|c| {
                matches!(
                    c,
                    Call::BufferCreate {
                        target: gl::ELEMENT_ARRAY_BUFFER,
                        ..
                    }
                )
            })
            .unwrap();
        let vao_bound = calls
            .iter()
            .position(|c| matches!(c, Call::VaoBind(v) if *v != 0))
            .unwrap();
        assert!(index_created < vao_bound);
        assert_eq!(calls.last(), Some(&Call::VaoBind(0)));
        let info = r.mesh_info(mesh).unwrap();
        assert_eq!(info.vertices, 4);
        assert_eq!(info.triangles, 2);
        assert_eq!(info.index_format, IndexFormat::U16);
        assert_eq!(info.uv_format, Some(UvFormat::Unorm16));
        assert_eq!(info.attribute_stride, 8);
    }

    #[test]
    fn mesh_without_normals_or_uvs_gets_computed_normals_and_no_uv_attribute() {
        let mut r = ready();
        let (p, _, _, i) = quad();
        let mesh = r
            .create_mesh(&MeshInput {
                positions: &p,
                indices: &i,
                ..MeshInput::default()
            })
            .unwrap();
        let info = r.mesh_info(mesh).unwrap();
        assert_eq!((info.attribute_stride, info.uv_format), (4, None));
        let packed = r.gl().buffer_data(1).expect("attribute stream");
        // Computed normals face +Z, like the explicit ones.
        assert_eq!(
            u32::from_le_bytes(packed[0..4].try_into().unwrap()),
            pack::pack_normal([0.0, 0.0, 1.0])
        );
        let uv_attribs = r
            .gl()
            .calls
            .iter()
            .filter(|c| matches!(c, Call::Attrib(_, a) if a.location == shaders::ATTRIB_UV))
            .count();
        assert_eq!(uv_attribs, 0);
    }

    #[test]
    fn bad_meshes_are_refused_before_any_gl_call() {
        let mut r = ready();
        let (p, n, uv, i) = quad();
        let cases: Vec<(MeshInput<'_>, &str)> = vec![
            (
                MeshInput {
                    positions: &p[..10],
                    indices: &i,
                    ..Default::default()
                },
                "positions",
            ),
            (
                MeshInput {
                    positions: &p,
                    normals: Some(&n[..9]),
                    indices: &i,
                    ..Default::default()
                },
                "normals",
            ),
            (
                MeshInput {
                    positions: &p,
                    uvs: Some(&uv[..6]),
                    indices: &i,
                    ..Default::default()
                },
                "uvs",
            ),
            (
                MeshInput {
                    positions: &p,
                    indices: &i[..4],
                    ..Default::default()
                },
                "whole triangles",
            ),
            (
                MeshInput {
                    positions: &p,
                    indices: &[0, 1, 4],
                    ..Default::default()
                },
                "past the last vertex",
            ),
            (
                MeshInput {
                    positions: &p,
                    indices: &i,
                    joints: Some(&[0; 16]),
                    ..Default::default()
                },
                "come together",
            ),
            (
                MeshInput {
                    positions: &p,
                    indices: &i,
                    morph_positions: Some(&[0.0; 12]),
                    morph_targets: 2,
                    ..Default::default()
                },
                "morph deltas",
            ),
        ];
        let nan = [f32::NAN, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let mut all = cases;
        all.push((
            MeshInput {
                positions: &nan,
                indices: &[0, 1, 2],
                ..Default::default()
            },
            "finite",
        ));
        for (input, why) in all {
            let err = r.create_mesh(&input).unwrap_err();
            assert!(err.to_string().contains(why), "{err} (expected {why})");
        }
        assert!(
            !r.gl()
                .calls
                .iter()
                .any(|c| matches!(c, Call::BufferCreate { .. } | Call::VaoCreate(_))),
            "a refused mesh made GL objects"
        );
    }

    #[test]
    fn skin_weights_must_be_in_range_and_sum_to_one() {
        let mut r = ready();
        let (p, _, _, i) = quad();
        let joints = [0u16; 16];
        let mesh = |r: &mut Renderer<MockGl>, w: &[f32]| {
            r.create_mesh(&MeshInput {
                positions: &p,
                indices: &i,
                joints: Some(&joints),
                weights: Some(w),
                ..MeshInput::default()
            })
        };
        // The critic's case: 1.75 each, 7 a vertex.
        assert!(mesh(&mut r, &[1.75f32; 16]).is_err());
        assert!(mesh(&mut r, &[0.5f32; 16])
            .unwrap_err()
            .to_string()
            .contains("sum to 1"));
        let mut negative = [0.25f32; 16];
        negative[0] = -0.25;
        negative[1] = 0.75;
        assert!(mesh(&mut r, &negative)
            .unwrap_err()
            .to_string()
            .contains("between 0 and 1"));
        let mut nan = [0.25f32; 16];
        nan[5] = f32::NAN;
        assert!(mesh(&mut r, &nan).is_err());
        // Bytes dequantised (255 = 1): 64 + 64 + 64 + 62 = 254 of 255, inside the tolerance.
        let bytes = [64.0 / 255.0, 64.0 / 255.0, 64.0 / 255.0, 62.0 / 255.0];
        let dequantised: Vec<f32> = bytes.iter().cycle().take(16).copied().collect();
        assert!(mesh(&mut r, &dequantised).is_ok());
    }

    #[test]
    fn morph_target_counts_are_capped_and_never_overflow() {
        let mut r = ready();
        let (p, _, _, i) = quad();
        let deltas = [0.0f32; 24];
        let mesh = |r: &mut Renderer<MockGl>, targets: usize| {
            r.create_mesh(&MeshInput {
                positions: &p,
                indices: &i,
                morph_positions: Some(&deltas),
                morph_targets: targets,
                ..MeshInput::default()
            })
        };
        assert!(mesh(&mut r, 2).is_ok());
        assert!(mesh(&mut r, 257)
            .unwrap_err()
            .to_string()
            .contains("1 to 256"));
        // 4 vertices * 3 * 0x5555_5556 wraps to 24 in 32 bits; refused, not accepted.
        assert!(mesh(&mut r, 0x5555_5556).is_err());
        let huge = MeshInput {
            positions: &p,
            indices: &i,
            morph_positions: Some(&deltas),
            morph_targets: usize::MAX,
            ..MeshInput::default()
        };
        assert!(validate_mesh(&huge).is_err());
    }

    #[test]
    fn skin_and_morph_data_are_accepted_and_recorded_for_later_phases() {
        let mut r = ready();
        let (p, n, uv, i) = quad();
        let joints = [0u16; 16];
        let weights = [0.25f32; 16];
        let morphs = [0.0f32; 24];
        let mesh = r
            .create_mesh(&MeshInput {
                positions: &p,
                normals: Some(&n),
                uvs: Some(&uv),
                indices: &i,
                joints: Some(&joints),
                weights: Some(&weights),
                morph_positions: Some(&morphs),
                morph_targets: 2,
            })
            .unwrap();
        let info = r.mesh_info(mesh).unwrap();
        assert!(info.skinned);
        assert_eq!(info.morph_targets, 2);
    }

    #[test]
    fn frame_draws_opaque_front_to_back_then_masked_then_blended_back_to_front() {
        let mut r = ready();
        r.set_camera_look_at([0.0, 0.0, 10.0], [0.0; 3], [0.0, 1.0, 0.0], 1.0, 0.1, 100.0)
            .unwrap();
        let mesh = quad_mesh(&mut r);
        let tex = texture(&mut r);
        let opaque = r.create_material(&MaterialDesc::default()).unwrap();
        let masked = r
            .create_material(&MaterialDesc {
                texture: tex,
                alpha_mode: AlphaMode::Mask,
                ..MaterialDesc::default()
            })
            .unwrap();
        let blended = r
            .create_material(&MaterialDesc {
                base_color: [1.0, 1.0, 1.0, 0.5],
                alpha_mode: AlphaMode::Blend,
                ..MaterialDesc::default()
            })
            .unwrap();
        // Created in an order that is none of the expected draw orders.
        let o_far = r
            .create_object(mesh, opaque, &translate(0.0, 0.0, -5.0))
            .unwrap();
        let b_near = r
            .create_object(mesh, blended, &translate(0.0, 0.0, 4.0))
            .unwrap();
        let o_near = r
            .create_object(mesh, opaque, &translate(0.0, 0.0, 3.0))
            .unwrap();
        let m_mid = r
            .create_object(mesh, masked, &translate(0.0, 0.0, 0.0))
            .unwrap();
        let b_far = r
            .create_object(mesh, blended, &translate(0.0, 0.0, -2.0))
            .unwrap();
        let slot = |h: u64| r.objects.index_of(h).unwrap() as u32;
        let expected = [
            slot(o_near),
            slot(o_far),
            slot(m_mid),
            slot(b_far),
            slot(b_near),
        ];
        r.gl_mut().calls.clear();
        r.frame();
        assert_eq!(drawn_slots(&r), expected);
        // Blending is on, and depth writes off, only for the blended draws.
        let calls = &r.gl().calls;
        let blend_on = calls
            .iter()
            .position(|c| *c == Call::Enable(gl::BLEND))
            .unwrap();
        let draws: Vec<usize> = calls
            .iter()
            .enumerate()
            .filter(|(_, c)| matches!(c, Call::Draw { .. }))
            .map(|(i, _)| i)
            .collect();
        assert!(draws[2] < blend_on && blend_on < draws[3]);
        assert!(calls[draws[2]..draws[3]].contains(&Call::DepthMask(false)));
        assert_eq!(r.stats().draws, 5);
        assert_eq!(r.stats().triangles, 10);
    }

    #[test]
    fn double_sided_turns_culling_off_and_mirroring_flips_the_front_face() {
        let mut r = ready();
        r.set_camera_look_at([0.0, 0.0, 10.0], [0.0; 3], [0.0, 1.0, 0.0], 1.0, 0.1, 100.0)
            .unwrap();
        let mesh = quad_mesh(&mut r);
        let two = r
            .create_material(&MaterialDesc {
                double_sided: true,
                ..MaterialDesc::default()
            })
            .unwrap();
        let one = r.create_material(&MaterialDesc::default()).unwrap();
        let mut mirror = translate(0.0, 0.0, 2.0);
        mirror[0] = -1.0;
        r.create_object(mesh, two, &translate(0.0, 0.0, 0.0))
            .unwrap();
        r.create_object(mesh, one, &mirror).unwrap();
        r.gl_mut().calls.clear();
        r.frame();
        let calls = &r.gl().calls;
        let draws: Vec<usize> = calls
            .iter()
            .enumerate()
            .filter(|(_, c)| matches!(c, Call::Draw { .. }))
            .map(|(i, _)| i)
            .collect();
        // By material: the double-sided quad (created first), then the mirrored single-sided one.
        assert!(calls[..draws[0]].contains(&Call::Disable(gl::CULL_FACE)));
        assert!(calls[..draws[0]].contains(&Call::FrontFace(gl::CCW)));
        assert!(calls[draws[0]..draws[1]].contains(&Call::Enable(gl::CULL_FACE)));
        assert!(calls[draws[0]..draws[1]].contains(&Call::FrontFace(gl::CW)));
    }

    #[test]
    fn objects_outside_the_view_hidden_or_fully_masked_are_not_drawn() {
        let mut r = ready();
        r.set_camera_look_at([0.0, 0.0, 10.0], [0.0; 3], [0.0, 1.0, 0.0], 1.0, 0.1, 100.0)
            .unwrap();
        let mesh = quad_mesh(&mut r);
        let mat = r.create_material(&MaterialDesc::default()).unwrap();
        let invisible_mask = r
            .create_material(&MaterialDesc {
                base_color: [1.0, 1.0, 1.0, 0.2],
                alpha_mode: AlphaMode::Mask,
                alpha_cutoff: 0.5,
                ..MaterialDesc::default()
            })
            .unwrap();
        r.create_object(mesh, mat, &translate(0.0, 0.0, 0.0))
            .unwrap();
        r.create_object(mesh, mat, &translate(0.0, 0.0, 20.0))
            .unwrap(); // behind the camera
        r.create_object(mesh, mat, &translate(500.0, 0.0, 0.0))
            .unwrap(); // far to the side
        let hidden = r
            .create_object(mesh, mat, &translate(0.5, 0.0, 0.0))
            .unwrap();
        r.set_visible(hidden, false).unwrap();
        r.create_object(mesh, invisible_mask, &translate(-0.5, 0.0, 0.0))
            .unwrap();
        r.frame();
        assert_eq!(r.stats().draws, 1);
        assert_eq!(r.stats().culled, 2);
        r.set_visible(hidden, true).unwrap();
        r.frame();
        assert_eq!(r.stats().draws, 2);
    }

    #[test]
    fn handles_are_checked_and_resources_in_use_cannot_be_destroyed() {
        let mut r = ready();
        let mesh = quad_mesh(&mut r);
        let tex = texture(&mut r);
        let mat = r
            .create_material(&MaterialDesc {
                texture: tex,
                ..MaterialDesc::default()
            })
            .unwrap();
        let obj = r.create_object(mesh, mat, &math::IDENTITY).unwrap();
        assert!(matches!(
            r.destroy_mesh(mesh),
            Err(Error::InUse {
                kind: "mesh",
                users: 1,
                ..
            })
        ));
        assert!(matches!(
            r.destroy_material(mat),
            Err(Error::InUse {
                kind: "material",
                ..
            })
        ));
        assert!(matches!(
            r.destroy_texture(tex),
            Err(Error::InUse {
                kind: "texture",
                ..
            })
        ));
        r.destroy_object(obj).unwrap();
        assert_eq!(
            r.set_transform(obj, &math::IDENTITY),
            Err(Error::Handle {
                kind: "object",
                handle: obj
            })
        );
        r.destroy_material(mat).unwrap();
        r.destroy_texture(tex).unwrap();
        r.destroy_mesh(mesh).unwrap();
        assert!(matches!(r.destroy_mesh(mesh), Err(Error::Handle { .. })));
        assert!(matches!(
            r.create_object(mesh, mat, &math::IDENTITY),
            Err(Error::Handle { kind: "mesh", .. })
        ));
        let deleted = r
            .gl()
            .calls
            .iter()
            .filter(|c| {
                matches!(
                    c,
                    Call::BufferDelete(_) | Call::VaoDelete(_) | Call::TextureDelete(_)
                )
            })
            .count();
        assert_eq!(deleted, 5, "three buffers, a vertex array and a texture");
    }

    #[test]
    fn texture_flags_refuse_unknown_wrap_modes_and_bits() {
        let d = TextureDesc::from_flags(4, 4, TEXTURE_SRGB | TEXTURE_MIPMAPS | 1 << 2 | 2 << 4)
            .unwrap();
        assert!(d.srgb && d.mipmaps);
        assert_eq!((d.wrap_s, d.wrap_t), (Wrap::Clamp, Wrap::Mirror));
        assert!(TextureDesc::from_flags(4, 4, 3 << 2).is_err(), "wrap s = 3");
        assert!(TextureDesc::from_flags(4, 4, 3 << 4).is_err(), "wrap t = 3");
        assert!(
            TextureDesc::from_flags(4, 4, 1 << 6).is_err(),
            "unknown bit"
        );
    }

    #[test]
    fn non_finite_normals_are_refused() {
        let mut r = ready();
        let (p, mut n, _, i) = quad();
        n[4] = f32::NAN;
        let err = r
            .create_mesh(&MeshInput {
                positions: &p,
                normals: Some(&n),
                indices: &i,
                ..MeshInput::default()
            })
            .unwrap_err();
        assert!(err.to_string().contains("normals must be finite"), "{err}");
    }

    #[test]
    fn transforms_must_be_finite() {
        let mut r = ready();
        let mesh = quad_mesh(&mut r);
        let mat = r.create_material(&MaterialDesc::default()).unwrap();
        let mut bad = math::IDENTITY;
        bad[13] = f32::INFINITY;
        assert_eq!(r.create_object(mesh, mat, &bad), Err(Error::Transform));
        // Scaled to zero is allowed (a common way to hide something).
        let mut zero = math::IDENTITY;
        zero[0] = 0.0;
        zero[5] = 0.0;
        zero[10] = 0.0;
        assert!(r.create_object(mesh, mat, &zero).is_ok());
    }

    #[test]
    fn textures_respect_the_size_limit_and_get_a_full_mip_chain() {
        let mut r = ready();
        assert_eq!(
            r.create_texture(&TextureDesc {
                width: 8192,
                height: 16,
                srgb: true,
                mipmaps: true,
                wrap_s: Wrap::Repeat,
                wrap_t: Wrap::Repeat
            }),
            Err(Error::TextureSize {
                width: 8192,
                height: 16,
                max: 4096
            })
        );
        texture(&mut r);
        let upload = r
            .gl()
            .calls
            .iter()
            .find_map(|c| match c {
                Call::TextureCreate(u) => Some(*u),
                _ => None,
            })
            .unwrap();
        assert_eq!(upload.levels, 9, "256x128: 256 down to 1");
        assert_eq!(upload.internal_format, gl::SRGB8_ALPHA8);
        assert_eq!(
            (upload.wrap_s, upload.wrap_t),
            (gl::REPEAT, gl::CLAMP_TO_EDGE)
        );
        assert_eq!(upload.min_filter, gl::LINEAR_MIPMAP_LINEAR);
        r.gl_mut().fail_texture = true;
        assert_eq!(
            r.create_texture(&TextureDesc {
                width: 4,
                height: 4,
                srgb: false,
                mipmaps: false,
                wrap_s: Wrap::Repeat,
                wrap_t: Wrap::Repeat
            }),
            Err(Error::TextureUpload)
        );
    }

    #[test]
    fn unchanged_state_is_not_sent_again() {
        let mut r = ready();
        r.set_camera_look_at([0.0, 0.0, 10.0], [0.0; 3], [0.0, 1.0, 0.0], 1.0, 0.1, 100.0)
            .unwrap();
        let mesh = quad_mesh(&mut r);
        let mat = r.create_material(&MaterialDesc::default()).unwrap();
        for i in 0..10 {
            r.create_object(mesh, mat, &translate(i as f32 * 0.2 - 1.0, 0.0, 0.0))
                .unwrap();
        }
        r.frame();
        r.gl_mut().calls.clear();
        r.frame();
        let calls = &r.gl().calls;
        let count = |f: fn(&Call) -> bool| calls.iter().filter(|c| f(c)).count();
        assert_eq!(count(|c| matches!(c, Call::Draw { .. })), 10);
        assert_eq!(count(|c| matches!(c, Call::UseProgram(_))), 0);
        assert_eq!(count(|c| matches!(c, Call::VaoBind(_))), 0);
        assert_eq!(
            count(|c| matches!(c, Call::Enable(_) | Call::Disable(_))),
            0
        );
        assert_eq!(
            count(|c| matches!(c, Call::Viewport(..) | Call::ClearColor(_))),
            0
        );
        // Only the frame block goes up: no object moved.
        assert_eq!(count(|c| matches!(c, Call::BufferUpdate { .. })), 1);
    }

    #[test]
    fn depth_writes_come_back_on_before_the_clear() {
        let mut r = ready();
        r.set_camera_look_at([0.0, 0.0, 10.0], [0.0; 3], [0.0, 1.0, 0.0], 1.0, 0.1, 100.0)
            .unwrap();
        let mesh = quad_mesh(&mut r);
        let glass = r
            .create_material(&MaterialDesc {
                alpha_mode: AlphaMode::Blend,
                ..MaterialDesc::default()
            })
            .unwrap();
        r.create_object(mesh, glass, &math::IDENTITY).unwrap();
        r.frame();
        r.gl_mut().calls.clear();
        r.frame();
        let calls = &r.gl().calls;
        let on = calls
            .iter()
            .position(|c| *c == Call::DepthMask(true))
            .unwrap();
        let clear = calls
            .iter()
            .position(|c| matches!(c, Call::Clear(_)))
            .unwrap();
        assert!(on < clear, "{calls:?}");
        assert_eq!(
            calls[clear],
            Call::Clear(gl::COLOR_BUFFER_BIT | gl::DEPTH_BUFFER_BIT)
        );
        assert!(calls.contains(&Call::InvalidateDepth));
    }

    #[test]
    fn frame_block_carries_camera_and_normalised_light() {
        let mut r = ready();
        let view = math::look_at([1.0, 2.0, 3.0], [0.0; 3], [0.0, 1.0, 0.0]);
        let proj = math::perspective(0.8, 1.5, 0.1, 50.0);
        r.set_camera(&view, &proj).unwrap();
        r.set_key_light([0.0, 3.0, 4.0], [1.0, 0.5, 0.25], 2.0)
            .unwrap();
        r.set_ambient([0.2, 0.3, 0.4], [0.1, 0.1, 0.1], 0.5)
            .unwrap();
        r.frame();
        let b = &r.frame_data;
        let vp = math::mul(&proj, &view);
        for (k, v) in vp.iter().enumerate() {
            assert!((pack::get_f32(b, frame_block::VIEW_PROJ + 4 * k) - v).abs() < 1e-6);
        }
        let at = |o: usize| pack::get_f32(b, o);
        assert!((at(frame_block::CAMERA_POS) - 1.0).abs() < 1e-5);
        assert!((at(frame_block::CAMERA_POS + 4) - 2.0).abs() < 1e-5);
        assert!((at(frame_block::CAMERA_POS + 8) - 3.0).abs() < 1e-5);
        assert_eq!(at(frame_block::KEY_DIR + 4), 0.6);
        assert_eq!(at(frame_block::KEY_DIR + 8), 0.8);
        assert_eq!(at(frame_block::KEY_COLOR + 4), 1.0);
        assert_eq!(at(frame_block::SKY + 8), 0.2);
        assert!(r.set_key_light([0.0; 3], [1.0; 3], 1.0).is_err());
        assert!(r.set_camera(&[0.0; 16], &proj).is_err());
    }

    #[test]
    fn resize_moves_the_viewport_and_keeps_the_look_at_projection_in_aspect() {
        let mut r = ready();
        r.set_camera_look_at([0.0, 0.0, 5.0], [0.0; 3], [0.0, 1.0, 0.0], 1.0, 0.1, 10.0)
            .unwrap();
        r.resize(1024, 512).unwrap();
        r.frame();
        assert!(r.gl().calls.contains(&Call::Viewport(1024, 512)));
        let p = r.camera.proj;
        assert!(
            (p[5] / p[0] - 2.0).abs() < 1e-5,
            "aspect 2: {} {}",
            p[0],
            p[5]
        );
        // A projection set directly is left alone.
        let proj = math::perspective(1.0, 1.0, 0.1, 10.0);
        r.set_camera(&math::IDENTITY, &proj).unwrap();
        r.resize(300, 100).unwrap();
        assert_eq!(r.camera.proj, proj);
        assert_eq!(
            r.resize(0x8000_0001, 100),
            Err(Error::Viewport {
                width: 0x8000_0001,
                height: 100
            })
        );
        assert_eq!(r.size(), (300, 100), "a refused size keeps the old one");
        assert!(r.resize(MAX_DRAWING_BUFFER + 1, 1).is_err());
        assert!(Renderer::new(MockGl::new()).init(1, 0x8000_0000).is_err());
        r.resize(0, 0).unwrap();
        r.gl_mut().calls.clear();
        r.frame();
        assert!(r.gl().calls.is_empty(), "nothing to draw into");
    }

    #[test]
    fn clear_colour_is_linear_and_encoded_once() {
        let mut r = ready();
        r.set_clear_color([0.5, 0.0, 1.0]).unwrap();
        assert!(r.set_clear_color([f32::NAN, 0.0, 0.0]).is_err());
        r.frame();
        r.frame();
        let clears: Vec<_> = r
            .gl()
            .calls
            .iter()
            .filter_map(|c| match c {
                Call::ClearColor(c) => Some(*c),
                _ => None,
            })
            .collect();
        assert_eq!(clears.len(), 1);
        assert!((clears[0][0] - 0.735_356_7).abs() < 1e-5);
    }

    #[test]
    fn cpu_timing_is_off_until_asked_for() {
        let mut r = ready();
        let before = r.gl().clock_ms;
        r.frame();
        assert!(r.stats().cpu_ms.is_nan());
        assert_eq!(r.gl().clock_ms, before, "no clock reading while off");
        r.set_cpu_timing(true);
        r.frame();
        assert_eq!(
            r.stats().cpu_ms,
            0.25,
            "two readings, 0.25 ms apart on the mock clock"
        );
        r.set_cpu_timing(false);
        assert!(r.stats().cpu_ms.is_nan());
    }

    #[test]
    fn gpu_time_reaches_the_stats() {
        let mut r = ready();
        r.gl_mut().query_ms = 3.25;
        r.frame();
        assert!(r.stats().gpu_ms.is_nan(), "results come back a frame late");
        r.frame();
        assert_eq!((r.stats().gpu_ms, r.stats().gpu_frame), (3.25, 1));
    }

    #[test]
    fn a_frame_allocates_nothing() {
        let mut r = ready();
        r.gl_mut().record = false;
        let mesh = quad_mesh(&mut r);
        let tex = texture(&mut r);
        let mats = [
            r.create_material(&MaterialDesc::default()).unwrap(),
            r.create_material(&MaterialDesc {
                texture: tex,
                alpha_mode: AlphaMode::Mask,
                double_sided: true,
                ..MaterialDesc::default()
            })
            .unwrap(),
            r.create_material(&MaterialDesc {
                alpha_mode: AlphaMode::Blend,
                ..MaterialDesc::default()
            })
            .unwrap(),
        ];
        let mut objects = Vec::new();
        for i in 0..60 {
            let at = translate(
                (i % 10) as f32 - 5.0,
                (i / 10) as f32 - 3.0,
                -(i as f32) * 0.3,
            );
            objects.push(r.create_object(mesh, mats[i % 3], &at).unwrap());
        }
        r.frame(); // warm-up
        let before = allocations();
        for f in 0..30 {
            let t = f as f32 * 0.1;
            r.set_camera_look_at(
                [t.sin() * 12.0, 2.0, t.cos() * 12.0],
                [0.0; 3],
                [0.0, 1.0, 0.0],
                1.0,
                0.1,
                100.0,
            )
            .unwrap();
            r.set_transform(objects[f % 60], &translate(t, 0.0, 0.0))
                .unwrap();
            r.set_key_light([t, 1.0, 0.5], [1.0; 3], 1.0).unwrap();
            r.frame();
        }
        let after = allocations();
        assert_eq!(after - before, 0, "allocations during 30 frames");
        assert!(r.stats().draws > 0);
    }

    #[test]
    fn draw_keys_order_passes_programs_materials_and_depth() {
        let near = draw_key(Pass::Opaque, Program::Plain, 1.0, 0);
        let far = draw_key(Pass::Opaque, Program::Plain, 50.0, 0);
        let other_material_near = draw_key(Pass::Opaque, Program::Plain, 0.5, 9);
        let textured = draw_key(Pass::Opaque, Program::Textured, 0.5, 0);
        let mask = draw_key(Pass::Mask, Program::TexturedMask, 0.1, 0);
        let blend_far = draw_key(Pass::Blend, Program::Textured, 40.0, 9);
        let blend_near = draw_key(Pass::Blend, Program::Plain, 2.0, 0);
        // Within a program, a material's draws stay together, front to back.
        assert!(near < far && far < other_material_near && other_material_near < textured);
        assert!(textured < mask);
        // Blended draws go back to front, whatever their program or material.
        assert!(mask < blend_far && blend_far < blend_near);
        // Behind the camera counts as distance 0; NaN sorts last.
        assert!(draw_key(Pass::Opaque, Program::Plain, -3.0, 0) <= near);
        assert!(draw_key(Pass::Opaque, Program::Plain, f32::NAN, 0) > far);
        assert!(draw_key(Pass::Opaque, Program::Plain, f32::NAN, 0) < other_material_near);
    }

    #[test]
    fn each_material_is_bound_once_a_frame() {
        let mut r = ready();
        r.set_camera_look_at([0.0, 0.0, 10.0], [0.0; 3], [0.0, 1.0, 0.0], 1.0, 0.1, 100.0)
            .unwrap();
        let mesh = quad_mesh(&mut r);
        let mats: Vec<u64> = (0..3)
            .map(|i| {
                r.create_material(&MaterialDesc {
                    roughness: 0.2 + 0.3 * i as f32,
                    ..MaterialDesc::default()
                })
                .unwrap()
            })
            .collect();
        // Depths interleave the materials: depth-first sorting would rebind on every draw.
        for i in 0..12 {
            let z = -(i as f32) * 0.5;
            r.create_object(mesh, mats[i % 3], &translate(0.0, 0.0, z))
                .unwrap();
        }
        r.frame();
        assert_eq!(r.stats().draws, 12);
        assert_eq!(r.stats().material_binds, 3);
        // And within each material, front to back.
        let order = drawn_slots(&r);
        for m in 0..3 {
            let slots: Vec<u32> = order
                .iter()
                .copied()
                .filter(|s| *s as usize % 3 == m)
                .collect();
            assert!(
                slots.windows(2).all(|w| w[0] < w[1]),
                "material {m}: {slots:?}"
            );
        }
    }

    #[test]
    fn released_renderers_cannot_be_initialised_again() {
        let mut r = ready();
        let mesh = quad_mesh(&mut r);
        r.release();
        assert_eq!(r.init(100, 100), Err(Error::Released));
        assert_eq!(r.mesh_info(mesh), None);
        assert!(matches!(r.destroy_mesh(mesh), Err(Error::Handle { .. })));
    }

    #[test]
    fn release_deletes_everything() {
        let mut r = ready();
        let mesh = quad_mesh(&mut r);
        let tex = texture(&mut r);
        let mat = r
            .create_material(&MaterialDesc {
                texture: tex,
                ..MaterialDesc::default()
            })
            .unwrap();
        r.create_object(mesh, mat, &math::IDENTITY).unwrap();
        r.gl_mut().calls.clear();
        r.release();
        let calls = &r.gl().calls;
        let count = |f: fn(&Call) -> bool| calls.iter().filter(|c| f(c)).count();
        assert_eq!(
            count(|c| matches!(c, Call::ProgramDelete(_))),
            PROGRAM_COUNT
        );
        assert_eq!(count(|c| matches!(c, Call::TextureDelete(_))), 1);
        assert_eq!(count(|c| matches!(c, Call::VaoDelete(_))), 1);
        // Three mesh buffers and three uniform buffers.
        assert_eq!(count(|c| matches!(c, Call::BufferDelete(_))), 6);
        assert_eq!(
            count(|c| matches!(c, Call::QueryDelete(_))),
            crate::timer::RING
        );
        assert!(!r.is_ready());
        r.frame();
    }
}
