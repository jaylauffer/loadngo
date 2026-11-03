//! Matrix and vector maths for the renderer.
//!
//! Matrices are 4x4, `f32`, column-major, laid out as WebGL and three.js (`Matrix4.elements`) lay
//! them out: the element in row `r`, column `c` is at index `c * 4 + r`. Matrices from a three.js
//! scene therefore pass through unchanged. Clip space is OpenGL's (`z` from -1 to 1) and
//! the camera looks down -Z, as in three.js.

pub type Vec3 = [f32; 3];
pub type Mat4 = [f32; 16];
/// A 3x3 matrix, column-major.
pub type Mat3 = [f32; 9];
/// A plane `a*x + b*y + c*z + d = 0` with a unit normal `(a, b, c)` pointing inside.
pub type Plane = [f32; 4];

#[rustfmt::skip]
pub const IDENTITY: Mat4 = [
    1.0, 0.0, 0.0, 0.0,
    0.0, 1.0, 0.0, 0.0,
    0.0, 0.0, 1.0, 0.0,
    0.0, 0.0, 0.0, 1.0,
];

pub fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub fn scale(a: Vec3, s: f32) -> Vec3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

pub fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub fn length(a: Vec3) -> f32 {
    dot(a, a).sqrt()
}

/// `a` scaled to unit length, or `None` when it is (nearly) zero or not finite.
pub fn normalize(a: Vec3) -> Option<Vec3> {
    let len = length(a);
    (len.is_finite() && len > 1e-12).then(|| scale(a, 1.0 / len))
}

/// `a * b`: applying the result applies `b` first, then `a`.
pub fn mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut out = [0.0; 16];
    for c in 0..4 {
        for r in 0..4 {
            out[c * 4 + r] = a[r] * b[c * 4]
                + a[4 + r] * b[c * 4 + 1]
                + a[8 + r] * b[c * 4 + 2]
                + a[12 + r] * b[c * 4 + 3];
        }
    }
    out
}

/// A perspective projection, as three.js's `PerspectiveCamera` builds it: vertical field of view
/// in radians, width over height, and the near and far planes (both positive, near < far).
pub fn perspective(fov_y: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
    let f = 1.0 / (fov_y * 0.5).tan();
    let nf = 1.0 / (near - far);
    let mut m = [0.0; 16];
    m[0] = f / aspect;
    m[5] = f;
    m[10] = (far + near) * nf;
    m[11] = -1.0;
    m[14] = 2.0 * far * near * nf;
    m
}

/// A view matrix for a camera at `eye` looking at `target`, with `up` as the rough up direction.
/// When `up` is parallel to the view direction another axis stands in for it, so the result is
/// always a rotation and translation.
pub fn look_at(eye: Vec3, target: Vec3, up: Vec3) -> Mat4 {
    let z = normalize(sub(eye, target)).unwrap_or([0.0, 0.0, 1.0]);
    let x = normalize(cross(up, z)).unwrap_or_else(|| {
        let fallback = if z[1].abs() < 0.9 {
            [0.0, 1.0, 0.0]
        } else {
            [1.0, 0.0, 0.0]
        };
        normalize(cross(fallback, z)).unwrap_or([1.0, 0.0, 0.0])
    });
    let y = cross(z, x);
    [
        x[0],
        y[0],
        z[0],
        0.0,
        x[1],
        y[1],
        z[1],
        0.0,
        x[2],
        y[2],
        z[2],
        0.0,
        -dot(x, eye),
        -dot(y, eye),
        -dot(z, eye),
        1.0,
    ]
}

/// The inverse of a general 4x4 matrix, or `None` when it is singular or not finite. (Cofactor
/// expansion; it is layout-agnostic, since the inverse of a transpose is the transpose of the
/// inverse.)
pub fn inverse(m: &Mat4) -> Option<Mat4> {
    let mut inv = [0.0f32; 16];
    inv[0] = m[5] * m[10] * m[15] - m[5] * m[11] * m[14] - m[9] * m[6] * m[15]
        + m[9] * m[7] * m[14]
        + m[13] * m[6] * m[11]
        - m[13] * m[7] * m[10];
    inv[4] = -m[4] * m[10] * m[15] + m[4] * m[11] * m[14] + m[8] * m[6] * m[15]
        - m[8] * m[7] * m[14]
        - m[12] * m[6] * m[11]
        + m[12] * m[7] * m[10];
    inv[8] = m[4] * m[9] * m[15] - m[4] * m[11] * m[13] - m[8] * m[5] * m[15]
        + m[8] * m[7] * m[13]
        + m[12] * m[5] * m[11]
        - m[12] * m[7] * m[9];
    inv[12] = -m[4] * m[9] * m[14] + m[4] * m[10] * m[13] + m[8] * m[5] * m[14]
        - m[8] * m[6] * m[13]
        - m[12] * m[5] * m[10]
        + m[12] * m[6] * m[9];
    inv[1] = -m[1] * m[10] * m[15] + m[1] * m[11] * m[14] + m[9] * m[2] * m[15]
        - m[9] * m[3] * m[14]
        - m[13] * m[2] * m[11]
        + m[13] * m[3] * m[10];
    inv[5] = m[0] * m[10] * m[15] - m[0] * m[11] * m[14] - m[8] * m[2] * m[15]
        + m[8] * m[3] * m[14]
        + m[12] * m[2] * m[11]
        - m[12] * m[3] * m[10];
    inv[9] = -m[0] * m[9] * m[15] + m[0] * m[11] * m[13] + m[8] * m[1] * m[15]
        - m[8] * m[3] * m[13]
        - m[12] * m[1] * m[11]
        + m[12] * m[3] * m[9];
    inv[13] = m[0] * m[9] * m[14] - m[0] * m[10] * m[13] - m[8] * m[1] * m[14]
        + m[8] * m[2] * m[13]
        + m[12] * m[1] * m[10]
        - m[12] * m[2] * m[9];
    inv[2] = m[1] * m[6] * m[15] - m[1] * m[7] * m[14] - m[5] * m[2] * m[15]
        + m[5] * m[3] * m[14]
        + m[13] * m[2] * m[7]
        - m[13] * m[3] * m[6];
    inv[6] = -m[0] * m[6] * m[15] + m[0] * m[7] * m[14] + m[4] * m[2] * m[15]
        - m[4] * m[3] * m[14]
        - m[12] * m[2] * m[7]
        + m[12] * m[3] * m[6];
    inv[10] = m[0] * m[5] * m[15] - m[0] * m[7] * m[13] - m[4] * m[1] * m[15]
        + m[4] * m[3] * m[13]
        + m[12] * m[1] * m[7]
        - m[12] * m[3] * m[5];
    inv[14] = -m[0] * m[5] * m[14] + m[0] * m[6] * m[13] + m[4] * m[1] * m[14]
        - m[4] * m[2] * m[13]
        - m[12] * m[1] * m[6]
        + m[12] * m[2] * m[5];
    inv[3] = -m[1] * m[6] * m[11] + m[1] * m[7] * m[10] + m[5] * m[2] * m[11]
        - m[5] * m[3] * m[10]
        - m[9] * m[2] * m[7]
        + m[9] * m[3] * m[6];
    inv[7] = m[0] * m[6] * m[11] - m[0] * m[7] * m[10] - m[4] * m[2] * m[11]
        + m[4] * m[3] * m[10]
        + m[8] * m[2] * m[7]
        - m[8] * m[3] * m[6];
    inv[11] = -m[0] * m[5] * m[11] + m[0] * m[7] * m[9] + m[4] * m[1] * m[11]
        - m[4] * m[3] * m[9]
        - m[8] * m[1] * m[7]
        + m[8] * m[3] * m[5];
    inv[15] = m[0] * m[5] * m[10] - m[0] * m[6] * m[9] - m[4] * m[1] * m[10]
        + m[4] * m[2] * m[9]
        + m[8] * m[1] * m[6]
        - m[8] * m[2] * m[5];

    let det = m[0] * inv[0] + m[1] * inv[4] + m[2] * inv[8] + m[3] * inv[12];
    if !det.is_finite() || det.abs() < 1e-30 {
        return None;
    }
    let inv_det = 1.0 / det;
    for v in &mut inv {
        *v *= inv_det;
    }
    inv.iter().all(|v| v.is_finite()).then_some(inv)
}

fn column(m: &Mat4, c: usize) -> Vec3 {
    [m[c * 4], m[c * 4 + 1], m[c * 4 + 2]]
}

/// The determinant of the upper-left 3x3 (the linear part of an affine transform). Negative for a
/// mirroring transform, which turns triangle winding over.
pub fn determinant3(m: &Mat4) -> f32 {
    dot(column(m, 0), cross(column(m, 1), column(m, 2)))
}

/// The normal matrix: the inverse-transpose of the upper-left 3x3, which keeps normals
/// perpendicular to surfaces under non-uniform scale. `None` when the 3x3 is singular.
///
/// For a 3x3 `A` with columns `a`, `b`, `c`, the rows of `A^-1` are `b x c`, `c x a` and `a x b`
/// over `det A`; so those vectors are the columns of `(A^-1)^T`.
pub fn normal_matrix(m: &Mat4) -> Option<Mat3> {
    let (a, b, c) = (column(m, 0), column(m, 1), column(m, 2));
    let bc = cross(b, c);
    let det = dot(a, bc);
    if !det.is_finite() || det.abs() < 1e-30 {
        return None;
    }
    let inv = 1.0 / det;
    let (x, y, z) = (
        scale(bc, inv),
        scale(cross(c, a), inv),
        scale(cross(a, b), inv),
    );
    let out = [x[0], x[1], x[2], y[0], y[1], y[2], z[0], z[1], z[2]];
    out.iter().all(|v| v.is_finite()).then_some(out)
}

/// `m * (p, 1)`, dropping `w` (for affine transforms).
pub fn transform_point(m: &Mat4, p: Vec3) -> Vec3 {
    [
        m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12],
        m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13],
        m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14],
    ]
}

/// `m3 * v` for a column-major 3x3.
pub fn transform_vector3(m: &Mat3, v: Vec3) -> Vec3 {
    [
        m[0] * v[0] + m[3] * v[1] + m[6] * v[2],
        m[1] * v[0] + m[4] * v[1] + m[7] * v[2],
        m[2] * v[0] + m[5] * v[1] + m[8] * v[2],
    ]
}

/// The largest axis scale of an affine transform, which bounds how much it can grow a sphere.
pub fn max_scale(m: &Mat4) -> f32 {
    length(column(m, 0))
        .max(length(column(m, 1)))
        .max(length(column(m, 2)))
}

/// The six clip planes of a view-projection matrix (Gribb and Hartmann), normals pointing inside:
/// left, right, bottom, top, near, far.
pub fn frustum_planes(view_proj: &Mat4) -> [Plane; 6] {
    let row = |i: usize| -> [f32; 4] {
        [
            view_proj[i],
            view_proj[4 + i],
            view_proj[8 + i],
            view_proj[12 + i],
        ]
    };
    let (r0, r1, r2, r3) = (row(0), row(1), row(2), row(3));
    let combine = |a: [f32; 4], b: [f32; 4], s: f32| -> Plane {
        let p = [
            a[0] + s * b[0],
            a[1] + s * b[1],
            a[2] + s * b[2],
            a[3] + s * b[3],
        ];
        let len = length([p[0], p[1], p[2]]);
        if len > 0.0 && len.is_finite() {
            [p[0] / len, p[1] / len, p[2] / len, p[3] / len]
        } else {
            // A degenerate plane culls nothing.
            [0.0, 0.0, 0.0, 1.0]
        }
    };
    [
        combine(r3, r0, 1.0),
        combine(r3, r0, -1.0),
        combine(r3, r1, 1.0),
        combine(r3, r1, -1.0),
        combine(r3, r2, 1.0),
        combine(r3, r2, -1.0),
    ]
}

/// Whether a sphere is at least partly inside all six planes.
pub fn sphere_visible(planes: &[Plane; 6], center: Vec3, radius: f32) -> bool {
    planes
        .iter()
        .all(|p| p[0] * center[0] + p[1] * center[1] + p[2] * center[2] + p[3] >= -radius)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 1e-5;

    fn close(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() <= eps * (1.0 + a.abs().max(b.abs()))
    }

    fn assert_mat_close(a: &Mat4, b: &Mat4, eps: f32) {
        for i in 0..16 {
            assert!(
                close(a[i], b[i], eps),
                "element {i}: {} vs {}\n{a:?}\n{b:?}",
                a[i],
                b[i]
            );
        }
    }

    fn translation(t: Vec3) -> Mat4 {
        let mut m = IDENTITY;
        m[12] = t[0];
        m[13] = t[1];
        m[14] = t[2];
        m
    }

    fn scaling(s: Vec3) -> Mat4 {
        let mut m = IDENTITY;
        m[0] = s[0];
        m[5] = s[1];
        m[10] = s[2];
        m
    }

    fn rotation_y(a: f32) -> Mat4 {
        let (s, c) = a.sin_cos();
        let mut m = IDENTITY;
        m[0] = c;
        m[2] = -s;
        m[8] = s;
        m[10] = c;
        m
    }

    fn rotation_x(a: f32) -> Mat4 {
        let (s, c) = a.sin_cos();
        let mut m = IDENTITY;
        m[5] = c;
        m[6] = s;
        m[9] = -s;
        m[10] = c;
        m
    }

    fn project(m: &Mat4, p: Vec3) -> Vec3 {
        let x = m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12];
        let y = m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13];
        let z = m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14];
        let w = m[3] * p[0] + m[7] * p[1] + m[11] * p[2] + m[15];
        [x / w, y / w, z / w]
    }

    #[test]
    fn multiply_identity_and_known_product() {
        let a = mul(&translation([1.0, 2.0, 3.0]), &scaling([2.0, 3.0, 4.0]));
        assert_mat_close(&mul(&IDENTITY, &a), &a, 0.0);
        assert_mat_close(&mul(&a, &IDENTITY), &a, 0.0);
        // Translate after scale: a point (1, 1, 1) goes to (2, 3, 4) + (1, 2, 3).
        assert_eq!(transform_point(&a, [1.0, 1.0, 1.0]), [3.0, 5.0, 7.0]);
        // Column-major storage: the translation sits in elements 12..15.
        assert_eq!(&a[12..16], &[1.0, 2.0, 3.0, 1.0]);
        assert_eq!([a[0], a[5], a[10]], [2.0, 3.0, 4.0]);
    }

    #[test]
    fn multiply_is_associative_and_not_commutative() {
        let a = mul(&rotation_y(0.7), &translation([1.0, -2.0, 0.5]));
        let b = mul(&scaling([1.5, 0.5, 2.0]), &rotation_x(-0.3));
        let c = translation([0.25, 4.0, -3.0]);
        assert_mat_close(&mul(&mul(&a, &b), &c), &mul(&a, &mul(&b, &c)), 1e-5);
        let ab = mul(&a, &b);
        let ba = mul(&b, &a);
        assert!((0..16).any(|i| !close(ab[i], ba[i], 1e-3)));
    }

    #[test]
    fn perspective_maps_near_and_far_planes_and_field_of_view() {
        let fov = 50f32.to_radians();
        let (near, far, aspect) = (0.1, 100.0, 16.0 / 9.0);
        let p = perspective(fov, aspect, near, far);
        assert!(close(project(&p, [0.0, 0.0, -near])[2], -1.0, EPS));
        assert!(close(project(&p, [0.0, 0.0, -far])[2], 1.0, 1e-4));
        // A point on the top edge of the field of view lands on y = 1.
        let d = 5.0;
        let top = project(&p, [0.0, d * (fov * 0.5).tan(), -d]);
        assert!(close(top[1], 1.0, EPS), "{top:?}");
        let right = project(&p, [d * (fov * 0.5).tan() * aspect, 0.0, -d]);
        assert!(close(right[0], 1.0, EPS), "{right:?}");
        // Same elements as three.js's makePerspective for this camera.
        assert!(close(p[11], -1.0, 0.0) && p[15] == 0.0);
    }

    #[test]
    fn look_at_puts_eye_at_origin_and_target_down_negative_z() {
        let eye = [3.0, 4.0, 5.0];
        let target = [-1.0, 0.5, 2.0];
        let view = look_at(eye, target, [0.0, 1.0, 0.0]);
        let e = transform_point(&view, eye);
        assert!(e.iter().all(|v| v.abs() < 1e-5), "{e:?}");
        let t = transform_point(&view, target);
        let dist = length(sub(eye, target));
        assert!(
            t[0].abs() < 1e-5 && t[1].abs() < 1e-5 && close(t[2], -dist, EPS),
            "{t:?}"
        );
        // World up stays in the upper half of the view.
        let up = transform_point(&view, add(eye, [0.0, 1.0, 0.0]));
        assert!(up[1] > 0.0 && up[0].abs() < 1e-5, "{up:?}");
        // Rotation part is orthonormal: determinant 1.
        assert!(close(determinant3(&view), 1.0, EPS));
    }

    #[test]
    fn look_at_survives_up_parallel_to_view() {
        let view = look_at([0.0, 10.0, 0.0], [0.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        assert!(view.iter().all(|v| v.is_finite()));
        assert!(close(determinant3(&view), 1.0, EPS));
        let t = transform_point(&view, [0.0, 0.0, 0.0]);
        assert!(close(t[2], -10.0, EPS), "{t:?}");
    }

    #[test]
    fn inverse_undoes_a_transform_and_rejects_singular() {
        let m = mul(
            &mul(&translation([4.0, -1.0, 2.5]), &rotation_y(1.1)),
            &mul(&rotation_x(0.4), &scaling([2.0, 0.5, 3.0])),
        );
        let inv = inverse(&m).expect("invertible");
        assert_mat_close(&mul(&m, &inv), &IDENTITY, 1e-5);
        assert_mat_close(&mul(&inv, &m), &IDENTITY, 1e-5);
        // A projection is invertible too (the renderer inverts views, but keep it general).
        let p = perspective(1.0, 1.5, 0.1, 50.0);
        assert_mat_close(&mul(&p, &inverse(&p).unwrap()), &IDENTITY, 1e-4);
        assert!(inverse(&scaling([1.0, 0.0, 1.0])).is_none());
        let mut nan = IDENTITY;
        nan[5] = f32::NAN;
        assert!(inverse(&nan).is_none());
    }

    #[test]
    fn normal_matrix_keeps_normals_perpendicular_under_non_uniform_scale() {
        let m = mul(
            &mul(&translation([1.0, 2.0, 3.0]), &rotation_x(0.6)),
            &mul(&scaling([3.0, 0.5, 1.0]), &rotation_y(0.9)),
        );
        let n = normal_matrix(&m).unwrap();
        // A surface spanned by two tangents, and its normal.
        let (t1, t2) = ([1.0, 0.2, -0.3], [0.1, 1.0, 0.4]);
        let normal = cross(t1, t2);
        let linear = |v: Vec3| sub(transform_point(&m, v), transform_point(&m, [0.0; 3]));
        let nn = transform_vector3(&n, normal);
        assert!(dot(nn, linear(t1)).abs() < 1e-4, "{}", dot(nn, linear(t1)));
        assert!(dot(nn, linear(t2)).abs() < 1e-4, "{}", dot(nn, linear(t2)));
        // The plain 3x3 would not keep them perpendicular.
        let plain = [m[0], m[1], m[2], m[4], m[5], m[6], m[8], m[9], m[10]];
        assert!(dot(transform_vector3(&plain, normal), linear(t1)).abs() > 1e-2);
        // Matches the transpose of the general inverse.
        let inv = inverse(&m).unwrap();
        let expected = [
            inv[0], inv[4], inv[8], inv[1], inv[5], inv[9], inv[2], inv[6], inv[10],
        ];
        for i in 0..9 {
            assert!(
                close(n[i], expected[i], 1e-5),
                "{i}: {} vs {}",
                n[i],
                expected[i]
            );
        }
        assert!(normal_matrix(&scaling([0.0, 1.0, 1.0])).is_none());
    }

    #[test]
    fn mirroring_has_negative_determinant() {
        assert!(determinant3(&scaling([-1.0, 1.0, 1.0])) < 0.0);
        assert!(determinant3(&rotation_y(2.0)) > 0.0);
        assert!(close(
            max_scale(&mul(&rotation_x(0.3), &scaling([1.0, 4.0, 2.0]))),
            4.0,
            EPS
        ));
    }

    #[test]
    fn frustum_culls_spheres_outside() {
        let view = look_at([0.0, 0.0, 10.0], [0.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        let proj = perspective(60f32.to_radians(), 1.0, 0.5, 50.0);
        let planes = frustum_planes(&mul(&proj, &view));
        assert!(sphere_visible(&planes, [0.0, 0.0, 0.0], 1.0));
        // Behind the camera.
        assert!(!sphere_visible(&planes, [0.0, 0.0, 20.0], 1.0));
        // Beyond the far plane.
        assert!(!sphere_visible(&planes, [0.0, 0.0, -45.0], 1.0));
        // Far off to the side; then a big one overlapping the left edge.
        assert!(!sphere_visible(&planes, [30.0, 0.0, 0.0], 1.0));
        assert!(sphere_visible(&planes, [-7.0, 0.0, 0.0], 2.0));
        assert!(!sphere_visible(&planes, [-7.0, 0.0, 0.0], 0.5));
        // Planes are normalised: distance from the near plane is in world units.
        let near = planes[4];
        let d = near[0] * 0.0 + near[1] * 0.0 + near[2] * 0.0 + near[3];
        assert!(close(d, 9.5, 1e-4), "{d}");
    }
}
