// WebGL2 for loadngo-web3d. The wasm module imports these functions as module `web3d_gl`.
//
// This file owns the WebGL2 context and the tables that turn small integers into WebGL objects
// (programs, buffers, vertex arrays, textures, queries; 0 is "none"). Every function takes and
// returns numbers.
//
// Once the context is lost, this shim is finished for good, even after the browser restores the
// context: its objects belonged to the lost one. Creating returns 0 (WebGL itself hands out objects
// while lost), and deleting only forgets the handle, so disposing the old renderer after a restore
// sends nothing into the restored context, which a new renderer then owns. The shim listens for no
// events: web3d.js keeps one loss controller a canvas, which says when this shim's context was lost.
//
// Byte data arrives as a pointer and a length into the wasm module's memory and is read in place
// through one cached Uint8Array view (WebGL2's `bufferSubData(target, offset, view, srcOffset,
// length)`), so drawing a frame allocates nothing here. Strings (shader sources, block names, and
// the messages of errors and logs) are decoded only while the programs are built or when a message
// is reported.
//
// BSD-3-Clause; see the repository's LICENSE.

const INVALID_INDEX = 0xffffffff;

/** Small integers for WebGL objects; slot 0 is always null. */
class Table {
  constructor() {
    this.items = [null];
    this.free = [];
  }
  put(object) {
    if (!object) return 0;
    if (this.free.length > 0) {
      const i = this.free.pop();
      this.items[i] = object;
      return i;
    }
    this.items.push(object);
    return this.items.length - 1;
  }
  take(i) {
    const object = this.items[i] ?? null;
    if (i > 0 && object) {
      this.items[i] = null;
      this.free.push(i);
    }
    return object;
  }
}

/**
 * The import object for the wasm module, over `gl`, a WebGL2 context web3d.js took from a canvas.
 * `lostSince()` says whether the context has been lost since this shim was made (web3d.js's loss
 * controller counts the canvas's losses), so a loss and restore that both fell between two checks
 * still finish the shim.
 */
export function createGl(gl, lostSince) {
  const timer = gl.getExtension('EXT_disjoint_timer_query_webgl2');
  // Lets the driver compile the programs in parallel; the link check then waits for them.
  gl.getExtension('KHR_parallel_shader_compile');

  const programs = new Table();
  const buffers = new Table();
  const arrays = new Table();
  const textures = new Table();
  const queries = new Table();
  const decoder = new TextDecoder();
  const depthOnly = [gl.DEPTH];
  const back = [gl.BACK];

  const state = { staged: null, stagedSize: null, lost: false, lastLog: '' };
  /** True from the moment the context is lost, and for good: the event can lag isContextLost(). */
  const gone = () => {
    if (!state.lost && (gl.isContextLost() || lostSince())) state.lost = true;
    return state.lost;
  };
  let memory = null;
  let heap = new Uint8Array(0);

  /** The module's memory as bytes; the view is remade only after the memory grows. */
  const bytes = () => {
    if (heap.buffer !== memory.buffer) heap = new Uint8Array(memory.buffer);
    return heap;
  };
  // Pointers and lengths arrive as wasm i32s; `>>> 0` reads them unsigned, as Rust meant them.
  const text = (ptr, len) => decoder.decode(bytes().subarray(ptr >>> 0, (ptr >>> 0) + (len >>> 0)));
  const note = (level, message) => {
    state.lastLog = message;
    if (level >= 2) console.error(message);
    else if (level === 1) console.warn(message);
    else console.info(message);
  };

  const compile = (type, source) => {
    const shader = gl.createShader(type);
    gl.shaderSource(shader, source);
    gl.compileShader(shader);
    return shader;
  };

  const imports = {
    get_integer: (pname) => gl.getParameter(pname) | 0,
    timer_query_supported: () => (timer ? 1 : 0),

    program_create(vsPtr, vsLen, fsPtr, fsLen) {
      if (gone()) return 0;
      const program = gl.createProgram();
      if (!program) return 0;
      const vs = compile(gl.VERTEX_SHADER, text(vsPtr, vsLen));
      const fs = compile(gl.FRAGMENT_SHADER, text(fsPtr, fsLen));
      gl.attachShader(program, vs);
      gl.attachShader(program, fs);
      gl.linkProgram(program);
      // The shaders are kept attached until the check, for their logs.
      program.web3dShaders = [vs, fs];
      return programs.put(program);
    },
    program_check(p) {
      const program = programs.items[p];
      if (!program) return 0;
      const ok = gl.getProgramParameter(program, gl.LINK_STATUS);
      if (!ok && gone()) {
        note(1, 'web3d: the WebGL context was lost while the shader programs were compiled');
      } else if (!ok) {
        const logs = program.web3dShaders.map((s) => gl.getShaderInfoLog(s)).filter(Boolean);
        logs.push(gl.getProgramInfoLog(program));
        note(2, `web3d: shader program ${p} failed:\n${logs.join('\n')}`);
      }
      for (const s of program.web3dShaders) {
        gl.detachShader(program, s);
        gl.deleteShader(s);
      }
      program.web3dShaders = [];
      return ok ? 1 : 0;
    },
    program_bind_block(p, namePtr, nameLen, binding) {
      const program = programs.items[p];
      const index = gl.getUniformBlockIndex(program, text(namePtr, nameLen));
      if (index === INVALID_INDEX) return 0;
      gl.uniformBlockBinding(program, index, binding);
      return 1;
    },
    program_bind_sampler(p, namePtr, nameLen, unit) {
      const program = programs.items[p];
      const location = gl.getUniformLocation(program, text(namePtr, nameLen));
      if (!location) return 0;
      gl.useProgram(program);
      gl.uniform1i(location, unit);
      return 1;
    },
    program_block_size(p, namePtr, nameLen) {
      const program = programs.items[p];
      const index = gl.getUniformBlockIndex(program, text(namePtr, nameLen));
      if (index === INVALID_INDEX) return -1;
      return gl.getActiveUniformBlockParameter(program, index, gl.UNIFORM_BLOCK_DATA_SIZE) | 0;
    },
    program_uniform_offset(p, namePtr, nameLen) {
      const program = programs.items[p];
      const indices = gl.getUniformIndices(program, [text(namePtr, nameLen)]);
      if (!indices || indices[0] === INVALID_INDEX) return -1;
      return gl.getActiveUniforms(program, indices, gl.UNIFORM_OFFSET)[0] | 0;
    },
    program_delete: (p) => {
      const program = programs.take(p);
      if (!gone()) gl.deleteProgram(program);
    },
    use_program: (p) => gl.useProgram(programs.items[p]),

    buffer_create(target, ptr, len, usage) {
      if (gone()) return 0;
      const buffer = gl.createBuffer();
      if (!buffer) return 0;
      gl.bindBuffer(target, buffer);
      // A length of 0 would mean "to the end of the view", the whole heap.
      if (len !== 0) gl.bufferData(target, bytes(), usage, ptr >>> 0, len >>> 0);
      return buffers.put(buffer);
    },
    buffer_alloc(target, size, usage) {
      if (gone()) return 0;
      const buffer = gl.createBuffer();
      if (!buffer) return 0;
      gl.bindBuffer(target, buffer);
      gl.bufferData(target, size, usage);
      return buffers.put(buffer);
    },
    buffer_update(b, target, offset, ptr, len) {
      if (len === 0) return;
      gl.bindBuffer(target, buffers.items[b]);
      gl.bufferSubData(target, offset >>> 0, bytes(), ptr >>> 0, len >>> 0);
    },
    buffer_delete: (b) => {
      const buffer = buffers.take(b);
      if (!gone()) gl.deleteBuffer(buffer);
    },
    bind_buffer_range: (target, index, b, offset, size) =>
      gl.bindBufferRange(target, index, buffers.items[b], offset, size),

    vertex_array_create: () => (gone() ? 0 : arrays.put(gl.createVertexArray())),
    vertex_array_bind: (a) => gl.bindVertexArray(arrays.items[a]),
    vertex_array_delete: (a) => {
      const array = arrays.take(a);
      if (!gone()) gl.deleteVertexArray(array);
    },
    vertex_attrib(location, b, size, type, normalized, stride, offset) {
      gl.bindBuffer(gl.ARRAY_BUFFER, buffers.items[b]);
      gl.enableVertexAttribArray(location);
      gl.vertexAttribPointer(location, size, type, normalized !== 0, stride, offset);
    },
    index_buffer_bind: (b) => gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, buffers.items[b]),

    texture_create_staged(internalFormat, width, height, levels, wrapS, wrapT, minFilter, magFilter) {
      const source = state.staged;
      if (!source) {
        note(2, 'web3d: no image source was staged for the texture');
        return 0;
      }
      if (gone()) return 0;
      // The size the texture is made for must still be the source's, or the upload would fail
      // (larger) or fill only a corner (smaller).
      let w = width;
      let h = height;
      try {
        if (state.stagedSize) [w, h] = state.stagedSize();
      } catch {
        w = -1; // no longer an image source; never throw through the wasm module
      }
      if (w !== width || h !== height) {
        note(2, `web3d: the image source is ${w}x${h} now, not the ${width}x${height} it was when the texture was asked for`);
        return 0;
      }
      // Errors left pending by earlier calls (the page's, on a context it made, or a draw's) are not
      // this upload's: report each as pending and clear it, so the check after the upload is exact.
      for (let i = 0; i < 16; i++) {
        const pending = gl.getError();
        if (pending === gl.NO_ERROR) break;
        note(1, `web3d: WebGL error 0x${pending.toString(16)} was pending before a texture upload; an earlier call raised it, not the upload`);
      }
      const texture = gl.createTexture();
      if (!texture) return 0;
      try {
        gl.activeTexture(gl.TEXTURE0);
        gl.bindTexture(gl.TEXTURE_2D, texture);
        gl.texStorage2D(gl.TEXTURE_2D, levels, internalFormat, width, height);
        // glTF's convention: the image's first row is v = 0. No premultiplying, no colour
        // management (ImageBitmaps take these from createImageBitmap's options instead).
        gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, false);
        gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, false);
        gl.pixelStorei(gl.UNPACK_COLORSPACE_CONVERSION_WEBGL, gl.NONE);
        gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, gl.RGBA, gl.UNSIGNED_BYTE, source);
        // Once a texture, not per frame: an upload WebGL refused leaves an error, not an exception.
        // Pending errors were cleared above, so this one is the upload's.
        const error = gl.getError();
        if (error !== gl.NO_ERROR) {
          note(2, `web3d: the texture upload raised WebGL error 0x${error.toString(16)}: WebGL refused to put the source's pixels into the ${width}x${height} texture made for it`);
          gl.deleteTexture(texture);
          return 0;
        }
        if (levels > 1) gl.generateMipmap(gl.TEXTURE_2D);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, wrapS);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, wrapT);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, minFilter);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, magFilter);
      } catch (error) {
        // A cross-origin image or a closed ImageBitmap throws; report it rather than unwind
        // through the wasm module.
        note(2, `web3d: texture upload failed: ${error}`);
        gl.deleteTexture(texture);
        return 0;
      }
      return textures.put(texture);
    },
    texture_bind(unit, t) {
      gl.activeTexture(gl.TEXTURE0 + unit);
      gl.bindTexture(gl.TEXTURE_2D, textures.items[t]);
    },
    texture_delete: (t) => {
      const texture = textures.take(t);
      if (!gone()) gl.deleteTexture(texture);
    },

    enable: (cap) => gl.enable(cap),
    disable: (cap) => gl.disable(cap),
    front_face: (mode) => gl.frontFace(mode),
    depth_mask: (write) => gl.depthMask(write !== 0),
    blend_func_separate: (a, b, c, d) => gl.blendFuncSeparate(a, b, c, d),
    // WebGL 2's default state, put back when a renderer is initialised (Renderer::init in
    // src/renderer.rs says which, and why), so state a page left on its context reaches nothing.
    color_mask: (r, g, b, a) => gl.colorMask(r !== 0, g !== 0, b !== 0, a !== 0),
    depth_func: (func) => gl.depthFunc(func),
    depth_range: (near, far) => gl.depthRange(near, far),
    clear_depth: (depth) => gl.clearDepth(depth),
    cull_face: (mode) => gl.cullFace(mode),
    blend_equation: (mode) => gl.blendEquation(mode),
    hint: (target, mode) => gl.hint(target, mode),
    pixel_store: (pname, value) => gl.pixelStorei(pname, value),
    use_default_framebuffer() {
      gl.bindFramebuffer(gl.FRAMEBUFFER, null);
      gl.drawBuffers(back);
      gl.readBuffer(gl.BACK);
    },
    unbind_buffer: (target) => gl.bindBuffer(target, null),
    unbind_sampler: (unit) => gl.bindSampler(unit, null),
    vertex_attrib_default: (location, x, y, z, w) => gl.vertexAttrib4f(location, x, y, z, w),
    end_transform_feedback() {
      // Ending it when none is active would be an error; binding while one is active, too.
      if (gl.getParameter(gl.TRANSFORM_FEEDBACK_ACTIVE)) gl.endTransformFeedback();
      gl.bindTransformFeedback(gl.TRANSFORM_FEEDBACK, null);
    },
    end_query_on(target) {
      if (gl.getQuery(target, gl.CURRENT_QUERY)) gl.endQuery(target);
    },
    // State that WebGL 2 extensions add, for each the browser has: getExtension returns the page's
    // object if it enabled one, and enabling one here changes no state. Not WEBGL_polygon_mode:
    // Chrome prints a console warning whenever it is enabled, and whether the page enabled it
    // cannot be asked without enabling it (docs/WEB3D.md says so).
    reset_extension_state() {
      const clip = gl.getExtension('EXT_clip_control');
      if (clip) clip.clipControlEXT(clip.LOWER_LEFT_EXT, clip.NEGATIVE_ONE_TO_ONE_EXT);
      const clamp = gl.getExtension('EXT_depth_clamp');
      if (clamp) gl.disable(clamp.DEPTH_CLAMP_EXT);
      const distances = gl.getExtension('WEBGL_clip_cull_distance');
      if (distances) {
        const n = Math.min(gl.getParameter(distances.MAX_CLIP_DISTANCES_WEBGL) | 0, 32);
        for (let i = 0; i < n; i++) gl.disable(distances.CLIP_DISTANCE0_WEBGL + i);
      }
    },
    viewport: (x, y, width, height) => gl.viewport(x, y, width, height),
    clear_color: (r, g, b, a) => gl.clearColor(r, g, b, a),
    clear: (mask) => gl.clear(mask),
    draw_elements: (mode, count, type, offset) => gl.drawElements(mode, count, type, offset),
    invalidate_depth: () => gl.invalidateFramebuffer(gl.FRAMEBUFFER, depthOnly),
    flush: () => gl.flush(),

    query_create: () => (timer && !gone() ? queries.put(gl.createQuery()) : 0),
    query_delete: (q) => {
      const query = queries.take(q);
      if (!gone()) gl.deleteQuery(query);
    },
    query_begin: (q) => gl.beginQuery(timer.TIME_ELAPSED_EXT, queries.items[q]),
    query_end: () => gl.endQuery(timer.TIME_ELAPSED_EXT),
    query_available: (q) => (gl.getQueryParameter(queries.items[q], gl.QUERY_RESULT_AVAILABLE) ? 1 : 0),
    // Nanoseconds, an integer: no boxed fraction on the way to wasm.
    query_result_ns: (q) => gl.getQueryParameter(queries.items[q], gl.QUERY_RESULT),
    gpu_disjoint: () => (gl.getParameter(timer.GPU_DISJOINT_EXT) ? 1 : 0),

    log: (level, ptr, len) => note(level, `web3d: ${text(ptr, len)}`),
    // performance.now, bound to performance. Each reading still comes back as a number V8 boxes
    // (20.8 sampled bytes a frame for the two, evidence/phase1/cpu-timing-allocation/on.txt): that
    // is why CPU timing is off unless asked for.
    now: performance.now.bind(performance),
  };

  return {
    gl,
    state,
    imports: { web3d_gl: imports },
    /** Gives the shim the instantiated module's memory. */
    attach(wasmMemory) {
      memory = wasmMemory;
      heap = new Uint8Array(memory.buffer);
    },
    /** Whether the context has been lost (for good, for this shim). */
    lost: gone,
    /** Holds an image source, and a way to read its size again, for the next texture_create_staged call. */
    stage(source, sizeOf = null) {
      state.staged = source;
      state.stagedSize = sizeOf;
    },
    info() {
      const debug = gl.getExtension('WEBGL_debug_renderer_info');
      return {
        renderer: gl.getParameter(debug ? debug.UNMASKED_RENDERER_WEBGL : gl.RENDERER),
        vendor: gl.getParameter(debug ? debug.UNMASKED_VENDOR_WEBGL : gl.VENDOR),
        version: gl.getParameter(gl.VERSION),
        maxTextureSize: gl.getParameter(gl.MAX_TEXTURE_SIZE),
        timerQuery: Boolean(timer),
        antialias: Boolean(gl.getContextAttributes()?.antialias),
        samples: gl.getParameter(gl.SAMPLES),
      };
    },
    /** Nothing to release: the shim holds no listeners, and its GL objects go with the renderer. */
    dispose() {},
  };
}
