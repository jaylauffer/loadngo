use std::ffi::c_void;
use std::ptr;

use loadngo_renderer::{FrameCommand, ImageRequest, RendererError};
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
use ui_core::geometry::Color;

pub type EglDisplay = *mut c_void;
pub type EglContext = *mut c_void;
pub type EglSurface = *mut c_void;
type EglConfig = *mut c_void;
type EglBoolean = i32;
type EglInt = i32;

#[repr(C)]
pub struct WlEglWindow {
    _private: [u8; 0],
}

const EGL_DEFAULT_DISPLAY: *mut c_void = ptr::null_mut();
const EGL_NO_DISPLAY: EglDisplay = ptr::null_mut();
const EGL_NO_CONTEXT: EglContext = ptr::null_mut();
const EGL_NO_SURFACE: EglSurface = ptr::null_mut();
const EGL_FALSE: EglBoolean = 0;
const EGL_OPENGL_ES_API: u32 = 0x30A0;
const EGL_SURFACE_TYPE: EglInt = 0x3033;
const EGL_WINDOW_BIT: EglInt = 0x0004;
const EGL_RENDERABLE_TYPE: EglInt = 0x3040;
const EGL_OPENGL_ES2_BIT: EglInt = 0x0004;
const EGL_BLUE_SIZE: EglInt = 0x3022;
const EGL_GREEN_SIZE: EglInt = 0x3023;
const EGL_RED_SIZE: EglInt = 0x3024;
const EGL_ALPHA_SIZE: EglInt = 0x3021;
const EGL_CONTEXT_CLIENT_VERSION: EglInt = 0x3098;
const EGL_NONE: EglInt = 0x3038;

#[derive(Debug, Clone, Copy)]
pub struct LinuxEglWindowHandles {
    pub display_handle: RawDisplayHandle,
    pub window_handle: RawWindowHandle,
}

pub struct LinuxEglBinding {
    pub display: EglDisplay,
    pub context: EglContext,
    pub surface: EglSurface,
    native_window: LinuxNativeWindow,
}

#[derive(Clone, Copy)]
enum LinuxNativeWindow {
    X11,
    Wayland(*mut WlEglWindow),
}

#[link(name = "EGL")]
unsafe extern "C" {
    fn eglGetDisplay(display_id: *mut c_void) -> EglDisplay;
    fn eglInitialize(display: EglDisplay, major: *mut EglInt, minor: *mut EglInt) -> EglBoolean;
    fn eglChooseConfig(
        display: EglDisplay,
        attrib_list: *const EglInt,
        configs: *mut EglConfig,
        config_size: EglInt,
        num_config: *mut EglInt,
    ) -> EglBoolean;
    fn eglBindAPI(api: u32) -> EglBoolean;
    fn eglCreateContext(
        display: EglDisplay,
        config: EglConfig,
        share_context: EglContext,
        attrib_list: *const EglInt,
    ) -> EglContext;
    fn eglCreateWindowSurface(
        display: EglDisplay,
        config: EglConfig,
        win: *mut c_void,
        attrib_list: *const EglInt,
    ) -> EglSurface;
    fn eglMakeCurrent(
        display: EglDisplay,
        draw: EglSurface,
        read: EglSurface,
        context: EglContext,
    ) -> EglBoolean;
    fn eglSwapBuffers(display: EglDisplay, surface: EglSurface) -> EglBoolean;
    fn eglDestroySurface(display: EglDisplay, surface: EglSurface) -> EglBoolean;
    fn eglDestroyContext(display: EglDisplay, context: EglContext) -> EglBoolean;
    fn eglTerminate(display: EglDisplay) -> EglBoolean;
    fn eglGetError() -> EglInt;
}

#[link(name = "wayland-egl")]
unsafe extern "C" {
    fn wl_egl_window_create(surface: *mut c_void, width: i32, height: i32) -> *mut WlEglWindow;
    fn wl_egl_window_destroy(egl_window: *mut WlEglWindow);
    fn wl_egl_window_resize(
        egl_window: *mut WlEglWindow,
        width: i32,
        height: i32,
        dx: i32,
        dy: i32,
    );
}

#[link(name = "GLESv2")]
unsafe extern "C" {
    fn glViewport(x: i32, y: i32, width: i32, height: i32);
    fn glClearColor(red: f32, green: f32, blue: f32, alpha: f32);
    fn glClear(mask: u32);
    fn glEnable(cap: u32);
    fn glBlendFunc(sfactor: u32, dfactor: u32);
    fn glCreateShader(shader_type: u32) -> u32;
    fn glShaderSource(shader: u32, count: i32, string: *const *const i8, length: *const i32);
    fn glCompileShader(shader: u32);
    fn glGetShaderiv(shader: u32, pname: u32, params: *mut i32);
    fn glGetShaderInfoLog(shader: u32, buf_size: i32, length: *mut i32, info_log: *mut i8);
    fn glDeleteShader(shader: u32);
    fn glCreateProgram() -> u32;
    fn glAttachShader(program: u32, shader: u32);
    fn glLinkProgram(program: u32);
    fn glGetProgramiv(program: u32, pname: u32, params: *mut i32);
    fn glGetProgramInfoLog(program: u32, buf_size: i32, length: *mut i32, info_log: *mut i8);
    fn glDeleteProgram(program: u32);
    fn glUseProgram(program: u32);
    fn glGetUniformLocation(program: u32, name: *const i8) -> i32;
    fn glUniform4f(location: i32, v0: f32, v1: f32, v2: f32, v3: f32);
    fn glGenBuffers(n: i32, buffers: *mut u32);
    fn glBindBuffer(target: u32, buffer: u32);
    fn glBufferData(target: u32, size: isize, data: *const c_void, usage: u32);
    fn glDeleteBuffers(n: i32, buffers: *const u32);
    fn glEnableVertexAttribArray(index: u32);
    fn glDisableVertexAttribArray(index: u32);
    fn glVertexAttribPointer(
        index: u32,
        size: i32,
        type_: u32,
        normalized: u8,
        stride: i32,
        pointer: *const c_void,
    );
    fn glDrawArrays(mode: u32, first: i32, count: i32);
    fn glGenTextures(n: i32, textures: *mut u32);
    fn glBindTexture(target: u32, texture: u32);
    fn glDeleteTextures(n: i32, textures: *const u32);
    fn glTexParameteri(target: u32, pname: u32, param: i32);
    fn glTexImage2D(
        target: u32,
        level: i32,
        internalformat: i32,
        width: i32,
        height: i32,
        border: i32,
        format: u32,
        type_: u32,
        pixels: *const c_void,
    );
    fn glActiveTexture(texture: u32);
    fn glUniform1i(location: i32, v0: i32);
}

const GL_COLOR_BUFFER_BIT: u32 = 0x0000_4000;
const GL_BLEND: u32 = 0x0BE2;
const GL_SRC_ALPHA: u32 = 0x0302;
const GL_ONE_MINUS_SRC_ALPHA: u32 = 0x0303;
const GL_VERTEX_SHADER: u32 = 0x8B31;
const GL_FRAGMENT_SHADER: u32 = 0x8B30;
const GL_COMPILE_STATUS: u32 = 0x8B81;
const GL_LINK_STATUS: u32 = 0x8B82;
const GL_INFO_LOG_LENGTH: u32 = 0x8B84;
const GL_ARRAY_BUFFER: u32 = 0x8892;
const GL_STREAM_DRAW: u32 = 0x88E0;
const GL_FLOAT: u32 = 0x1406;
const GL_TRIANGLES: u32 = 0x0004;
const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_TEXTURE0: u32 = 0x84C0;
const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
const GL_TEXTURE_WRAP_S: u32 = 0x2802;
const GL_TEXTURE_WRAP_T: u32 = 0x2803;
const GL_LINEAR: i32 = 0x2601;
const GL_CLAMP_TO_EDGE: i32 = 0x812F;
const GL_RGBA: u32 = 0x1908;
const GL_UNSIGNED_BYTE: u32 = 0x1401;

pub fn bind_window(
    handles: &LinuxEglWindowHandles,
    width: i32,
    height: i32,
) -> Result<LinuxEglBinding, RendererError> {
    let width = width.max(1);
    let height = height.max(1);

    let native_display = match handles.display_handle {
        RawDisplayHandle::Xlib(handle) => handle
            .display
            .map(|display| display.as_ptr())
            .unwrap_or(EGL_DEFAULT_DISPLAY),
        RawDisplayHandle::Xcb(handle) => handle
            .connection
            .map(|connection| connection.as_ptr())
            .unwrap_or(EGL_DEFAULT_DISPLAY),
        RawDisplayHandle::Wayland(handle) => handle.display.as_ptr(),
        _ => {
            return Err(RendererError::Backend(
                "Linux EGL requires an X11/Xcb/Wayland display handle".to_string(),
            ));
        }
    };

    let (native_window, native_window_kind) = match handles.window_handle {
        RawWindowHandle::Xlib(handle) => (
            handle.window as usize as *mut c_void,
            LinuxNativeWindow::X11,
        ),
        RawWindowHandle::Xcb(handle) => (
            handle.window.get() as usize as *mut c_void,
            LinuxNativeWindow::X11,
        ),
        RawWindowHandle::Wayland(handle) => {
            let egl_window = unsafe { wl_egl_window_create(handle.surface.as_ptr(), width, height) };
            if egl_window.is_null() {
                return Err(RendererError::Backend(
                    "wl_egl_window_create returned null".to_string(),
                ));
            }
            (
                egl_window.cast::<c_void>(),
                LinuxNativeWindow::Wayland(egl_window),
            )
        }
        _ => {
            return Err(RendererError::Backend(
                "Linux EGL requires an X11/Xcb/Wayland window handle".to_string(),
            ));
        }
    };

    unsafe {
        let display = eglGetDisplay(native_display);
        if display == EGL_NO_DISPLAY {
            destroy_native_window(native_window_kind);
            return Err(last_egl_error("eglGetDisplay"));
        }

        let mut major = 0;
        let mut minor = 0;
        if eglInitialize(display, &mut major, &mut minor) == EGL_FALSE {
            destroy_native_window(native_window_kind);
            return Err(last_egl_error("eglInitialize"));
        }

        let attribs = [
            EGL_SURFACE_TYPE,
            EGL_WINDOW_BIT,
            EGL_RENDERABLE_TYPE,
            EGL_OPENGL_ES2_BIT,
            EGL_RED_SIZE,
            8,
            EGL_GREEN_SIZE,
            8,
            EGL_BLUE_SIZE,
            8,
            EGL_ALPHA_SIZE,
            8,
            EGL_NONE,
        ];
        let mut config: EglConfig = ptr::null_mut();
        let mut num_config = 0;
        if eglChooseConfig(display, attribs.as_ptr(), &mut config, 1, &mut num_config)
            == EGL_FALSE
            || config.is_null()
            || num_config == 0
        {
            destroy_native_window(native_window_kind);
            let _ = eglTerminate(display);
            return Err(last_egl_error("eglChooseConfig"));
        }

        if eglBindAPI(EGL_OPENGL_ES_API) == EGL_FALSE {
            destroy_native_window(native_window_kind);
            let _ = eglTerminate(display);
            return Err(last_egl_error("eglBindAPI"));
        }

        let context_attribs = [EGL_CONTEXT_CLIENT_VERSION, 3, EGL_NONE];
        let context = eglCreateContext(display, config, EGL_NO_CONTEXT, context_attribs.as_ptr());
        if context == EGL_NO_CONTEXT {
            destroy_native_window(native_window_kind);
            let _ = eglTerminate(display);
            return Err(last_egl_error("eglCreateContext"));
        }

        let surface = eglCreateWindowSurface(display, config, native_window, ptr::null());
        if surface == EGL_NO_SURFACE {
            let _ = eglDestroyContext(display, context);
            destroy_native_window(native_window_kind);
            let _ = eglTerminate(display);
            return Err(last_egl_error("eglCreateWindowSurface"));
        }

        if eglMakeCurrent(display, surface, surface, context) == EGL_FALSE {
            let _ = eglDestroySurface(display, surface);
            let _ = eglDestroyContext(display, context);
            destroy_native_window(native_window_kind);
            let _ = eglTerminate(display);
            return Err(last_egl_error("eglMakeCurrent"));
        }

        Ok(LinuxEglBinding {
            display,
            context,
            surface,
            native_window: native_window_kind,
        })
    }
}

pub fn resize(binding: &mut LinuxEglBinding, width: i32, height: i32) {
    if let LinuxNativeWindow::Wayland(egl_window) = binding.native_window {
        unsafe {
            wl_egl_window_resize(egl_window, width.max(1), height.max(1), 0, 0);
        }
    }
}

pub fn destroy(binding: LinuxEglBinding) {
    unsafe {
        let _ = eglMakeCurrent(
            binding.display,
            EGL_NO_SURFACE,
            EGL_NO_SURFACE,
            EGL_NO_CONTEXT,
        );
        let _ = eglDestroySurface(binding.display, binding.surface);
        let _ = eglDestroyContext(binding.display, binding.context);
        destroy_native_window(binding.native_window);
        let _ = eglTerminate(binding.display);
    }
}

pub fn present_scene(
    binding: &LinuxEglBinding,
    solid_program: &mut u32,
    solid_vbo: &mut u32,
    textured_program: &mut u32,
    textured_vbo: &mut u32,
    image_resources: &std::collections::HashMap<String, super::GlesImageResource>,
    gpu_textures: &mut std::collections::HashMap<String, u32>,
    width: i32,
    height: i32,
    commands: &[FrameCommand],
) -> Result<(), RendererError> {
    unsafe {
        if eglMakeCurrent(
            binding.display,
            binding.surface,
            binding.surface,
            binding.context,
        ) == EGL_FALSE
        {
            return Err(last_egl_error("eglMakeCurrent"));
        }
        glViewport(0, 0, width, height);
        glEnable(GL_BLEND);
        glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);

        if !commands
            .iter()
            .any(|command| matches!(command, FrameCommand::Clear { .. }))
        {
            glClearColor(0.0, 0.0, 0.0, 1.0);
            glClear(GL_COLOR_BUFFER_BIT);
        }

        for command in commands {
            match command {
                FrameCommand::Clear { color } => {
                    glClearColor(
                        color.r as f32 / 255.0,
                        color.g as f32 / 255.0,
                        color.b as f32 / 255.0,
                        color.a as f32 / 255.0,
                    );
                    glClear(GL_COLOR_BUFFER_BIT);
                }
                FrameCommand::FillRect { rect, color } => {
                    ensure_solid_pipeline(solid_program, solid_vbo)?;
                    draw_solid_rects(*solid_program, *solid_vbo, width, height, &[(*rect, *color)])?;
                }
                FrameCommand::StrokeRect {
                    rect,
                    color,
                    thickness,
                } => {
                    ensure_solid_pipeline(solid_program, solid_vbo)?;
                    let rects = super::stroke_rects(*rect, *color, *thickness);
                    draw_solid_rects(*solid_program, *solid_vbo, width, height, &rects)?;
                }
                FrameCommand::Image(request) => {
                    ensure_textured_pipeline(textured_program, textured_vbo)?;
                    draw_images(
                        *textured_program,
                        *textured_vbo,
                        width,
                        height,
                        std::slice::from_ref(request),
                        image_resources,
                        gpu_textures,
                    )?;
                }
                FrameCommand::Line { .. } | FrameCommand::Circle { .. } | FrameCommand::Text(_) => {}
            }
        }

        if eglSwapBuffers(binding.display, binding.surface) == EGL_FALSE {
            return Err(last_egl_error("eglSwapBuffers"));
        }
        Ok(())
    }
}

pub fn destroy_scene_resources(
    solid_program: &mut u32,
    solid_vbo: &mut u32,
    textured_program: &mut u32,
    textured_vbo: &mut u32,
    gpu_textures: &mut std::collections::HashMap<String, u32>,
) {
    unsafe {
        if *solid_vbo != 0 {
            glDeleteBuffers(1, solid_vbo as *const u32);
            *solid_vbo = 0;
        }
        if *solid_program != 0 {
            glDeleteProgram(*solid_program);
            *solid_program = 0;
        }
        if *textured_vbo != 0 {
            glDeleteBuffers(1, textured_vbo as *const u32);
            *textured_vbo = 0;
        }
        if *textured_program != 0 {
            glDeleteProgram(*textured_program);
            *textured_program = 0;
        }
        for texture in gpu_textures.values() {
            glDeleteTextures(1, texture as *const u32);
        }
        gpu_textures.clear();
    }
}

pub fn destroy_texture(texture: &u32) {
    unsafe {
        glDeleteTextures(1, texture as *const u32);
    }
}

fn destroy_native_window(native_window: LinuxNativeWindow) {
    if let LinuxNativeWindow::Wayland(egl_window) = native_window {
        unsafe {
            if !egl_window.is_null() {
                wl_egl_window_destroy(egl_window);
            }
        }
    }
}

fn last_egl_error(label: &str) -> RendererError {
    let code = unsafe { eglGetError() };
    RendererError::Backend(format!("{label} failed with EGL error 0x{code:04x}"))
}

fn ensure_solid_pipeline(solid_program: &mut u32, solid_vbo: &mut u32) -> Result<(), RendererError> {
    unsafe {
        if *solid_program == 0 {
            let vertex_shader = compile_shader(
                GL_VERTEX_SHADER,
                b"#version 300 es\nlayout(location = 0) in vec2 a_pos;\nvoid main() { gl_Position = vec4(a_pos, 0.0, 1.0); }\n\0",
            )?;
            let fragment_shader = compile_shader(
                GL_FRAGMENT_SHADER,
                b"#version 300 es\nprecision mediump float;\nuniform vec4 u_color;\nout vec4 frag_color;\nvoid main() { frag_color = u_color; }\n\0",
            )?;
            let program = glCreateProgram();
            glAttachShader(program, vertex_shader);
            glAttachShader(program, fragment_shader);
            glLinkProgram(program);
            glDeleteShader(vertex_shader);
            glDeleteShader(fragment_shader);

            let mut status = 0;
            glGetProgramiv(program, GL_LINK_STATUS, &mut status);
            if status == 0 {
                let message = program_info_log(program);
                glDeleteProgram(program);
                return Err(RendererError::Backend(format!("glLinkProgram failed: {message}")));
            }
            *solid_program = program;
        }

        if *solid_vbo == 0 {
            glGenBuffers(1, solid_vbo as *mut u32);
            if *solid_vbo == 0 {
                return Err(RendererError::Backend(
                    "glGenBuffers failed for Linux GLES solid rect VBO".to_string(),
                ));
            }
        }
    }
    Ok(())
}

fn ensure_textured_pipeline(
    textured_program: &mut u32,
    textured_vbo: &mut u32,
) -> Result<(), RendererError> {
    unsafe {
        if *textured_program == 0 {
            let vertex_shader = compile_shader(
                GL_VERTEX_SHADER,
                b"#version 300 es\nlayout(location = 0) in vec2 a_pos;\nlayout(location = 1) in vec2 a_uv;\nout vec2 v_uv;\nvoid main() { v_uv = a_uv; gl_Position = vec4(a_pos, 0.0, 1.0); }\n\0",
            )?;
            let fragment_shader = compile_shader(
                GL_FRAGMENT_SHADER,
                b"#version 300 es\nprecision mediump float;\nin vec2 v_uv;\nuniform sampler2D u_tex;\nuniform vec4 u_tint;\nout vec4 frag_color;\nvoid main() { frag_color = texture(u_tex, v_uv) * u_tint; }\n\0",
            )?;
            let program = glCreateProgram();
            glAttachShader(program, vertex_shader);
            glAttachShader(program, fragment_shader);
            glLinkProgram(program);
            glDeleteShader(vertex_shader);
            glDeleteShader(fragment_shader);

            let mut status = 0;
            glGetProgramiv(program, GL_LINK_STATUS, &mut status);
            if status == 0 {
                let message = program_info_log(program);
                glDeleteProgram(program);
                return Err(RendererError::Backend(format!("glLinkProgram failed: {message}")));
            }
            *textured_program = program;
        }

        if *textured_vbo == 0 {
            glGenBuffers(1, textured_vbo as *mut u32);
            if *textured_vbo == 0 {
                return Err(RendererError::Backend(
                    "glGenBuffers failed for Linux GLES textured quad VBO".to_string(),
                ));
            }
        }
    }
    Ok(())
}

fn draw_solid_rects(
    program: u32,
    vbo: u32,
    width: i32,
    height: i32,
    rects: &[(ui_core::geometry::Rect, Color)],
) -> Result<(), RendererError> {
    unsafe {
        glUseProgram(program);
        glBindBuffer(GL_ARRAY_BUFFER, vbo);
        glEnableVertexAttribArray(0);
        glVertexAttribPointer(0, 2, GL_FLOAT, 0, 0, ptr::null());

        let color_location = glGetUniformLocation(program, b"u_color\0".as_ptr().cast());
        if color_location < 0 {
            return Err(RendererError::Backend(
                "u_color uniform not found in Linux GLES solid program".to_string(),
            ));
        }

        for (rect, color) in rects {
            if rect.width <= 0.0 || rect.height <= 0.0 {
                continue;
            }
            let vertices = rect_vertices(*rect, width, height);
            glBufferData(
                GL_ARRAY_BUFFER,
                (vertices.len() * std::mem::size_of::<f32>()) as isize,
                vertices.as_ptr().cast(),
                GL_STREAM_DRAW,
            );
            glUniform4f(
                color_location,
                color.r as f32 / 255.0,
                color.g as f32 / 255.0,
                color.b as f32 / 255.0,
                color.a as f32 / 255.0,
            );
            glDrawArrays(GL_TRIANGLES, 0, 6);
        }

        glDisableVertexAttribArray(0);
        glBindBuffer(GL_ARRAY_BUFFER, 0);
    }
    Ok(())
}

fn draw_images(
    program: u32,
    vbo: u32,
    width: i32,
    height: i32,
    images: &[ImageRequest],
    image_resources: &std::collections::HashMap<String, super::GlesImageResource>,
    gpu_textures: &mut std::collections::HashMap<String, u32>,
) -> Result<(), RendererError> {
    unsafe {
        glUseProgram(program);
        glBindBuffer(GL_ARRAY_BUFFER, vbo);
        glEnableVertexAttribArray(0);
        glEnableVertexAttribArray(1);
        glVertexAttribPointer(
            0,
            2,
            GL_FLOAT,
            0,
            (4 * std::mem::size_of::<f32>()) as i32,
            ptr::null(),
        );
        glVertexAttribPointer(
            1,
            2,
            GL_FLOAT,
            0,
            (4 * std::mem::size_of::<f32>()) as i32,
            (2 * std::mem::size_of::<f32>()) as *const c_void,
        );

        let u_tint = glGetUniformLocation(program, b"u_tint\0".as_ptr().cast());
        let u_tex = glGetUniformLocation(program, b"u_tex\0".as_ptr().cast());
        if u_tint < 0 || u_tex < 0 {
            return Err(RendererError::Backend(
                "Linux GLES textured shader uniforms are unavailable".to_string(),
            ));
        }

        glActiveTexture(GL_TEXTURE0);
        glUniform1i(u_tex, 0);

        for request in images {
            let Some(resource) = image_resources.get(request.image_key.as_str()) else {
                continue;
            };
            let texture = ensure_gpu_texture(request.image_key.as_str(), resource, gpu_textures)?;
            let vertices = textured_rect_vertices(request, width, height);
            glBufferData(
                GL_ARRAY_BUFFER,
                (vertices.len() * std::mem::size_of::<f32>()) as isize,
                vertices.as_ptr().cast(),
                GL_STREAM_DRAW,
            );
            glBindTexture(GL_TEXTURE_2D, texture);
            glUniform4f(u_tint, 1.0, 1.0, 1.0, request.alpha.clamp(0.0, 1.0));
            glDrawArrays(GL_TRIANGLES, 0, 6);
        }

        glBindTexture(GL_TEXTURE_2D, 0);
        glDisableVertexAttribArray(0);
        glDisableVertexAttribArray(1);
        glBindBuffer(GL_ARRAY_BUFFER, 0);
    }
    Ok(())
}

fn ensure_gpu_texture(
    key: &str,
    resource: &super::GlesImageResource,
    gpu_textures: &mut std::collections::HashMap<String, u32>,
) -> Result<u32, RendererError> {
    if let Some(texture) = gpu_textures.get(key) {
        return Ok(*texture);
    }

    unsafe {
        let mut texture = 0;
        glGenTextures(1, &mut texture);
        if texture == 0 {
            return Err(RendererError::Backend(
                "glGenTextures failed for Linux GLES image".to_string(),
            ));
        }
        glBindTexture(GL_TEXTURE_2D, texture);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
        glTexImage2D(
            GL_TEXTURE_2D,
            0,
            GL_RGBA as i32,
            resource.width.max(1),
            resource.height.max(1),
            0,
            GL_RGBA,
            GL_UNSIGNED_BYTE,
            resource.rgba8.as_ptr().cast(),
        );
        gpu_textures.insert(key.to_string(), texture);
        Ok(texture)
    }
}

fn rect_vertices(rect: ui_core::geometry::Rect, width: i32, height: i32) -> [f32; 12] {
    let width = width.max(1) as f32;
    let height = height.max(1) as f32;
    let x0 = rect.x;
    let y0 = rect.y;
    let x1 = rect.x + rect.width;
    let y1 = rect.y + rect.height;
    let to_clip_x = |x: f32| (x / width) * 2.0 - 1.0;
    let to_clip_y = |y: f32| 1.0 - (y / height) * 2.0;
    let left = to_clip_x(x0);
    let right = to_clip_x(x1);
    let top = to_clip_y(y0);
    let bottom = to_clip_y(y1);
    [left, top, right, top, left, bottom, left, bottom, right, top, right, bottom]
}

fn textured_rect_vertices(request: &ImageRequest, width: i32, height: i32) -> [f32; 24] {
    let width = width.max(1) as f32;
    let height = height.max(1) as f32;
    let rect = request.rect;
    let mut draw_x0 = rect.x;
    let mut draw_y0 = rect.y;
    let mut draw_x1 = rect.x + rect.width;
    let mut draw_y1 = rect.y + rect.height;
    if let Some(clip) = request.clip_rect {
        draw_x0 = draw_x0.max(clip.x);
        draw_y0 = draw_y0.max(clip.y);
        draw_x1 = draw_x1.min(clip.x + clip.width);
        draw_y1 = draw_y1.min(clip.y + clip.height);
    }
    let safe_width = rect.width.max(1.0);
    let safe_height = rect.height.max(1.0);
    let u0 = ((draw_x0 - rect.x) / safe_width).clamp(0.0, 1.0);
    let u1 = ((draw_x1 - rect.x) / safe_width).clamp(0.0, 1.0);
    let v0 = ((draw_y0 - rect.y) / safe_height).clamp(0.0, 1.0);
    let v1 = ((draw_y1 - rect.y) / safe_height).clamp(0.0, 1.0);
    let to_clip_x = |x: f32| (x / width) * 2.0 - 1.0;
    let to_clip_y = |y: f32| 1.0 - (y / height) * 2.0;
    let left = to_clip_x(draw_x0);
    let right = to_clip_x(draw_x1);
    let top = to_clip_y(draw_y0);
    let bottom = to_clip_y(draw_y1);
    [
        left, top, u0, v0, right, top, u1, v0, left, bottom, u0, v1, left, bottom, u0, v1,
        right, top, u1, v0, right, bottom, u1, v1,
    ]
}

fn compile_shader(shader_type: u32, source: &[u8]) -> Result<u32, RendererError> {
    unsafe {
        let shader = glCreateShader(shader_type);
        if shader == 0 {
            return Err(RendererError::Backend("glCreateShader failed".to_string()));
        }
        let source_ptr = source.as_ptr().cast::<i8>();
        glShaderSource(shader, 1, &source_ptr, ptr::null());
        glCompileShader(shader);
        let mut status = 0;
        glGetShaderiv(shader, GL_COMPILE_STATUS, &mut status);
        if status == 0 {
            let message = shader_info_log(shader);
            glDeleteShader(shader);
            return Err(RendererError::Backend(format!("glCompileShader failed: {message}")));
        }
        Ok(shader)
    }
}

fn shader_info_log(shader: u32) -> String {
    unsafe {
        let mut length = 0;
        glGetShaderiv(shader, GL_INFO_LOG_LENGTH, &mut length);
        if length <= 1 {
            return "no shader info log".to_string();
        }
        let mut buffer = vec![0u8; length as usize];
        glGetShaderInfoLog(shader, length, ptr::null_mut(), buffer.as_mut_ptr().cast::<i8>());
        String::from_utf8_lossy(&buffer)
            .trim_end_matches(char::from(0))
            .to_string()
    }
}

fn program_info_log(program: u32) -> String {
    unsafe {
        let mut length = 0;
        glGetProgramiv(program, GL_INFO_LOG_LENGTH, &mut length);
        if length <= 1 {
            return "no program info log".to_string();
        }
        let mut buffer = vec![0u8; length as usize];
        glGetProgramInfoLog(program, length, ptr::null_mut(), buffer.as_mut_ptr().cast::<i8>());
        String::from_utf8_lossy(&buffer)
            .trim_end_matches(char::from(0))
            .to_string()
    }
}
