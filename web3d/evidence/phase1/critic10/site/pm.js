// Tenth critic: what WEBGL_polygon_mode does in this Chrome, for the claims in docs/WEB3D.md and
// js/gl.js: "Chrome prints a console warning whenever that extension is enabled, and whether the
// page enabled it cannot be asked without enabling it". Console messages are recorded by the driver;
// each step prints a MARK line first so they can be told apart. No web3d here.
const out = {};
const mark = (s) => console.info(`MARK ${s}`);
const POLYGON_MODE_WEBGL = 0x0b40;
const ctx = () => document.createElement('canvas').getContext('webgl2', { antialias: false });
const lose = (gl) => gl.getExtension('WEBGL_lose_context')?.loseContext();

// 1. Asking without enabling: getParameter(POLYGON_MODE_WEBGL) on a context that never enabled it.
{
  const gl = ctx();
  mark('ask-without-enabling');
  const v = gl.getParameter(POLYGON_MODE_WEBGL);
  out.askNotEnabled = { value: v, error: gl.getError() };
  lose(gl);
}
// 2. Enabling once, then getExtension again on the same context.
{
  const gl = ctx();
  mark('enable-first');
  const a = gl.getExtension('WEBGL_polygon_mode');
  mark('enable-second');
  const b = gl.getExtension('WEBGL_polygon_mode');
  out.enable = { supported: Boolean(a), sameObject: a === b };
  lose(gl);
}
// 3. The page enabled it and set LINE: asking without calling getExtension again.
{
  const gl = ctx();
  const x = gl.getExtension('WEBGL_polygon_mode');
  if (x) x.polygonModeWEBGL(gl.FRONT_AND_BACK, x.LINE_WEBGL);
  mark('ask-after-page-enabled');
  out.askEnabledLine = { value: gl.getParameter(POLYGON_MODE_WEBGL), error: gl.getError(), LINE_WEBGL: x?.LINE_WEBGL, FILL_WEBGL: x?.FILL_WEBGL };
  // 4. A test draw tells line mode apart without the extension object or any GL error: a triangle
  // far larger than a 1x1 framebuffer covers its one pixel when filled, and no edge crosses it.
  mark('draw-test');
  const detect = () => {
    const fb = gl.createFramebuffer();
    const rb = gl.createRenderbuffer();
    gl.bindRenderbuffer(gl.RENDERBUFFER, rb);
    gl.renderbufferStorage(gl.RENDERBUFFER, gl.RGBA8, 1, 1);
    gl.bindFramebuffer(gl.FRAMEBUFFER, fb);
    gl.framebufferRenderbuffer(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.RENDERBUFFER, rb);
    const sh = (t, s) => {
      const o = gl.createShader(t);
      gl.shaderSource(o, s);
      gl.compileShader(o);
      return o;
    };
    const p = gl.createProgram();
    gl.attachShader(p, sh(gl.VERTEX_SHADER, '#version 300 es\nvoid main(){vec2 v[3]=vec2[3](vec2(-9.0,-9.0),vec2(9.0,-9.0),vec2(0.0,9.0));gl_Position=vec4(v[gl_VertexID],0.0,1.0);}'));
    gl.attachShader(p, sh(gl.FRAGMENT_SHADER, '#version 300 es\nprecision mediump float;out vec4 o;void main(){o=vec4(1.0);}'));
    gl.linkProgram(p);
    gl.useProgram(p);
    gl.viewport(0, 0, 1, 1);
    gl.clearColor(0, 0, 0, 0);
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
    const px = new Uint8Array(4);
    gl.readPixels(0, 0, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, px);
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    return { filled: px[0] === 255, error: gl.getError() };
  };
  out.drawTestLine = detect();
  if (x) x.polygonModeWEBGL(gl.FRONT_AND_BACK, x.FILL_WEBGL);
  out.drawTestFill = detect();
  lose(gl);
}
// 5. Same draw test on a context that never enabled the extension: filled, no error, no warning.
{
  const gl = ctx();
  mark('draw-test-never-enabled');
  const fb = gl.createFramebuffer();
  const rb = gl.createRenderbuffer();
  gl.bindRenderbuffer(gl.RENDERBUFFER, rb);
  gl.renderbufferStorage(gl.RENDERBUFFER, gl.RGBA8, 1, 1);
  gl.bindFramebuffer(gl.FRAMEBUFFER, fb);
  gl.framebufferRenderbuffer(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.RENDERBUFFER, rb);
  out.neverEnabledError = gl.getError();
  lose(gl);
}
mark('end');
window.__probe10 = { done: Promise.resolve(out) };
