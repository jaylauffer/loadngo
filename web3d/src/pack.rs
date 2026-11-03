//! Packing data for the GPU: vertex streams, indices and std140 uniform blocks.
//!
//! A mesh becomes two vertex streams and an index buffer:
//!
//! - **Positions** (`f32` x 3, 12 bytes a vertex) in a stream of their own. A tile-based GPU such
//!   as the Pi 5's V3D runs a position-only pass to bin triangles into tiles before shading, and
//!   that pass then reads nothing else.
//! - **Attributes**, interleaved: the normal as `INT_2_10_10_10_REV` (4 bytes, 10 bits a
//!   component), then the texture coordinates as normalised `u16` pairs when every coordinate is in
//!   0..=1 (4 bytes, a step of 1/65535), else as `f32` pairs (8 bytes). That is 8 bytes a vertex in
//!   the common case, against 20 unpacked.
//! - **Indices** as `u16` when the mesh has at most 65,535 vertices, else `u32`. WebGL 2 always
//!   has `PRIMITIVE_RESTART_FIXED_INDEX` on, so the `u16` index 0xFFFF ends the primitive instead
//!   of naming vertex 65,535: a 65,536-vertex mesh needs `u32`.
//!
//! All multi-byte values are little-endian, as WebAssembly and WebGL are.

use crate::gl;
use crate::math::{self, Vec3};

/// How the texture coordinates are stored in the attribute stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UvFormat {
    /// Normalised `u16` pairs, for coordinates all within 0..=1.
    Unorm16,
    /// `f32` pairs, for anything else (tiling, or atlases with margins outside 0..=1).
    Float32,
}

impl UvFormat {
    /// The GL component type and whether it is normalised.
    pub fn gl_type(self) -> (u32, bool) {
        match self {
            UvFormat::Unorm16 => (gl::UNSIGNED_SHORT, true),
            UvFormat::Float32 => (gl::FLOAT, false),
        }
    }

    pub fn size(self) -> usize {
        match self {
            UvFormat::Unorm16 => 4,
            UvFormat::Float32 => 8,
        }
    }
}

/// The layout of a packed attribute stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttributeLayout {
    /// Bytes from one vertex to the next.
    pub stride: usize,
    /// The normal is always at offset 0.
    pub normal_offset: usize,
    /// Format and byte offset of the texture coordinates, when the mesh has them.
    pub uv: Option<(UvFormat, usize)>,
}

/// Index width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexFormat {
    U16,
    U32,
}

impl IndexFormat {
    pub fn gl_type(self) -> u32 {
        match self {
            IndexFormat::U16 => gl::UNSIGNED_SHORT,
            IndexFormat::U32 => gl::UNSIGNED_INT,
        }
    }
}

/// A bounding sphere.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sphere {
    pub center: Vec3,
    pub radius: f32,
}

/// One signed 10-bit normalised component, as `INT_2_10_10_10_REV` stores it (GL ES 3.0 2.1.6.1:
/// `c / 511`, clamped to -1).
fn snorm10(v: f32) -> u32 {
    ((v.clamp(-1.0, 1.0) * 511.0).round() as i32 & 0x3ff) as u32
}

fn from_snorm10(bits: u32) -> f32 {
    // Sign-extend the 10-bit field.
    let v = ((bits as i32) << 22) >> 22;
    (v as f32 / 511.0).max(-1.0)
}

/// Packs a normal into `INT_2_10_10_10_REV`: x in bits 0-9, y in 10-19, z in 20-29, and 0 in the
/// 2-bit w (kept for a tangent's handedness later). The normal is renormalised first; a zero or
/// non-finite normal becomes +Y, which at least takes the sky's light.
pub fn pack_normal(n: Vec3) -> u32 {
    let n = math::normalize(n).unwrap_or([0.0, 1.0, 0.0]);
    snorm10(n[0]) | snorm10(n[1]) << 10 | snorm10(n[2]) << 20
}

/// The inverse of [`pack_normal`], as the GPU decodes it.
pub fn unpack_normal(p: u32) -> Vec3 {
    [
        from_snorm10(p & 0x3ff),
        from_snorm10((p >> 10) & 0x3ff),
        from_snorm10((p >> 20) & 0x3ff),
    ]
}

/// The narrowest exact-enough format for these coordinates.
pub fn uv_format(uvs: &[f32]) -> UvFormat {
    if uvs.iter().all(|&v| (0.0..=1.0).contains(&v)) {
        UvFormat::Unorm16
    } else {
        UvFormat::Float32
    }
}

fn unorm16(v: f32) -> u16 {
    (v.clamp(0.0, 1.0) * 65535.0).round() as u16
}

/// Interleaves normals (3 floats a vertex) and optional texture coordinates (2 a vertex) into
/// `out`, which is cleared first and reused across meshes. The caller has checked the lengths.
pub fn pack_attributes(normals: &[f32], uvs: Option<&[f32]>, out: &mut Vec<u8>) -> AttributeLayout {
    let vertex_count = normals.len() / 3;
    let uv = uvs.map(|uvs| (uv_format(uvs), 4));
    let stride = 4 + uv.map_or(0, |(format, _)| format.size());
    out.clear();
    out.reserve(vertex_count * stride);
    for i in 0..vertex_count {
        let n = [normals[i * 3], normals[i * 3 + 1], normals[i * 3 + 2]];
        out.extend_from_slice(&pack_normal(n).to_le_bytes());
        if let (Some(uvs), Some((format, _))) = (uvs, uv) {
            let (u, v) = (uvs[i * 2], uvs[i * 2 + 1]);
            match format {
                UvFormat::Unorm16 => {
                    out.extend_from_slice(&unorm16(u).to_le_bytes());
                    out.extend_from_slice(&unorm16(v).to_le_bytes());
                }
                UvFormat::Float32 => {
                    out.extend_from_slice(&u.to_le_bytes());
                    out.extend_from_slice(&v.to_le_bytes());
                }
            }
        }
    }
    AttributeLayout {
        stride,
        normal_offset: 0,
        uv,
    }
}

/// The most vertices a mesh may have and still take `u16` indices: 0xFFFF is WebGL 2's primitive
/// restart index for `u16`, always on, so the largest usable index is 0xFFFE.
pub const MAX_U16_VERTICES: usize = 0xFFFF;

/// Packs indices into `out` (cleared first): `u16` when every index is below 0xFFFF (at most
/// [`MAX_U16_VERTICES`] vertices), else `u32`. Returns `None` if an index is out of range.
pub fn pack_indices(
    indices: &[u32],
    vertex_count: usize,
    out: &mut Vec<u8>,
) -> Option<IndexFormat> {
    if indices.iter().any(|&i| i as usize >= vertex_count) {
        return None;
    }
    out.clear();
    if vertex_count <= MAX_U16_VERTICES {
        out.reserve(indices.len() * 2);
        for &i in indices {
            out.extend_from_slice(&(i as u16).to_le_bytes());
        }
        Some(IndexFormat::U16)
    } else {
        out.reserve(indices.len() * 4);
        for &i in indices {
            out.extend_from_slice(&i.to_le_bytes());
        }
        Some(IndexFormat::U32)
    }
}

/// Smooth vertex normals for a mesh that came without any: each triangle adds its area-weighted
/// face normal to its three corners. `out` is cleared and reused. The caller has checked indices.
pub fn compute_normals(positions: &[f32], indices: &[u32], out: &mut Vec<f32>) {
    out.clear();
    out.resize(positions.len(), 0.0);
    let p = |i: u32| -> Vec3 {
        let i = i as usize * 3;
        [positions[i], positions[i + 1], positions[i + 2]]
    };
    for tri in indices.as_chunks::<3>().0 {
        // Twice the area, as the length of the cross product: larger faces weigh more.
        let n = math::cross(
            math::sub(p(tri[1]), p(tri[0])),
            math::sub(p(tri[2]), p(tri[0])),
        );
        for &i in tri {
            let i = i as usize * 3;
            out[i] += n[0];
            out[i + 1] += n[1];
            out[i + 2] += n[2];
        }
    }
    for n in out.as_chunks_mut::<3>().0 {
        *n = math::normalize(*n).unwrap_or([0.0, 1.0, 0.0]);
    }
}

/// A sphere around the points (centred on their bounding box; not minimal, but cheap and never
/// smaller than needed).
pub fn bounding_sphere(positions: &[f32]) -> Sphere {
    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];
    let points = positions.as_chunks::<3>().0;
    for p in points {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    if lo[0] > hi[0] {
        return Sphere {
            center: [0.0; 3],
            radius: 0.0,
        };
    }
    let center = math::scale(math::add(lo, hi), 0.5);
    let radius_sq = points
        .iter()
        .map(|&p| {
            let d = math::sub(p, center);
            math::dot(d, d)
        })
        .fold(0.0f32, f32::max);
    Sphere {
        center,
        radius: radius_sq.sqrt(),
    }
}

/// The bytes of an `f32` slice, for uploading positions without a copy.
pub fn f32_bytes(values: &[f32]) -> &[u8] {
    // SAFETY: `f32` has no padding and every bit pattern is a valid `u8`; `u8` needs no alignment;
    // the length covers exactly the same memory, borrowed for the same lifetime.
    unsafe {
        std::slice::from_raw_parts(values.as_ptr().cast::<u8>(), std::mem::size_of_val(values))
    }
}

/// Writes floats into a std140 block at a byte offset.
pub fn put_f32s(block: &mut [u8], offset: usize, values: &[f32]) {
    for (i, v) in values.iter().enumerate() {
        let at = offset + i * 4;
        block[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
}

/// Reads floats back out of a block (for tests and checks).
pub fn get_f32(block: &[u8], offset: usize) -> f32 {
    f32::from_le_bytes([
        block[offset],
        block[offset + 1],
        block[offset + 2],
        block[offset + 3],
    ])
}

/// Layout of the `Frame` uniform block (std140), written once a frame.
pub mod frame_block {
    pub const VIEW_PROJ: usize = 0;
    pub const CAMERA_POS: usize = 64;
    pub const KEY_DIR: usize = 80;
    pub const KEY_COLOR: usize = 96;
    pub const SKY: usize = 112;
    pub const GROUND: usize = 128;
    pub const SIZE: usize = 144;
}

/// Layout of the `Object` uniform block (std140), one slot per object, written when it moves.
pub mod object_block {
    pub const MODEL: usize = 0;
    /// The 3x3 normal matrix in the upper-left of a `mat4` (std140 pads `mat3` columns anyway, and
    /// some mobile drivers have mishandled `mat3` in blocks).
    pub const NORMAL: usize = 64;
    pub const SIZE: usize = 128;
}

/// Layout of the `Material` uniform block (std140), one slot per material.
pub mod material_block {
    /// Linear RGBA.
    pub const BASE_COLOR: usize = 0;
    /// x: Blinn-Phong exponent, y: metallic, z: alpha cutoff, w: specular normalisation.
    pub const PARAMS: usize = 16;
    pub const SIZE: usize = 32;
}

/// A uniform block's std140 layout: its size and each member's byte offset.
pub struct BlockLayout {
    pub name: &'static str,
    pub size: usize,
    pub uniforms: &'static [(&'static str, usize)],
}

/// The uniforms each block declares, with their std140 offsets: checked against the driver's
/// own layout when the programs link.
pub const BLOCK_LAYOUTS: [BlockLayout; 3] = [
    BlockLayout {
        name: "Frame",
        size: frame_block::SIZE,
        uniforms: &[
            ("u_view_proj", frame_block::VIEW_PROJ),
            ("u_camera_pos", frame_block::CAMERA_POS),
            ("u_key_dir", frame_block::KEY_DIR),
            ("u_key_color", frame_block::KEY_COLOR),
            ("u_sky", frame_block::SKY),
            ("u_ground", frame_block::GROUND),
        ],
    },
    BlockLayout {
        name: "Object",
        size: object_block::SIZE,
        uniforms: &[
            ("u_model", object_block::MODEL),
            ("u_normal_matrix", object_block::NORMAL),
        ],
    },
    BlockLayout {
        name: "Material",
        size: material_block::SIZE,
        uniforms: &[
            ("u_base_color", material_block::BASE_COLOR),
            ("u_material", material_block::PARAMS),
        ],
    },
];

/// The sRGB transfer function (IEC 61966-2-1), linear to encoded, for colours that bypass the
/// shader (the clear colour).
pub fn linear_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Rounds `value` up to a multiple of `align` (which is at least 1).
pub fn align_up(value: usize, align: usize) -> usize {
    let align = align.max(1);
    value.div_ceil(align) * align
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normals_pack_into_10_bit_fields_and_round_trip() {
        assert_eq!(pack_normal([1.0, 0.0, 0.0]), 511);
        assert_eq!(pack_normal([0.0, 1.0, 0.0]), 511 << 10);
        assert_eq!(pack_normal([0.0, 0.0, 1.0]), 511 << 20);
        // -1 is 0x201 in 10-bit two's complement.
        assert_eq!(pack_normal([-1.0, 0.0, 0.0]), 0x201);
        assert_eq!(pack_normal([0.0, 0.0, -1.0]) >> 30, 0, "w stays 0");
        let mut worst = 0.0f32;
        for i in 0..2000 {
            let t = i as f32 * 0.618;
            let n = math::normalize([t.sin(), (t * 1.7).cos(), (t * 0.3).sin() - 0.2]).unwrap();
            let back = unpack_normal(pack_normal(n));
            for k in 0..3 {
                worst = worst.max((back[k] - n[k]).abs());
            }
        }
        // Half a step of 1/511.
        assert!(worst <= 0.5 / 511.0 + 1e-6, "worst component error {worst}");
    }

    #[test]
    fn normals_are_renormalised_and_zero_becomes_up() {
        assert_eq!(pack_normal([0.0, 3.0, 0.0]), pack_normal([0.0, 1.0, 0.0]));
        assert_eq!(pack_normal([0.0, 0.0, 0.0]), pack_normal([0.0, 1.0, 0.0]));
        assert_eq!(
            pack_normal([f32::NAN, 0.0, 1.0]),
            pack_normal([0.0, 1.0, 0.0])
        );
    }

    #[test]
    fn uv_format_follows_range() {
        assert_eq!(uv_format(&[0.0, 1.0, 0.5, 0.25]), UvFormat::Unorm16);
        assert_eq!(uv_format(&[0.0, 1.0001]), UvFormat::Float32);
        assert_eq!(uv_format(&[-0.01, 0.5]), UvFormat::Float32);
        assert_eq!(uv_format(&[]), UvFormat::Unorm16);
    }

    #[test]
    fn attributes_interleave_normal_then_unorm16_uv() {
        let normals = [0.0, 0.0, 1.0, 1.0, 0.0, 0.0];
        let uvs = [0.0, 1.0, 0.5, 0.25];
        let mut out = vec![0xAA; 3];
        let layout = pack_attributes(&normals, Some(&uvs), &mut out);
        assert_eq!(
            layout,
            AttributeLayout {
                stride: 8,
                normal_offset: 0,
                uv: Some((UvFormat::Unorm16, 4))
            }
        );
        assert_eq!(out.len(), 16);
        assert_eq!(u32::from_le_bytes(out[0..4].try_into().unwrap()), 511 << 20);
        assert_eq!(u16::from_le_bytes([out[4], out[5]]), 0);
        assert_eq!(u16::from_le_bytes([out[6], out[7]]), 65535);
        assert_eq!(u32::from_le_bytes(out[8..12].try_into().unwrap()), 511);
        assert_eq!(u16::from_le_bytes([out[12], out[13]]), 32768);
        assert_eq!(u16::from_le_bytes([out[14], out[15]]), 16384);
        // The precision of unorm16 is far finer than a texel of a 4096 texture.
        assert!((32768.0 / 65535.0 - 0.5f32).abs() < 1.0 / 8192.0);
    }

    #[test]
    fn attributes_fall_back_to_float_uv_and_omit_missing_uv() {
        let normals = [0.0, 1.0, 0.0];
        let mut out = Vec::new();
        let layout = pack_attributes(&normals, Some(&[2.5, -1.0]), &mut out);
        assert_eq!(layout.stride, 12);
        assert_eq!(layout.uv, Some((UvFormat::Float32, 4)));
        assert_eq!(get_f32(&out, 4), 2.5);
        assert_eq!(get_f32(&out, 8), -1.0);
        let layout = pack_attributes(&normals, None, &mut out);
        assert_eq!((layout.stride, layout.uv, out.len()), (4, None, 4));
        // Offsets and strides are multiples of 4, as WebGL requires for FLOAT attributes.
        assert_eq!(UvFormat::Float32.gl_type(), (gl::FLOAT, false));
        assert_eq!(UvFormat::Unorm16.gl_type(), (gl::UNSIGNED_SHORT, true));
    }

    #[test]
    fn indices_narrow_to_u16_when_they_fit() {
        let mut out = Vec::new();
        assert_eq!(
            pack_indices(&[0, 1, 2, 2, 1, 3], 4, &mut out),
            Some(IndexFormat::U16)
        );
        assert_eq!(out, [0, 0, 1, 0, 2, 0, 2, 0, 1, 0, 3, 0]);
        // 65,535 vertices fit in u16: the largest index is 0xFFFE.
        assert_eq!(
            pack_indices(&[65534, 0, 1], 65535, &mut out),
            Some(IndexFormat::U16)
        );
        assert_eq!(&out[0..2], &[0xfe, 0xff]);
        // 65,536 do not: u16 index 0xFFFF is WebGL 2's primitive restart, always on, and the
        // triangle would vanish (the critic's GPU probe drew nothing).
        assert_eq!(
            pack_indices(&[65535, 0, 1], 65536, &mut out),
            Some(IndexFormat::U32)
        );
        assert_eq!(out.len(), 12);
        assert_eq!(u32::from_le_bytes(out[0..4].try_into().unwrap()), 65535);
        assert_eq!(
            pack_indices(&[65536, 0, 1], 65537, &mut out),
            Some(IndexFormat::U32)
        );
        assert_eq!(u32::from_le_bytes(out[0..4].try_into().unwrap()), 65536);
        // No u16 output ever contains the restart index.
        for n in [1usize, 2, 1000, MAX_U16_VERTICES] {
            if pack_indices(&[(n - 1) as u32, 0, 0], n, &mut out) == Some(IndexFormat::U16) {
                assert!(
                    out.as_chunks::<2>().0.iter().all(|c| *c != [0xff, 0xff]),
                    "{n}"
                );
            }
        }
        assert_eq!(IndexFormat::U32.gl_type(), gl::UNSIGNED_INT);
    }

    #[test]
    fn out_of_range_index_is_refused() {
        let mut out = Vec::new();
        assert_eq!(pack_indices(&[0, 1, 4], 4, &mut out), None);
    }

    #[test]
    fn computed_normals_face_out_of_a_quad_and_weigh_by_area() {
        // A unit quad in the XY plane, counter-clockwise seen from +Z.
        let positions = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 0.0];
        let mut normals = Vec::new();
        compute_normals(&positions, &[0, 1, 2, 0, 2, 3], &mut normals);
        assert_eq!(normals.len(), 12);
        for n in normals.as_chunks::<3>().0 {
            assert!(
                (n[2] - 1.0).abs() < 1e-6 && n[0].abs() < 1e-6 && n[1].abs() < 1e-6,
                "{n:?}"
            );
        }
        // A vertex shared by a big +Z face and a tiny +X face leans towards +Z.
        let positions = [
            0.0, 0.0, 0.0, 10.0, 0.0, 0.0, 0.0, 10.0, 0.0, // big, +Z
            0.0, 0.0, -0.1, 0.0, 0.1, 0.0, // tiny, sharing vertex 0
        ];
        compute_normals(&positions, &[0, 1, 2, 0, 3, 4], &mut normals);
        assert!(normals[2] > 0.99, "{:?}", &normals[0..3]);
    }

    #[test]
    fn bounding_sphere_contains_every_point() {
        let positions = [-1.0, 0.0, 0.0, 3.0, 2.0, 0.0, 1.0, -2.0, 4.0, 0.5, 0.5, 0.5];
        let s = bounding_sphere(&positions);
        assert_eq!(s.center, [1.0, 0.0, 2.0]);
        for p in positions.as_chunks::<3>().0 {
            let d = math::length(math::sub([p[0], p[1], p[2]], s.center));
            assert!(d <= s.radius + 1e-6);
        }
        assert_eq!(bounding_sphere(&[]).radius, 0.0);
    }

    #[test]
    fn f32_bytes_are_little_endian_floats() {
        let v = [1.0f32, -2.5];
        let b = f32_bytes(&v);
        assert_eq!(b.len(), 8);
        assert_eq!(&b[0..4], &1.0f32.to_le_bytes());
        assert_eq!(&b[4..8], &(-2.5f32).to_le_bytes());
    }

    #[test]
    fn std140_blocks_are_legal_and_ordered() {
        for BlockLayout {
            name: block,
            size,
            uniforms,
        } in &BLOCK_LAYOUTS
        {
            // Every member here is a vec4 or mat4: 16-byte aligned, in declaration order, packed.
            let mut next = 0;
            for (name, offset) in uniforms.iter() {
                assert_eq!(offset % 16, 0, "{block}.{name}");
                assert_eq!(*offset, next, "{block}.{name} leaves a gap");
                next = offset
                    + if name.contains("view_proj") || name.contains("matrix") || *name == "u_model"
                    {
                        64
                    } else {
                        16
                    };
            }
            assert_eq!(next, *size, "{block} size");
            assert_eq!(size % 16, 0);
        }
        let mut block = [0u8; frame_block::SIZE];
        put_f32s(&mut block, frame_block::KEY_DIR, &[0.25, -0.5, 1.0, 0.0]);
        assert_eq!(get_f32(&block, frame_block::KEY_DIR + 4), -0.5);
    }

    #[test]
    fn srgb_encoding_matches_reference_values() {
        assert_eq!(linear_to_srgb(0.0), 0.0);
        assert!((linear_to_srgb(1.0) - 1.0).abs() < 1e-6);
        assert!((linear_to_srgb(0.5) - 0.735_356_7).abs() < 1e-5);
        assert!((linear_to_srgb(0.002) - 0.025_84).abs() < 1e-6);
        assert!((linear_to_srgb(0.214_041_14) - 0.5).abs() < 1e-5);
        assert_eq!(linear_to_srgb(-1.0), 0.0);
    }

    #[test]
    fn align_up_rounds_to_multiples() {
        assert_eq!(align_up(128, 256), 256);
        assert_eq!(align_up(256, 256), 256);
        assert_eq!(align_up(144, 16), 144);
        assert_eq!(align_up(5, 0), 5);
    }
}
