//! The wasm32 boundary: plain C-ABI functions over numbers and pointers into this
//! module's memory, as loadngo-web3d does (no wasm-bindgen, no imports at all).
//!
//! - Bytes go in through [`anim_alloc`]: the page copies a database file into the returned
//!   region and passes the pointer and length to [`anim_database_load`].
//! - A character's pose comes out through one buffer the character owns
//!   ([`anim_character_pose`]): the page keeps a `Float32Array` view over it, and every
//!   [`anim_character_step`] rewrites it in place. A step allocates nothing on either side,
//!   so the view stays valid; memory grows only when a database or character is made, after
//!   which the page remakes its views (`js/motion.js` does).
//! - Fallible calls return 0 on success and 1 on failure, creating calls a handle or 0; the
//!   reason is at [`anim_error_ptr`] / [`anim_error_len`] (UTF-8).
//!
//! Pose buffer, `7 + 4 * bones` numbers: the root's `x`, `z` and facing (radians about +Y,
//! 0 = +Z), the feet held (bit 0 left, bit 1 right, as a number), the hips' position
//! relative to the root (`x`, `y`, `z`), then each driven bone's rotation `x, y, z, w`:
//! the hips' relative to the root, every other bone's relative to its parent.

use std::cell::RefCell;
use std::sync::Arc;

use crate::controller::{Config, MotionMatcher};
use crate::database::Database;
use crate::math::Vec3;
use crate::path::{ConstantSteering, PathSteering, Steering, MAX_PATH_POINTS};

thread_local! {
    static ERROR: RefCell<String> = const { RefCell::new(String::new()) };
}

fn fail(msg: impl Into<String>) {
    ERROR.with(|e| *e.borrow_mut() = msg.into());
}

/// The last error's UTF-8 bytes.
#[no_mangle]
pub extern "C" fn anim_error_ptr() -> *const u8 {
    ERROR.with(|e| e.borrow().as_ptr())
}

/// The last error's length in bytes.
#[no_mangle]
pub extern "C" fn anim_error_len() -> usize {
    ERROR.with(|e| e.borrow().len())
}

/// A zeroed region of `len` bytes for the page to fill, or null.
#[no_mangle]
pub extern "C" fn anim_alloc(len: usize) -> *mut u8 {
    let mut v = vec![0u8; len.max(1)].into_boxed_slice();
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

/// Frees a region from [`anim_alloc`] of the same `len`.
///
/// # Safety
/// `ptr` and `len` must come from one [`anim_alloc`] call, freed once.
#[no_mangle]
pub unsafe extern "C" fn anim_free(ptr: *mut u8, len: usize) {
    if ptr.is_null() {
        return;
    }
    drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len.max(1))) });
}

/// A loaded database.
pub struct DatabaseHandle {
    db: Arc<Database>,
}

/// Reads a database file from `len` bytes at `ptr` (checking its hash and version);
/// returns a handle or null.
///
/// # Safety
/// `ptr` must point at `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn anim_database_load(ptr: *const u8, len: usize) -> *mut DatabaseHandle {
    if ptr.is_null() {
        fail("no bytes");
        return std::ptr::null_mut();
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    match Database::from_bytes(bytes) {
        Ok(db) => Box::into_raw(Box::new(DatabaseHandle { db: Arc::new(db) })),
        Err(e) => {
            fail(e.to_string());
            std::ptr::null_mut()
        }
    }
}

/// Frees a database. Characters made from it keep their own reference.
///
/// # Safety
/// `db` must come from [`anim_database_load`], freed once.
#[no_mangle]
pub unsafe extern "C" fn anim_database_free(db: *mut DatabaseHandle) {
    if !db.is_null() {
        drop(unsafe { Box::from_raw(db) });
    }
}

/// Driven bones in each pose.
///
/// # Safety
/// `db` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn anim_database_bone_count(db: *const DatabaseHandle) -> u32 {
    unsafe { db.as_ref() }.map_or(0, |d| d.db.bone_count() as u32)
}

/// Frames in the database.
///
/// # Safety
/// `db` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn anim_database_frame_count(db: *const DatabaseHandle) -> u32 {
    unsafe { db.as_ref() }.map_or(0, |d| d.db.frame_count() as u32)
}

/// The parent of bone `i` among the driven bones, or -1.
///
/// # Safety
/// `db` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn anim_database_bone_parent(db: *const DatabaseHandle, i: u32) -> i32 {
    unsafe { db.as_ref() }
        .and_then(|d| d.db.skeleton.parents.get(i as usize).copied())
        .flatten()
        .map_or(-1, |p| p as i32)
}

/// Puts bone `i`'s name where [`anim_error_ptr`] reads (0), or fails (1).
///
/// # Safety
/// `db` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn anim_database_bone_name(db: *const DatabaseHandle, i: u32) -> u32 {
    match unsafe { db.as_ref() }.and_then(|d| d.db.skeleton.names.get(i as usize)) {
        Some(n) => {
            fail(n.clone());
            0
        }
        None => {
            fail("no such bone");
            1
        }
    }
}

enum Steer {
    Constant(ConstantSteering),
    /// Boxed: a path holds its points inline.
    Path(Box<PathSteering>),
}

impl Steering for Steer {
    fn desired(&self, p: Vec3) -> (Vec3, f32) {
        match self {
            Self::Constant(c) => c.desired(p),
            Self::Path(s) => s.desired(p),
        }
    }
}

/// One character.
pub struct CharacterHandle {
    matcher: MotionMatcher,
    steer: Steer,
    pose: Vec<f32>,
}

impl CharacterHandle {
    fn write_pose(&mut self) {
        let m = &self.matcher;
        let r = m.root();
        let held = m.feet_held();
        let h = m.hips_position();
        let p = &mut self.pose;
        p[0] = r.translation.x;
        p[1] = r.translation.z;
        p[2] = r.rotation.yaw();
        p[3] = f32::from(u8::from(held[0]) | (u8::from(held[1]) << 1));
        p[4] = h.x;
        p[5] = h.y;
        p[6] = h.z;
        for (i, q) in m.local_rotations().iter().enumerate() {
            let o = 7 + 4 * i;
            p[o] = q.x;
            p[o + 1] = q.y;
            p[o + 2] = q.z;
            p[o + 3] = q.w;
        }
    }
}

/// A character standing at `(x, z)` facing `facing` (radians about +Y, 0 = +Z), with the
/// runtime's default settings; returns a handle or null.
///
/// # Safety
/// `db` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn anim_character_create(
    db: *const DatabaseHandle,
    x: f32,
    z: f32,
    facing: f32,
) -> *mut CharacterHandle {
    let Some(d) = (unsafe { db.as_ref() }) else {
        fail("no database");
        return std::ptr::null_mut();
    };
    if !(x.is_finite() && z.is_finite() && facing.is_finite()) {
        fail("the position and facing must be finite");
        return std::ptr::null_mut();
    }
    let matcher = MotionMatcher::new(
        Arc::clone(&d.db),
        Config::default(),
        Vec3::new(x, 0.0, z),
        facing,
    );
    let mut c = CharacterHandle {
        pose: vec![0.0; 7 + 4 * d.db.bone_count()],
        steer: Steer::Constant(ConstantSteering {
            velocity: Vec3::ZERO,
            facing,
        }),
        matcher,
    };
    c.write_pose();
    Box::into_raw(Box::new(c))
}

/// Frees a character.
///
/// # Safety
/// `c` must come from [`anim_character_create`], freed once.
#[no_mangle]
pub unsafe extern "C" fn anim_character_free(c: *mut CharacterHandle) {
    if !c.is_null() {
        drop(unsafe { Box::from_raw(c) });
    }
}

/// Sets one runtime setting by name (`controller::Config` field names), from `len` UTF-8
/// bytes at `ptr` holding `name=value`. 0 on success.
///
/// # Safety
/// `c` must be live; `ptr` must point at `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn anim_character_set(
    c: *mut CharacterHandle,
    ptr: *const u8,
    len: usize,
) -> u32 {
    let Some(c) = (unsafe { c.as_mut() }) else {
        fail("no character");
        return 1;
    };
    let text = unsafe { std::slice::from_raw_parts(ptr, len) };
    let Some((k, v)) = std::str::from_utf8(text)
        .ok()
        .and_then(|t| t.split_once('='))
    else {
        fail("expected name=value");
        return 1;
    };
    let mut cfg = c.matcher.config();
    match cfg.set(k.trim(), v.trim()) {
        Ok(()) => {
            c.matcher.set_config(cfg);
            0
        }
        Err(e) => {
            fail(e);
            1
        }
    }
}

/// Steers by a desired ground velocity `(vx, vz)` (m/s) and facing. 0 on success.
///
/// # Safety
/// `c` must be live.
#[no_mangle]
pub unsafe extern "C" fn anim_character_set_desired(
    c: *mut CharacterHandle,
    vx: f32,
    vz: f32,
    facing: f32,
) -> u32 {
    let Some(c) = (unsafe { c.as_mut() }) else {
        fail("no character");
        return 1;
    };
    if !(vx.is_finite() && vz.is_finite() && facing.is_finite()) {
        fail("the velocity and facing must be finite");
        return 1;
    }
    c.steer = Steer::Constant(ConstantSteering {
        velocity: Vec3::new(vx, 0.0, vz),
        facing,
    });
    0
}

/// Steers along a path of `count` points (`x, z` pairs of `f32` at `ptr`, at most 64) at
/// `speed` m/s, stopping at its end facing `end_facing` (NaN: the last segment's way).
/// 0 on success.
///
/// # Safety
/// `c` must be live; `ptr` must point at `2 * count` readable `f32`s.
#[no_mangle]
pub unsafe extern "C" fn anim_character_set_path(
    c: *mut CharacterHandle,
    ptr: *const f32,
    count: usize,
    speed: f32,
    end_facing: f32,
) -> u32 {
    let Some(c) = (unsafe { c.as_mut() }) else {
        fail("no character");
        return 1;
    };
    if !(2..=MAX_PATH_POINTS).contains(&count)
        || ptr.is_null()
        || !(speed.is_finite() && speed >= 0.0)
    {
        fail(format!(
            "a path needs 2 to {MAX_PATH_POINTS} points and a speed of 0 or more"
        ));
        return 1;
    }
    let xy = unsafe { std::slice::from_raw_parts(ptr, 2 * count) };
    if xy.iter().any(|v| !v.is_finite()) {
        fail("path points must be finite");
        return 1;
    }
    let mut pts = [Vec3::ZERO; MAX_PATH_POINTS];
    for (i, p) in pts.iter_mut().enumerate().take(count) {
        *p = Vec3::new(xy[2 * i], 0.0, xy[2 * i + 1]);
    }
    let mut s = PathSteering::new(&pts[..count], speed);
    s.end_facing = end_facing.is_finite().then_some(end_facing);
    c.steer = Steer::Path(Box::new(s));
    0
}

/// Places the character at once (a cut). 0 on success.
///
/// # Safety
/// `c` must be live.
#[no_mangle]
pub unsafe extern "C" fn anim_character_teleport(
    c: *mut CharacterHandle,
    x: f32,
    z: f32,
    facing: f32,
) -> u32 {
    let Some(c) = (unsafe { c.as_mut() }) else {
        fail("no character");
        return 1;
    };
    if !(x.is_finite() && z.is_finite() && facing.is_finite()) {
        fail("the position and facing must be finite");
        return 1;
    }
    c.matcher.teleport(Vec3::new(x, 0.0, z), facing);
    c.write_pose();
    0
}

/// Advances by `dt` seconds and rewrites the pose buffer. Allocates nothing. 0 on success.
///
/// # Safety
/// `c` must be live.
#[no_mangle]
pub unsafe extern "C" fn anim_character_step(c: *mut CharacterHandle, dt: f32) -> u32 {
    let Some(c) = (unsafe { c.as_mut() }) else {
        fail("no character");
        return 1;
    };
    if !(dt.is_finite() && dt > 0.0) {
        fail("dt must be positive");
        return 1;
    }
    let CharacterHandle { matcher, steer, .. } = c;
    matcher.step(dt, steer);
    c.write_pose();
    0
}

/// The pose buffer (see the module documentation).
///
/// # Safety
/// `c` must be live.
#[no_mangle]
pub unsafe extern "C" fn anim_character_pose(c: *const CharacterHandle) -> *const f32 {
    unsafe { c.as_ref() }.map_or(std::ptr::null(), |c| c.pose.as_ptr())
}

/// Numbers in the pose buffer.
///
/// # Safety
/// `c` must be live.
#[no_mangle]
pub unsafe extern "C" fn anim_character_pose_len(c: *const CharacterHandle) -> u32 {
    unsafe { c.as_ref() }.map_or(0, |c| c.pose.len() as u32)
}

/// The database frame playing now, and transitions so far (for checks).
///
/// # Safety
/// `c` must be live.
#[no_mangle]
pub unsafe extern "C" fn anim_character_frame(c: *const CharacterHandle) -> u32 {
    unsafe { c.as_ref() }.map_or(0, |c| c.matcher.frame() as u32)
}

/// Transitions so far.
///
/// # Safety
/// `c` must be live.
#[no_mangle]
pub unsafe extern "C" fn anim_character_transitions(c: *const CharacterHandle) -> u32 {
    unsafe { c.as_ref() }.map_or(0, |c| c.matcher.stats().transitions as u32)
}
