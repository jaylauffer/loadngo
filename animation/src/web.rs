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

use crate::character::{Character, Follow, Seat, State};
use crate::controller::Config;
use crate::database::Database;
use crate::math::{Quat, Transform, Vec3};
use crate::path::MAX_PATH_POINTS;

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

/// One character.
pub struct CharacterHandle {
    c: Character,
    pose: Vec<f32>,
    /// Rotations read in by `anim_character_take_over`, kept to be reused.
    scratch: Vec<Quat>,
}

impl CharacterHandle {
    fn write_pose(&mut self) {
        let m = &self.c;
        let r = m.root();
        let held = m.matcher().feet_held();
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

fn live<'a>(c: *mut CharacterHandle) -> Option<&'a mut CharacterHandle> {
    let c = unsafe { c.as_mut() };
    if c.is_none() {
        fail("no character");
    }
    c
}

fn finite(v: &[f32]) -> bool {
    if v.iter().all(|x| x.is_finite()) {
        true
    } else {
        fail("numbers must be finite");
        false
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
    if !finite(&[x, z, facing]) {
        return std::ptr::null_mut();
    }
    let c = Character::new(
        Arc::clone(&d.db),
        Config::default(),
        Vec3::new(x, 0.0, z),
        facing,
    );
    let mut h = CharacterHandle {
        pose: vec![0.0; 7 + 4 * d.db.bone_count()],
        scratch: Vec::with_capacity(d.db.bone_count()),
        c,
    };
    h.write_pose();
    Box::into_raw(Box::new(h))
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
    let Some(c) = live(c) else { return 1 };
    let text = unsafe { std::slice::from_raw_parts(ptr, len) };
    let Some((k, v)) = std::str::from_utf8(text)
        .ok()
        .and_then(|t| t.split_once('='))
    else {
        fail("expected name=value");
        return 1;
    };
    let mut cfg = c.c.matcher().config();
    match cfg.set(k.trim(), v.trim()) {
        Ok(()) => {
            c.c.set_config(cfg);
            0
        }
        Err(e) => {
            fail(e);
            1
        }
    }
}

/// Steers by a ground velocity `(vx, vz)` (m/s) and facing. 0 on success.
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
    let Some(c) = live(c) else { return 1 };
    if !finite(&[vx, vz, facing]) {
        return 1;
    }
    c.c.steer(Vec3::new(vx, 0.0, vz), facing);
    0
}

/// Walks a path of `count` points (`x, z` pairs of `f32` at `ptr`, at most 64) at `speed`
/// m/s, stopping at its end facing `end_facing` (NaN: the last leg's way). 0 on success.
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
    let Some(c) = live(c) else { return 1 };
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
    if !finite(xy) {
        return 1;
    }
    let mut pts = [Vec3::ZERO; MAX_PATH_POINTS];
    for (i, p) in pts.iter_mut().enumerate().take(count) {
        *p = Vec3::new(xy[2 * i], 0.0, xy[2 * i + 1]);
    }
    c.c.go_to(
        &pts[..count],
        speed,
        end_facing.is_finite().then_some(end_facing),
    );
    0
}

/// Turns to face `facing`, standing. 0 on success.
///
/// # Safety
/// `c` must be live.
#[no_mangle]
pub unsafe extern "C" fn anim_character_face(c: *mut CharacterHandle, facing: f32) -> u32 {
    let Some(c) = live(c) else { return 1 };
    if !finite(&[facing]) {
        return 1;
    }
    c.c.face(facing);
    0
}

/// Keeps to `(ox, oz)` (right, ahead) of someone at `(x, z)` moving at `(vx, vz)` facing
/// `heading`; call each frame. 0 on success.
///
/// # Safety
/// `c` must be live.
#[no_mangle]
pub unsafe extern "C" fn anim_character_follow(
    c: *mut CharacterHandle,
    x: f32,
    z: f32,
    vx: f32,
    vz: f32,
    heading: f32,
    ox: f32,
    oz: f32,
) -> u32 {
    let Some(c) = live(c) else { return 1 };
    if !finite(&[x, z, vx, vz, heading, ox, oz]) {
        return 1;
    }
    c.c.follow(Follow {
        position: Vec3::new(x, 0.0, z),
        velocity: Vec3::new(vx, 0.0, vz),
        heading,
        offset: Vec3::new(ox, 0.0, oz),
    });
    0
}

/// Sits on a seat: the seated hips at `(hx, hy, hz)` (world; `hy` their height), facing
/// `facing`. 0 on success.
///
/// # Safety
/// `c` must be live.
#[no_mangle]
pub unsafe extern "C" fn anim_character_sit(
    c: *mut CharacterHandle,
    hx: f32,
    hy: f32,
    hz: f32,
    facing: f32,
) -> u32 {
    let Some(c) = live(c) else { return 1 };
    if !finite(&[hx, hy, hz, facing]) {
        return 1;
    }
    c.c.sit(Seat {
        hips: Vec3::new(hx, hy, hz),
        facing,
    });
    0
}

/// Stands up from a seat. 0 on success.
///
/// # Safety
/// `c` must be live.
#[no_mangle]
pub unsafe extern "C" fn anim_character_stand(c: *mut CharacterHandle) -> u32 {
    let Some(c) = live(c) else { return 1 };
    c.c.stand();
    0
}

/// Beat gestures on `count` word times (seconds from now, `f32` at `ptr`) at `strength`. 0 on success.
///
/// # Safety
/// `c` must be live; `ptr` must point at `count` readable `f32`s.
#[no_mangle]
pub unsafe extern "C" fn anim_character_gesture(
    c: *mut CharacterHandle,
    ptr: *const f32,
    count: usize,
    strength: f32,
) -> u32 {
    let Some(c) = live(c) else { return 1 };
    if ptr.is_null() || count > 256 {
        fail("up to 256 word times");
        return 1;
    }
    let t = unsafe { std::slice::from_raw_parts(ptr, count) };
    if !finite(t) || !finite(&[strength]) {
        return 1;
    }
    c.c.gesture(t, strength);
    0
}

/// Looks at `(x, y, z)` (world), or stops looking with NaN `x`. 0 on success.
///
/// # Safety
/// `c` must be live.
#[no_mangle]
pub unsafe extern "C" fn anim_character_look_at(
    c: *mut CharacterHandle,
    x: f32,
    y: f32,
    z: f32,
) -> u32 {
    let Some(c) = live(c) else { return 1 };
    c.c.look_at(if x.is_nan() {
        None
    } else if finite(&[x, y, z]) {
        Some(Vec3::new(x, y, z))
    } else {
        return 1;
    });
    0
}

/// Reaches hand `side` (0 left, 1 right) to `(x, y, z)` (world, the wrist), or lets it go with
/// NaN `x`. 0 on success.
///
/// # Safety
/// `c` must be live.
#[no_mangle]
pub unsafe extern "C" fn anim_character_hand_to(
    c: *mut CharacterHandle,
    side: u32,
    x: f32,
    y: f32,
    z: f32,
) -> u32 {
    let Some(c) = live(c) else { return 1 };
    c.c.hand_to(
        side as usize,
        if x.is_nan() {
            None
        } else if finite(&[x, y, z]) {
            Some(Vec3::new(x, y, z))
        } else {
            return 1;
        },
    );
    0
}

/// What the character is doing: 0 standing, 1 walking, 2 approaching a seat, 3 sitting down,
/// 4 seated, 5 standing up.
///
/// # Safety
/// `c` must be live.
#[no_mangle]
pub unsafe extern "C" fn anim_character_state(c: *const CharacterHandle) -> u32 {
    unsafe { c.as_ref() }.map_or(0, |c| match c.c.state() {
        State::Standing => 0,
        State::Walking => 1,
        State::Approaching => 2,
        State::SittingDown => 3,
        State::Seated => 4,
        State::StandingUp => 5,
    })
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
    let Some(c) = live(c) else { return 1 };
    if !finite(&[x, z, facing]) {
        return 1;
    }
    c.c.teleport(Vec3::new(x, 0.0, z), facing);
    c.write_pose();
    0
}

/// Takes a body over from other animation without a jump: the root at `(x, z)` facing
/// `facing`, and the pose shown now as `count` numbers at `ptr` laid out as the pose buffer is
/// (its first four ignored; then the hips relative to the root and each driven bone's
/// rotation). 0 on success.
///
/// # Safety
/// `c` must be live; `ptr` must point at `count` readable `f32`s.
#[no_mangle]
pub unsafe extern "C" fn anim_character_take_over(
    c: *mut CharacterHandle,
    x: f32,
    z: f32,
    facing: f32,
    ptr: *const f32,
    count: usize,
) -> u32 {
    let Some(c) = live(c) else { return 1 };
    let n = c.pose.len();
    if ptr.is_null() || count != n || !finite(&[x, z, facing]) {
        fail(format!("a pose of {n} numbers"));
        return 1;
    }
    let p = unsafe { std::slice::from_raw_parts(ptr, count) };
    if !finite(p) {
        return 1;
    }
    let bones = (n - 7) / 4;
    let mut rot = std::mem::take(&mut c.scratch);
    rot.clear();
    for i in 0..bones {
        let o = 7 + 4 * i;
        rot.push(Quat::new(p[o], p[o + 1], p[o + 2], p[o + 3]).normalize());
    }
    c.c.take_over(
        Transform::new(Vec3::new(x, 0.0, z), Quat::from_yaw(facing)),
        Vec3::new(p[4], p[5], p[6]),
        &rot,
    );
    c.scratch = rot;
    c.write_pose();
    0
}

/// Plants the feet where they stand now: each ankle's place on the ground `(lx, lz)`, `(rx, rz)`
/// (world), NaN to leave a foot to the capture; for a body handed over from other animation.
/// 0 on success.
///
/// # Safety
/// `c` must be live.
#[no_mangle]
pub unsafe extern "C" fn anim_character_plant_feet(
    c: *mut CharacterHandle,
    lx: f32,
    lz: f32,
    rx: f32,
    rz: f32,
) -> u32 {
    let Some(c) = live(c) else { return 1 };
    let at = |x: f32, z: f32| (x.is_finite() && z.is_finite()).then(|| Vec3::new(x, 0.0, z));
    c.c.plant_feet([at(lx, lz), at(rx, rz)]);
    0
}

/// Advances by `dt` seconds and rewrites the pose buffer. Allocates nothing. 0 on success.
///
/// # Safety
/// `c` must be live.
#[no_mangle]
pub unsafe extern "C" fn anim_character_step(c: *mut CharacterHandle, dt: f32) -> u32 {
    let Some(c) = live(c) else { return 1 };
    if !(dt.is_finite() && dt > 0.0) {
        fail("dt must be positive");
        return 1;
    }
    c.c.step(dt);
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
    unsafe { c.as_ref() }.map_or(0, |c| c.c.matcher().frame() as u32)
}

/// Transitions so far.
///
/// # Safety
/// `c` must be live.
#[no_mangle]
pub unsafe extern "C" fn anim_character_transitions(c: *const CharacterHandle) -> u32 {
    unsafe { c.as_ref() }.map_or(0, |c| c.c.matcher().stats().transitions as u32)
}
