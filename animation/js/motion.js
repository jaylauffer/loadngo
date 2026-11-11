// motion.js: loads loadngo-animation's WebAssembly module and wraps it (docs/ANIMATION.md).
//
//   import { loadMotion } from './motion.js';
//   const motion = await loadMotion(new URL('./loadngo_animation.wasm', import.meta.url));
//   const db = motion.loadDatabase(await (await fetch('walk.lmm')).arrayBuffer());
//   const who = db.createCharacter(0, 0, 0);            // x, z, facing (radians about +Y, 0 = +Z)
//   who.setPath([0, 0, 0, 6, 6, 6], 0.83);              // x, z pairs; m/s
//   // each frame the host asks for:
//   const pose = who.step(dt);                          // a Float32Array view, rewritten in place
//   who.sit(hx, hy, hz, facing); who.stand();           // seats: the seated hips, the way they face
//   who.lookAt([x, y, z]); who.gesture([0.2, 0.7], 0.6); // layers over the matched motion
//
// The module has no imports and allocates nothing in step(). Its memory grows only when a
// database or character is made; the views here are remade then.
//
// pose: [rootX, rootZ, facing, feetHeld, hipsX, hipsY, hipsZ, then x, y, z, w per bone]: the
// hips' rotation relative to the root, every other bone's relative to its parent (db.boneNames,
// db.parents). applyPose() below writes it into three.js-style objects.

const decoder = new TextDecoder();
const STATES = ['standing', 'walking', 'approaching', 'sitting-down', 'seated', 'standing-up'];
const encoder = new TextEncoder();

export async function loadMotion(source) {
  let module;
  if (source instanceof WebAssembly.Module) module = source;
  else if (source instanceof ArrayBuffer || ArrayBuffer.isView(source)) module = await WebAssembly.compile(source);
  else module = await WebAssembly.compileStreaming(fetch(source));
  const instance = await WebAssembly.instantiate(module, {});
  return new Motion(instance.exports);
}

export class Motion {
  constructor(exports) {
    this.x = exports;
  }
  /** The text the module last left (an error or a bone name). */
  text() {
    const x = this.x;
    return decoder.decode(new Uint8Array(x.memory.buffer, x.anim_error_ptr(), x.anim_error_len()));
  }
  stage(bytes) {
    const u8 = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes.buffer ?? bytes, bytes.byteOffset ?? 0, bytes.byteLength);
    const p = this.x.anim_alloc(u8.byteLength);
    new Uint8Array(this.x.memory.buffer, p, u8.byteLength).set(u8);
    return [p, u8.byteLength];
  }
  /** A database from an anim_build file (its hash and version are checked). */
  loadDatabase(bytes) {
    const [p, n] = this.stage(bytes);
    const h = this.x.anim_database_load(p, n);
    this.x.anim_free(p, n);
    if (!h) throw new Error(`motion: ${this.text()}`);
    return new MotionDatabase(this, h);
  }
}

export class MotionDatabase {
  constructor(motion, handle) {
    this.motion = motion;
    this.h = handle;
    const x = motion.x;
    const n = x.anim_database_bone_count(handle);
    this.boneNames = [];
    this.parents = new Int32Array(n);
    for (let i = 0; i < n; i++) {
      x.anim_database_bone_name(handle, i);
      this.boneNames.push(motion.text());
      this.parents[i] = x.anim_database_bone_parent(handle, i);
    }
    this.frameCount = x.anim_database_frame_count(handle);
  }
  /** The name of the clip holding database frame `frame` (`-m`: its mirror image), for checks. */
  clipName(frame) {
    return this.motion.x.anim_database_clip_name(this.h, frame) === 0 ? this.motion.text() : null;
  }
  createCharacter(x = 0, z = 0, facing = 0) {
    const h = this.motion.x.anim_character_create(this.h, x, z, facing);
    if (!h) throw new Error(`motion: ${this.motion.text()}`);
    return new MotionCharacter(this, h);
  }
  dispose() {
    if (this.h) this.motion.x.anim_database_free(this.h);
    this.h = 0;
  }
}

export class MotionCharacter {
  constructor(db, handle) {
    this.db = db;
    this.motion = db.motion;
    this.h = handle;
    this.buffer = null;
    this.pose = null;
    this.view();
  }
  view() {
    const x = this.motion.x;
    if (this.buffer !== x.memory.buffer) {
      this.buffer = x.memory.buffer;
      this.pose = new Float32Array(this.buffer, x.anim_character_pose(this.h), x.anim_character_pose_len(this.h));
    }
    return this.pose;
  }
  check(r) {
    if (r !== 0) throw new Error(`motion: ${this.motion.text()}`);
  }
  /** Steer by a ground velocity (m/s) and facing (radians). */
  setDesired(vx, vz, facing) {
    this.check(this.motion.x.anim_character_set_desired(this.h, vx, vz, facing));
  }
  /** Steer along x, z pairs at speed (m/s), stopping at the end facing endFacing (NaN: the last segment's way). */
  setPath(points, speed, endFacing = NaN) {
    const f = Float32Array.from(points);
    const [p, n] = this.motion.stage(new Uint8Array(f.buffer));
    const r = this.motion.x.anim_character_set_path(this.h, p, f.length / 2, speed, endFacing);
    this.motion.x.anim_free(p, n);
    this.view();
    this.check(r);
  }
  /** One runtime setting by name (controller::Config), e.g. set('transition_cost', 0.5). */
  set(name, value) {
    const [p, n] = this.motion.stage(encoder.encode(`${name}=${value}`));
    const r = this.motion.x.anim_character_set(this.h, p, n);
    this.motion.x.anim_free(p, n);
    this.view();
    this.check(r);
  }
  /** Turn to face `facing` (radians), standing. */
  face(facing) {
    this.check(this.motion.x.anim_character_face(this.h, facing));
  }
  /** Keep to (ox right, oz ahead) of someone at x, z moving at vx, vz facing heading; call each frame. */
  follow(x, z, vx, vz, heading, ox, oz) {
    this.check(this.motion.x.anim_character_follow(this.h, x, z, vx, vz, heading, ox, oz));
  }
  /** Sit on a seat: the seated hips at (hx, hy, hz) in world space, facing `facing`. */
  sit(hx, hy, hz, facing) {
    this.check(this.motion.x.anim_character_sit(this.h, hx, hy, hz, facing));
  }
  /** Stand up from a seat (a later command waits until standing). */
  stand() {
    this.check(this.motion.x.anim_character_stand(this.h));
  }
  /** Beat gestures on word times (seconds from now) at a strength (0..1). */
  gesture(wordTimes, strength = 1) {
    const f = Float32Array.from(wordTimes);
    const [p, n] = this.motion.stage(new Uint8Array(f.buffer));
    const r = this.motion.x.anim_character_gesture(this.h, p, f.length, strength);
    this.motion.x.anim_free(p, n);
    this.view();
    this.check(r);
  }
  /** Look at a world point, or stop looking with null. */
  lookAt(point) {
    const [x, y, z] = point ?? [NaN, 0, 0];
    this.check(this.motion.x.anim_character_look_at(this.h, x, y, z));
  }
  /** Reach hand `side` ('left' or 'right') to a world point (the wrist), or let go with null. */
  handTo(side, point) {
    const [x, y, z] = point ?? [NaN, 0, 0];
    this.check(this.motion.x.anim_character_hand_to(this.h, side === 'left' ? 0 : 1, x, y, z));
  }
  /** 'standing', 'walking', 'approaching', 'sitting-down', 'seated' or 'standing-up'. */
  get state() {
    return STATES[this.motion.x.anim_character_state(this.h)];
  }
  /** Plant the feet where they stand now: each ankle's [x, z] on the ground, or null to leave it to the capture. */
  plantFeet(left, right) {
    const [lx, lz] = left ?? [NaN, NaN];
    const [rx, rz] = right ?? [NaN, NaN];
    this.check(this.motion.x.anim_character_plant_feet(this.h, lx, lz, rx, rz));
  }
  /**
   * Take a body over from other animation without a jump: the root at x, z facing `facing`, and
   * `pose` (a Float32Array laid out as step() returns: its first four numbers ignored, then the
   * hips relative to the root and each bone's rotation) as the body is shown now.
   */
  takeOver(x, z, facing, pose) {
    const [p, n] = this.motion.stage(new Uint8Array(pose.buffer, pose.byteOffset, pose.byteLength));
    const r = this.motion.x.anim_character_take_over(this.h, x, z, facing, p, pose.length);
    this.motion.x.anim_free(p, n);
    this.view();
    this.check(r);
  }
  teleport(x, z, facing) {
    this.check(this.motion.x.anim_character_teleport(this.h, x, z, facing));
  }
  /** Advance by dt seconds; returns the pose view (no allocation). */
  step(dt) {
    this.check(this.motion.x.anim_character_step(this.h, dt));
    return this.view();
  }
  get frame() {
    return this.motion.x.anim_character_frame(this.h);
  }
  get transitions() {
    return this.motion.x.anim_character_transitions(this.h);
  }
  dispose() {
    if (this.h) this.motion.x.anim_character_free(this.h);
    this.h = 0;
  }
}

/**
 * Writes a pose into three.js-style objects without importing three: `root` (an Object3D
 * placed on the ground) takes x, z and the facing; `bones[i]` (Bones in db.boneNames order)
 * take their rotations, and bones[0] (the hips) its position relative to the root. The hips'
 * parent must sit at the root (an armature node with no transform of its own).
 */
export function applyPose(pose, root, bones) {
  root.position.x = pose[0];
  root.position.z = pose[1];
  root.rotation.y = pose[2];
  bones[0].position.set(pose[4], pose[5], pose[6]);
  for (let i = 0; i < bones.length; i++) {
    const o = 7 + 4 * i;
    bones[i].quaternion.set(pose[o], pose[o + 1], pose[o + 2], pose[o + 3]);
  }
}
