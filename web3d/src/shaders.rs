//! The shader programs: GLSL ES 3.00, one source with `#define` switches, and a fixed, small set
//! of variants compiled once at start-up.
//!
//! Lighting is lean on purpose (the target GPU is a Pi 5's V3D). A fragment is shaded exactly as
//! three.js r180's `MeshPhongMaterial` would shade it under one `DirectionalLight` and one
//! `HemisphereLight`, in three's physically based units: the intensity of a three.js directional or
//! hemisphere light carries over unchanged. No other three.js light type (spot, point, rect-area,
//! an environment map) has an equivalent here yet, so a scene lit by those does not carry over.
//!
//! - diffuse: `albedo / pi` times the irradiance, from the key (`colour * intensity * N.L`) and
//!   from the hemisphere (`mix(ground, sky, 0.5 * N.y + 0.5)`);
//! - specular: three's `BRDF_BlinnPhong` (Schlick Fresnel, `G = 1/4`,
//!   `D = (n/2 + 1) / pi * (N.H)^n`) times the key's irradiance.
//!
//! The material is glTF's metallic-roughness one: `albedo = base * (1 - metallic)`, specular colour
//! `mix(0.04, base, metallic)`, and a Blinn-Phong exponent `n` from roughness by web3d's own match
//! to GGX ([`blinn_phong`]). [`shade`] is the same arithmetic on the CPU, tested against values
//! computed from three's own shader chunks (`tools/three-reference.mjs`, `tools/three-reference.txt`);
//! the GPU probe checks rendered pixels against the same values.
//!
//! One term is not three's: the specular colour times the hemisphere seen in the mirror direction,
//! as radiance (`/ pi`). It stands in for an environment map (three's `scene.environment`), so a
//! metal is not black; three.js draws no reflection at all without an environment map.
//!
//! Output is clamped, not tone-mapped (`docs/WEB3D.md`, the phase plan). Colours are
//! linear; textures are sampled through `SRGB8_ALPHA8`, so the hardware decodes them, and the
//! fragment shader encodes the result with the exact sRGB curve, because WebGL's default
//! framebuffer is not sRGB.
//!
//! Alpha-tested (`discard`) materials get their own variant, so `discard` never turns off early
//! depth testing for the others. An untextured masked material needs no variant: its alpha is a
//! constant, so it is either fully drawn or skipped on the CPU.

use std::f32::consts::PI;

use crate::math::{self, Vec3};

/// Vertex attribute locations.
pub const ATTRIB_POSITION: u32 = 0;
pub const ATTRIB_NORMAL: u32 = 1;
pub const ATTRIB_UV: u32 = 2;

/// Uniform block names and their binding points.
pub const BLOCK_FRAME: &str = "Frame";
pub const BLOCK_OBJECT: &str = "Object";
pub const BLOCK_MATERIAL: &str = "Material";
pub const BINDING_FRAME: u32 = 0;
pub const BINDING_OBJECT: u32 = 1;
pub const BINDING_MATERIAL: u32 = 2;
pub const BLOCK_BINDINGS: [(&str, u32); 3] = [
    (BLOCK_FRAME, BINDING_FRAME),
    (BLOCK_OBJECT, BINDING_OBJECT),
    (BLOCK_MATERIAL, BINDING_MATERIAL),
];

/// The base-colour sampler, on texture unit 0.
pub const SAMPLER_BASE_COLOR: &str = "u_base_color_tex";
pub const UNIT_BASE_COLOR: u32 = 0;

/// The program variants. Phase 3 adds skinned and morphed variants, phase 4 an unlit one for the
/// baked room; the set stays fixed and small (see `docs/WEB3D.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Program {
    /// Base colour only.
    Plain = 0,
    /// Base colour times a texture.
    Textured = 1,
    /// Textured, with `discard` below the alpha cutoff.
    TexturedMask = 2,
}

pub const PROGRAM_COUNT: usize = 3;
pub const PROGRAMS: [Program; PROGRAM_COUNT] =
    [Program::Plain, Program::Textured, Program::TexturedMask];

impl Program {
    pub fn name(self) -> &'static str {
        match self {
            Program::Plain => "plain",
            Program::Textured => "textured",
            Program::TexturedMask => "textured-mask",
        }
    }

    pub fn textured(self) -> bool {
        self != Program::Plain
    }

    fn defines(self) -> &'static str {
        match self {
            Program::Plain => "",
            Program::Textured => "#define HAS_TEXTURE 1\n",
            Program::TexturedMask => "#define HAS_TEXTURE 1\n#define ALPHA_MASK 1\n",
        }
    }
}

/// Shared by both stages. Every member is `highp`, so the declarations match across stages
/// whatever each stage's default precision is (a mismatch fails to link in GLSL ES).
const FRAME_BLOCK: &str = "\
layout(std140) uniform Frame {
    highp mat4 u_view_proj;
    highp vec4 u_camera_pos;
    highp vec4 u_key_dir;
    highp vec4 u_key_color;
    highp vec4 u_sky;
    highp vec4 u_ground;
};
";

const VERTEX_BODY: &str = "\
layout(std140) uniform Object {
    highp mat4 u_model;
    highp mat4 u_normal_matrix;
};
layout(location = 0) in vec3 a_position;
layout(location = 1) in vec4 a_normal;
#ifdef HAS_TEXTURE
layout(location = 2) in vec2 a_uv;
out highp vec2 v_uv;
#endif
out highp vec3 v_world;
out highp vec3 v_normal;
void main() {
    vec4 world = u_model * vec4(a_position, 1.0);
    v_world = world.xyz;
    v_normal = mat3(u_normal_matrix) * a_normal.xyz;
#ifdef HAS_TEXTURE
    v_uv = a_uv;
#endif
    gl_Position = u_view_proj * world;
}
";

// Texture coordinates stay highp: at mediump a 4096-texel texture would be addressed in steps of
// two texels.
const FRAGMENT_BODY: &str = "\
layout(std140) uniform Material {
    mediump vec4 u_base_color;
    highp vec4 u_material;
};
#ifdef HAS_TEXTURE
uniform mediump sampler2D u_base_color_tex;
in highp vec2 v_uv;
#endif
in highp vec3 v_world;
in highp vec3 v_normal;
layout(location = 0) out mediump vec4 o_color;

vec3 srgb_encode(vec3 c) {
    c = clamp(c, 0.0, 1.0);
    vec3 lo = c * 12.92;
    vec3 hi = 1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055;
    return mix(hi, lo, vec3(lessThanEqual(c, vec3(0.0031308))));
}

const float RECIPROCAL_PI = 0.3183098861837907;

void main() {
    vec4 base = u_base_color;
#ifdef HAS_TEXTURE
    base *= texture(u_base_color_tex, v_uv);
#endif
#ifdef ALPHA_MASK
    if (base.a < u_material.z) discard;
#endif
    // Directions and the highlight in highp: at mediump (fp16, where a driver honours it) N.H
    // moves in steps of 1/2048 near 1, and pow(N.H, 2048), a sharp catchlight, would band.
    highp vec3 n = normalize(v_normal);
    if (!gl_FrontFacing) n = -n;
    highp vec3 l = u_key_dir.xyz;
    highp vec3 v = normalize(u_camera_pos.xyz - v_world);
    highp vec3 h = normalize(l + v);
    float ndl = clamp(dot(n, l), 0.0, 1.0);
    highp float ndh = clamp(dot(n, h), 0.0, 1.0);
    float vdh = clamp(dot(v, h), 0.0, 1.0);
    float metallic = u_material.y;
    vec3 albedo = base.rgb * (1.0 - metallic);
    vec3 f0 = mix(vec3(0.04), base.rgb, metallic);
    // three.js r180: F_Schlick (f90 = 1), then G * D = u_material.w * (N.H)^n.
    float fresnel = exp2((-5.55473 * vdh - 6.98316) * vdh);
    highp float lobe = u_material.w * pow(ndh, u_material.x);
    vec3 specular = (f0 * (1.0 - fresnel) + fresnel) * lobe;
    vec3 key = u_key_color.rgb * ndl;
    vec3 hemi = mix(u_ground.rgb, u_sky.rgb, 0.5 * n.y + 0.5);
    vec3 env = mix(u_ground.rgb, u_sky.rgb, 0.5 * reflect(-v, n).y + 0.5);
    vec3 color = (key + hemi) * (albedo * RECIPROCAL_PI) + key * specular + f0 * env * RECIPROCAL_PI;
    o_color = vec4(srgb_encode(color), base.a);
}
";

pub fn vertex_source(program: Program) -> String {
    format!(
        "#version 300 es\n{}precision highp float;\n{FRAME_BLOCK}{VERTEX_BODY}",
        program.defines()
    )
}

pub fn fragment_source(program: Program) -> String {
    format!(
        "#version 300 es\n{}precision mediump float;\n{FRAME_BLOCK}{FRAGMENT_BODY}",
        program.defines()
    )
}

/// The Blinn-Phong exponent for a roughness, by the usual match to GGX (`alpha = roughness^2`,
/// `n = 2 / alpha^2 - 2`), kept within 1..=2048 (web3d's choice; three.js has no such mapping);
/// and three.js's `G_BlinnPhong_Implicit * D_BlinnPhong` factor without the `(N.H)^n`:
/// `0.25 * (n / 2 + 1) / pi`.
pub fn blinn_phong(roughness: f32) -> (f32, f32) {
    let r = roughness.clamp(0.0, 1.0).max(0.03);
    let alpha2 = (r * r) * (r * r);
    let n = (2.0 / alpha2 - 2.0).clamp(1.0, 2048.0);
    (n, 0.25 * (n * 0.5 + 1.0) / PI)
}

/// A surface point, for [`shade`].
#[derive(Clone, Copy, Debug)]
pub struct Surface {
    /// Linear base colour (after the texture).
    pub base: Vec3,
    pub metallic: f32,
    pub roughness: f32,
    /// Unit normal, facing the viewer's side.
    pub normal: Vec3,
    /// Unit vector towards the viewer.
    pub view: Vec3,
}

/// The lights, in three.js r180 units: colours times intensities, linear.
#[derive(Clone, Copy, Debug)]
pub struct Lights {
    /// Unit vector towards the key light.
    pub key_dir: Vec3,
    pub key: Vec3,
    pub sky: Vec3,
    pub ground: Vec3,
}

/// The fragment shader's lighting on the CPU, step for step, before clamping and encoding. The
/// first value is what three.js r180 computes for the same surface and lights (direct diffuse,
/// direct specular, hemisphere diffuse); the second is web3d's environment stand-in, which three
/// adds only with an environment map. Their sum is what the shader writes.
pub fn shade(s: &Surface, lights: &Lights) -> (Vec3, Vec3) {
    let (n, v, l) = (s.normal, s.view, lights.key_dir);
    let h = math::normalize(math::add(l, v)).unwrap_or(n);
    let ndl = math::dot(n, l).clamp(0.0, 1.0);
    let ndh = math::dot(n, h).clamp(0.0, 1.0);
    let vdh = math::dot(v, h).clamp(0.0, 1.0);
    let (shininess, gd) = blinn_phong(s.roughness);
    let metallic = s.metallic.clamp(0.0, 1.0);
    let mix = |a: f32, b: f32, t: f32| a * (1.0 - t) + b * t;
    let fresnel = ((-5.55473 * vdh - 6.98316) * vdh).exp2();
    let lobe = gd * ndh.powf(shininess);
    let hemi_t = 0.5 * n[1] + 0.5;
    let reflected_y = 2.0 * math::dot(n, v) * n[1] - v[1];
    let env_t = 0.5 * reflected_y + 0.5;
    let mut three = [0.0; 3];
    let mut env = [0.0; 3];
    for c in 0..3 {
        let albedo = s.base[c] * (1.0 - metallic);
        let f0 = mix(0.04, s.base[c], metallic);
        let key = lights.key[c] * ndl;
        let hemi = mix(lights.ground[c], lights.sky[c], hemi_t);
        let specular = (f0 * (1.0 - fresnel) + fresnel) * lobe;
        three[c] = (key + hemi) * albedo / PI + key * specular;
        env[c] = f0 * mix(lights.ground[c], lights.sky[c], env_t) / PI;
    }
    (three, env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack;

    #[test]
    fn sources_start_with_version_and_switch_features() {
        for p in PROGRAMS {
            let vs = vertex_source(p);
            let fs = fragment_source(p);
            assert!(vs.starts_with("#version 300 es\n"), "{}", p.name());
            assert!(fs.starts_with("#version 300 es\n"), "{}", p.name());
            assert_eq!(vs.contains("#define HAS_TEXTURE"), p.textured());
            assert_eq!(
                fs.contains("#define ALPHA_MASK"),
                p == Program::TexturedMask
            );
        }
        assert_eq!(PROGRAMS.len(), PROGRAM_COUNT);
        for (i, p) in PROGRAMS.iter().enumerate() {
            assert_eq!(*p as usize, i);
        }
    }

    #[test]
    fn every_block_member_is_declared_in_the_source() {
        let all = format!("{FRAME_BLOCK}{VERTEX_BODY}{FRAGMENT_BODY}");
        for layout in &pack::BLOCK_LAYOUTS {
            let block = layout.name;
            for (name, _) in layout.uniforms {
                assert!(
                    all.contains(&format!(" {name};")),
                    "{block}.{name} missing from the GLSL"
                );
            }
        }
        for (block, _) in BLOCK_BINDINGS {
            assert!(all.contains(&format!("uniform {block} {{")), "{block}");
        }
        assert!(all.contains(SAMPLER_BASE_COLOR));
        // Locations in the GLSL match the constants the packer binds.
        assert!(all.contains(&format!("location = {ATTRIB_POSITION}) in vec3 a_position")));
        assert!(all.contains(&format!("location = {ATTRIB_NORMAL}) in vec4 a_normal")));
        assert!(all.contains(&format!("location = {ATTRIB_UV}) in vec2 a_uv")));
    }

    #[test]
    fn blinn_phong_narrows_with_smoothness() {
        let (rough, _) = blinn_phong(1.0);
        let (mid, _) = blinn_phong(0.5);
        let (smooth, gd) = blinn_phong(0.0);
        assert_eq!(rough, 1.0);
        assert!((mid - 30.0).abs() < 1e-3, "{mid}");
        assert_eq!(smooth, 2048.0);
        // three's G (1/4) times D's normalisation (n/2 + 1) / pi.
        assert!((gd - 0.25 * 1025.0 / PI).abs() < 1e-3, "{gd}");
        assert_eq!(
            blinn_phong(f32::NAN).0,
            2048.0,
            "NaN roughness reads as smooth, not NaN"
        );
    }

    /// The CPU mirror of the shader against three.js r180's formulas, case by case
    /// (`tools/three-reference.txt`, written by `tools/three-reference.mjs` from three's chunks).
    #[test]
    fn shading_matches_three_js_r180() {
        let reference = include_str!("../tools/three-reference.txt");
        let mut cases = 0;
        for line in reference
            .lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        {
            let mut fields = line.split_whitespace();
            let name = fields.next().unwrap();
            let v: Vec<f32> = fields.map(|f| f.parse().unwrap()).collect();
            assert_eq!(v.len(), 26, "{name}");
            let v3 = |i: usize| [v[i], v[i + 1], v[i + 2]];
            let surface = Surface {
                base: v3(0),
                metallic: v[3],
                roughness: v[4],
                normal: v3(5),
                view: v3(11),
            };
            let lights = Lights {
                key_dir: v3(8),
                key: v3(14),
                sky: v3(17),
                ground: v3(20),
            };
            let (three, env) = shade(&surface, &lights);
            let expected = v3(23);
            for c in 0..3 {
                let tolerance = 2e-5 * expected[c].abs().max(1.0);
                assert!(
                    (three[c] - expected[c]).abs() <= tolerance,
                    "{name} channel {c}: web3d {} against three.js {}",
                    three[c],
                    expected[c]
                );
                assert!(env[c] >= 0.0);
            }
            cases += 1;
        }
        assert!(cases >= 7, "only {cases} reference cases");
    }

    #[test]
    fn environment_stand_in_is_specular_colour_times_hemisphere_radiance() {
        // A uniform hemisphere of irradiance E: a white dielectric gets E/pi of diffuse and
        // 0.04 E/pi of reflection; a metal only the reflection, in its own colour.
        let lights = Lights {
            key_dir: [0.0, 1.0, 0.0],
            key: [0.0; 3],
            sky: [2.0; 3],
            ground: [2.0; 3],
        };
        let up = [0.0, 0.0, 1.0];
        let white = Surface {
            base: [1.0; 3],
            metallic: 0.0,
            roughness: 1.0,
            normal: up,
            view: up,
        };
        let (three, env) = shade(&white, &lights);
        assert!((three[0] - 2.0 / PI).abs() < 1e-6);
        assert!((env[0] - 0.04 * 2.0 / PI).abs() < 1e-6);
        let gold = Surface {
            base: [1.0, 0.77, 0.34],
            metallic: 1.0,
            ..white
        };
        let (three, env) = shade(&gold, &lights);
        assert_eq!(three, [0.0; 3], "no diffuse, and no key light");
        assert!((env[1] - 0.77 * 2.0 / PI).abs() < 1e-6);
    }

    #[test]
    fn the_shader_text_carries_the_same_constants() {
        let fs = fragment_source(Program::Plain);
        for needle in [
            "exp2((-5.55473 * vdh - 6.98316) * vdh)",
            "highp float lobe = u_material.w * pow(ndh, u_material.x);",
            "(f0 * (1.0 - fresnel) + fresnel) * lobe",
            "highp float ndh = clamp(dot(n, h), 0.0, 1.0);",
            "(key + hemi) * (albedo * RECIPROCAL_PI) + key * specular + f0 * env * RECIPROCAL_PI",
            "mix(u_ground.rgb, u_sky.rgb, 0.5 * n.y + 0.5)",
            "mix(vec3(0.04), base.rgb, metallic)",
        ] {
            assert!(fs.contains(needle), "the fragment shader lost: {needle}");
        }
    }
}
