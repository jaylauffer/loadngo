#[cfg(any(target_os = "android", target_os = "linux"))]
use std::collections::HashMap;
use std::sync::Arc;

use loadngo_renderer::{FrameCommand, GraphicsBackend, RendererError};
#[cfg(any(target_os = "android", target_os = "linux", test))]
use ui_core::geometry::Color;
#[cfg(any(target_os = "android", target_os = "linux", test))]
use ui_core::geometry::Point;

#[cfg(target_os = "android")]
mod android_egl;
#[cfg(target_os = "linux")]
pub mod linux_egl;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlesBackendState {
    UnboundSurface,
    Headless,
    Ready,
    SurfaceBound,
}

#[derive(Clone)]
pub struct GlesImageResource {
    pub width: i32,
    pub height: i32,
    pub rgba8: Arc<[u8]>,
    pub identity: usize,
}

#[cfg(any(target_os = "android", target_os = "linux"))]
fn image_resource_changed(previous: &GlesImageResource, next: &GlesImageResource) -> bool {
    previous.width != next.width
        || previous.height != next.height
        || previous.identity != next.identity
}

pub struct GlesBackend {
    state: GlesBackendState,
    recorded_commands: Vec<FrameCommand>,
    frame_open: bool,
    surface_width: i32,
    surface_height: i32,
    #[cfg(any(target_os = "android", target_os = "linux"))]
    solid_program: u32,
    #[cfg(any(target_os = "android", target_os = "linux"))]
    solid_vbo: u32,
    #[cfg(any(target_os = "android", target_os = "linux"))]
    textured_program: u32,
    #[cfg(any(target_os = "android", target_os = "linux"))]
    textured_vbo: u32,
    #[cfg(any(target_os = "android", target_os = "linux"))]
    image_resources: HashMap<String, GlesImageResource>,
    #[cfg(any(target_os = "android", target_os = "linux"))]
    gpu_textures: HashMap<String, u32>,
    #[cfg(target_os = "android")]
    display: Option<android::EglDisplay>,
    #[cfg(target_os = "android")]
    context: Option<android::EglContext>,
    #[cfg(target_os = "android")]
    surface: Option<android::EglSurface>,
    #[cfg(target_os = "linux")]
    linux_binding: Option<linux_egl::LinuxEglBinding>,
}

unsafe impl Send for GlesBackend {}
unsafe impl Sync for GlesBackend {}

impl Default for GlesBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl GlesBackend {
    pub fn new() -> Self {
        Self {
            state: GlesBackendState::UnboundSurface,
            recorded_commands: Vec::new(),
            frame_open: false,
            surface_width: 0,
            surface_height: 0,
            #[cfg(any(target_os = "android", target_os = "linux"))]
            solid_program: 0,
            #[cfg(any(target_os = "android", target_os = "linux"))]
            solid_vbo: 0,
            #[cfg(any(target_os = "android", target_os = "linux"))]
            textured_program: 0,
            #[cfg(any(target_os = "android", target_os = "linux"))]
            textured_vbo: 0,
            #[cfg(any(target_os = "android", target_os = "linux"))]
            image_resources: HashMap::new(),
            #[cfg(any(target_os = "android", target_os = "linux"))]
            gpu_textures: HashMap::new(),
            #[cfg(target_os = "android")]
            display: None,
            #[cfg(target_os = "android")]
            context: None,
            #[cfg(target_os = "android")]
            surface: None,
            #[cfg(target_os = "linux")]
            linux_binding: None,
        }
    }

    pub fn new_headless() -> Self {
        Self {
            state: GlesBackendState::Headless,
            recorded_commands: Vec::new(),
            frame_open: false,
            surface_width: 0,
            surface_height: 0,
            #[cfg(any(target_os = "android", target_os = "linux"))]
            solid_program: 0,
            #[cfg(any(target_os = "android", target_os = "linux"))]
            solid_vbo: 0,
            #[cfg(any(target_os = "android", target_os = "linux"))]
            textured_program: 0,
            #[cfg(any(target_os = "android", target_os = "linux"))]
            textured_vbo: 0,
            #[cfg(any(target_os = "android", target_os = "linux"))]
            image_resources: HashMap::new(),
            #[cfg(any(target_os = "android", target_os = "linux"))]
            gpu_textures: HashMap::new(),
            #[cfg(target_os = "android")]
            display: None,
            #[cfg(target_os = "android")]
            context: None,
            #[cfg(target_os = "android")]
            surface: None,
            #[cfg(target_os = "linux")]
            linux_binding: None,
        }
    }

    pub fn state(&self) -> GlesBackendState {
        self.state
    }

    pub fn update_surface_size(&mut self, width: i32, height: i32) {
        self.surface_width = width.max(1);
        self.surface_height = height.max(1);
        #[cfg(target_os = "linux")]
        if let Some(binding) = self.linux_binding.as_mut() {
            linux_egl::resize(binding, self.surface_width, self.surface_height);
        }
    }

    pub fn supports_commands(&self, commands: &[FrameCommand]) -> bool {
        commands.iter().all(|command| {
            matches!(
                command,
                FrameCommand::Clear { .. }
                    | FrameCommand::FillRect { .. }
                    | FrameCommand::StrokeRect { .. }
                    | FrameCommand::Line { .. }
                    | FrameCommand::Circle { .. }
                    | FrameCommand::Polyline { .. }
                    | FrameCommand::Arc { .. }
                    | FrameCommand::Image(_)
            )
        })
    }

    pub fn recorded_commands(&self) -> &[FrameCommand] {
        &self.recorded_commands
    }

    pub fn sync_image_resources(
        &mut self,
        resources: impl IntoIterator<Item = (String, GlesImageResource)>,
    ) {
        #[cfg(any(target_os = "android", target_os = "linux"))]
        {
            let mut next = HashMap::new();
            for (key, resource) in resources {
                next.insert(key, resource);
            }
            let mut changed_keys = Vec::new();
            for (key, resource) in next.iter_mut() {
                if let Some(previous) = self.image_resources.get(key) {
                    if image_resource_changed(previous, resource) {
                        changed_keys.push(key.clone());
                    } else {
                        *resource = previous.clone();
                    }
                }
            }
            for key in changed_keys {
                if let Some(texture) = self.gpu_textures.remove(&key) {
                    #[cfg(target_os = "android")]
                    android::destroy_texture(&texture);
                    #[cfg(target_os = "linux")]
                    linux_egl::destroy_texture(&texture);
                }
            }
            self.image_resources.retain(|key, _| next.contains_key(key));
            self.gpu_textures.retain(|key, texture| {
                if next.contains_key(key) {
                    true
                } else {
                    #[cfg(target_os = "android")]
                    android::destroy_texture(texture);
                    #[cfg(target_os = "linux")]
                    linux_egl::destroy_texture(texture);
                    false
                }
            });
            self.image_resources = next;
        }

        #[cfg(all(not(target_os = "android"), not(target_os = "linux")))]
        {
            let _ = resources.into_iter().count();
        }
    }

    #[cfg(target_os = "android")]
    pub fn try_bind_native_window(
        window: &ndk::native_window::NativeWindow,
    ) -> Result<Self, RendererError> {
        let (display, context, surface) = android::bind_native_window(window)?;
        Ok(Self {
            state: GlesBackendState::SurfaceBound,
            recorded_commands: Vec::new(),
            frame_open: false,
            surface_width: window.width(),
            surface_height: window.height(),
            solid_program: 0,
            solid_vbo: 0,
            textured_program: 0,
            textured_vbo: 0,
            image_resources: HashMap::new(),
            gpu_textures: HashMap::new(),
            display: Some(display),
            context: Some(context),
            surface: Some(surface),
        })
    }

    #[cfg(not(target_os = "android"))]
    pub fn try_bind_native_window(_window: &()) -> Result<Self, RendererError> {
        Err(RendererError::Backend(
            "OpenGL ES backend is only available on Android in this build".to_string(),
        ))
    }

    #[cfg(target_os = "linux")]
    pub fn try_bind_linux_window(
        handles: &linux_egl::LinuxEglWindowHandles,
        width: i32,
        height: i32,
    ) -> Result<Self, RendererError> {
        let binding = linux_egl::bind_window(handles, width, height)?;
        Ok(Self {
            state: GlesBackendState::SurfaceBound,
            recorded_commands: Vec::new(),
            frame_open: false,
            surface_width: width.max(1),
            surface_height: height.max(1),
            solid_program: 0,
            solid_vbo: 0,
            textured_program: 0,
            textured_vbo: 0,
            image_resources: HashMap::new(),
            gpu_textures: HashMap::new(),
            linux_binding: Some(binding),
        })
    }
}

impl Drop for GlesBackend {
    fn drop(&mut self) {
        #[cfg(target_os = "android")]
        if let (Some(display), Some(context), Some(surface)) = (
            self.display.take(),
            self.context.take(),
            self.surface.take(),
        ) {
            android::destroy_scene_resources(
                &mut self.solid_program,
                &mut self.solid_vbo,
                &mut self.textured_program,
                &mut self.textured_vbo,
                &mut self.gpu_textures,
            );
            android::destroy(display, context, surface);
        }

        #[cfg(target_os = "linux")]
        if let Some(binding) = self.linux_binding.take() {
            linux_egl::destroy_scene_resources(
                &mut self.solid_program,
                &mut self.solid_vbo,
                &mut self.textured_program,
                &mut self.textured_vbo,
                &mut self.gpu_textures,
            );
            linux_egl::destroy(binding);
        }
    }
}

impl GraphicsBackend for GlesBackend {
    fn begin_frame(&mut self) -> Result<(), RendererError> {
        self.frame_open = true;
        self.recorded_commands.clear();
        Ok(())
    }

    fn submit(&mut self, commands: &[FrameCommand]) -> Result<(), RendererError> {
        if !self.frame_open {
            return Err(RendererError::Backend(
                "cannot submit GLES commands outside an open frame".to_string(),
            ));
        }
        self.recorded_commands.extend_from_slice(commands);
        Ok(())
    }

    fn end_frame(&mut self) -> Result<(), RendererError> {
        if !self.frame_open {
            return Err(RendererError::Backend(
                "cannot end a GLES frame that was never opened".to_string(),
            ));
        }
        self.frame_open = false;

        if !self.supports_commands(&self.recorded_commands) {
            return Err(RendererError::Backend(
                "GLES backend only supports clear, rect, polyline, and image frames right now"
                    .to_string(),
            ));
        }

        match self.state {
            GlesBackendState::Headless => Ok(()),
            GlesBackendState::SurfaceBound => {
                #[cfg(target_os = "android")]
                {
                    let display = self.display.ok_or_else(|| {
                        RendererError::Backend("EGL display is unavailable".to_string())
                    })?;
                    let context = self.context.ok_or_else(|| {
                        RendererError::Backend("EGL context is unavailable".to_string())
                    })?;
                    let surface = self.surface.ok_or_else(|| {
                        RendererError::Backend("EGL surface is unavailable".to_string())
                    })?;
                    android::present_scene(
                        display,
                        context,
                        surface,
                        &mut self.solid_program,
                        &mut self.solid_vbo,
                        &mut self.textured_program,
                        &mut self.textured_vbo,
                        &self.image_resources,
                        &mut self.gpu_textures,
                        self.surface_width.max(1),
                        self.surface_height.max(1),
                        &self.recorded_commands,
                    )
                }
                #[cfg(target_os = "linux")]
                {
                    let binding = self.linux_binding.as_ref().ok_or_else(|| {
                        RendererError::Backend("Linux EGL binding is unavailable".to_string())
                    })?;
                    linux_egl::present_scene(
                        binding,
                        &mut self.solid_program,
                        &mut self.solid_vbo,
                        &mut self.textured_program,
                        &mut self.textured_vbo,
                        &self.image_resources,
                        &mut self.gpu_textures,
                        self.surface_width.max(1),
                        self.surface_height.max(1),
                        &self.recorded_commands,
                    )
                }
                #[cfg(all(not(target_os = "android"), not(target_os = "linux")))]
                {
                    Err(RendererError::Backend(
                        "Android GLES surface rendering is unavailable on this platform"
                            .to_string(),
                    ))
                }
            }
            _ => Err(RendererError::Backend(
                "Android GLES backend is not bound to a surface".to_string(),
            )),
        }
    }
}

#[cfg(any(target_os = "android", target_os = "linux", test))]
fn stroke_rects(
    rect: ui_core::geometry::Rect,
    color: Color,
    thickness: i32,
) -> [(ui_core::geometry::Rect, Color); 4] {
    let thickness = thickness.max(1) as f32;
    [
        (
            ui_core::geometry::Rect {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: thickness,
            },
            color,
        ),
        (
            ui_core::geometry::Rect {
                x: rect.x,
                y: rect.y + rect.height - thickness,
                width: rect.width,
                height: thickness,
            },
            color,
        ),
        (
            ui_core::geometry::Rect {
                x: rect.x,
                y: rect.y,
                width: thickness,
                height: rect.height,
            },
            color,
        ),
        (
            ui_core::geometry::Rect {
                x: rect.x + rect.width - thickness,
                y: rect.y,
                width: thickness,
                height: rect.height,
            },
            color,
        ),
    ]
}

#[cfg(any(target_os = "android", target_os = "linux", test))]
pub(crate) fn polyline_triangle_vertices(
    points: &[Point],
    thickness: i32,
    closed: bool,
    width: i32,
    height: i32,
) -> Vec<f32> {
    let mut vertices = Vec::new();
    let mut append_segment = |from: Point, to: Point| {
        let dx = to.x - from.x;
        let dy = to.y - from.y;
        let length = (dx * dx + dy * dy).sqrt();
        if length <= f32::EPSILON {
            return;
        }
        let half = (thickness.max(1) as f32) * 0.5;
        let nx = -dy / length * half;
        let ny = dx / length * half;

        let p0 = Point {
            x: from.x + nx,
            y: from.y + ny,
        };
        let p1 = Point {
            x: to.x + nx,
            y: to.y + ny,
        };
        let p2 = Point {
            x: from.x - nx,
            y: from.y - ny,
        };
        let p3 = Point {
            x: to.x - nx,
            y: to.y - ny,
        };

        push_clip_vertex(&mut vertices, p0, width, height);
        push_clip_vertex(&mut vertices, p1, width, height);
        push_clip_vertex(&mut vertices, p2, width, height);
        push_clip_vertex(&mut vertices, p2, width, height);
        push_clip_vertex(&mut vertices, p1, width, height);
        push_clip_vertex(&mut vertices, p3, width, height);
    };

    for segment in points.windows(2) {
        append_segment(segment[0], segment[1]);
    }
    if closed && points.len() > 2 {
        append_segment(*points.last().unwrap(), points[0]);
    }

    vertices
}

#[cfg(any(target_os = "android", target_os = "linux", test))]
pub(crate) fn line_triangle_vertices(
    from: Point,
    to: Point,
    thickness: i32,
    width: i32,
    height: i32,
) -> Vec<f32> {
    polyline_triangle_vertices(&[from, to], thickness, false, width, height)
}

#[cfg(any(target_os = "android", target_os = "linux", test))]
pub(crate) fn circle_triangle_vertices(
    center: Point,
    radius: f32,
    width: i32,
    height: i32,
) -> Vec<f32> {
    let radius = radius.max(1.0);
    let segments = ((radius * 0.75).ceil() as usize).clamp(12, 48);
    let mut vertices = Vec::with_capacity(segments * 6);

    for index in 0..segments {
        let a0 = (index as f32 / segments as f32) * std::f32::consts::TAU;
        let a1 = ((index + 1) as f32 / segments as f32) * std::f32::consts::TAU;
        let p0 = Point {
            x: center.x + radius * a0.cos(),
            y: center.y + radius * a0.sin(),
        };
        let p1 = Point {
            x: center.x + radius * a1.cos(),
            y: center.y + radius * a1.sin(),
        };
        push_clip_vertex(&mut vertices, center, width, height);
        push_clip_vertex(&mut vertices, p0, width, height);
        push_clip_vertex(&mut vertices, p1, width, height);
    }

    vertices
}

#[cfg(any(target_os = "android", target_os = "linux", test))]
pub(crate) fn arc_polyline_points(
    center: Point,
    radius: f32,
    start_angle: f32,
    sweep_angle: f32,
) -> Vec<Point> {
    let radius = radius.max(1.0);
    let arc_len = radius * sweep_angle.abs().max(0.05);
    let segments = (arc_len / 6.0).ceil() as usize;
    let segments = segments.clamp(8, 96);

    (0..=segments)
        .map(|index| {
            let t = index as f32 / segments as f32;
            let angle = start_angle + sweep_angle * t;
            Point {
                x: center.x + radius * angle.cos(),
                y: center.y + radius * angle.sin(),
            }
        })
        .collect()
}

#[cfg(any(target_os = "android", target_os = "linux", test))]
fn push_clip_vertex(vertices: &mut Vec<f32>, point: Point, width: i32, height: i32) {
    let width = width.max(1) as f32;
    let height = height.max(1) as f32;
    vertices.push((point.x / width) * 2.0 - 1.0);
    vertices.push(1.0 - (point.y / height) * 2.0);
}

#[cfg(target_os = "android")]
mod android {
    use std::ffi::c_void;
    use std::ptr;

    use loadngo_renderer::{FrameCommand, ImageRequest, RendererError};
    use ndk::native_window::NativeWindow;
    use ui_core::geometry::{Color, Point};

    pub type EglDisplay = *mut c_void;
    pub type EglContext = *mut c_void;
    pub type EglSurface = *mut c_void;
    pub type GlTexture = u32;
    type EglConfig = *mut c_void;
    type EglBoolean = i32;
    type EglInt = i32;

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

    #[link(name = "EGL")]
    unsafe extern "C" {
        fn eglGetDisplay(display_id: *mut c_void) -> EglDisplay;
        fn eglInitialize(display: EglDisplay, major: *mut EglInt, minor: *mut EglInt)
            -> EglBoolean;
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

    pub fn bind_native_window(
        window: &NativeWindow,
    ) -> Result<(EglDisplay, EglContext, EglSurface), RendererError> {
        unsafe {
            let display = eglGetDisplay(EGL_DEFAULT_DISPLAY);
            if display == EGL_NO_DISPLAY {
                return Err(last_egl_error("eglGetDisplay"));
            }

            let mut major = 0;
            let mut minor = 0;
            if eglInitialize(display, &mut major, &mut minor) == EGL_FALSE {
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
                let _ = eglTerminate(display);
                return Err(last_egl_error("eglChooseConfig"));
            }

            if eglBindAPI(EGL_OPENGL_ES_API) == EGL_FALSE {
                let _ = eglTerminate(display);
                return Err(last_egl_error("eglBindAPI"));
            }

            let context_attribs = [EGL_CONTEXT_CLIENT_VERSION, 3, EGL_NONE];
            let context =
                eglCreateContext(display, config, EGL_NO_CONTEXT, context_attribs.as_ptr());
            if context == EGL_NO_CONTEXT {
                let _ = eglTerminate(display);
                return Err(last_egl_error("eglCreateContext"));
            }

            let surface =
                eglCreateWindowSurface(display, config, window.ptr().as_ptr().cast(), ptr::null());
            if surface == EGL_NO_SURFACE {
                let _ = eglDestroyContext(display, context);
                let _ = eglTerminate(display);
                return Err(last_egl_error("eglCreateWindowSurface"));
            }

            if eglMakeCurrent(display, surface, surface, context) == EGL_FALSE {
                let _ = eglDestroySurface(display, surface);
                let _ = eglDestroyContext(display, context);
                let _ = eglTerminate(display);
                return Err(last_egl_error("eglMakeCurrent"));
            }

            Ok((display, context, surface))
        }
    }

    pub fn present_scene(
        display: EglDisplay,
        context: EglContext,
        surface: EglSurface,
        solid_program: &mut u32,
        solid_vbo: &mut u32,
        textured_program: &mut u32,
        textured_vbo: &mut u32,
        image_resources: &std::collections::HashMap<String, super::GlesImageResource>,
        gpu_textures: &mut std::collections::HashMap<String, GlTexture>,
        width: i32,
        height: i32,
        commands: &[FrameCommand],
    ) -> Result<(), RendererError> {
        unsafe {
            if eglMakeCurrent(display, surface, surface, context) == EGL_FALSE {
                return Err(last_egl_error("eglMakeCurrent"));
            }
            glViewport(0, 0, width, height);
            glEnable(GL_BLEND);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
            let mut cleared = false;
            if !commands
                .iter()
                .any(|command| matches!(command, FrameCommand::Clear { .. }))
            {
                glClearColor(0.0, 0.0, 0.0, 1.0);
                glClear(GL_COLOR_BUFFER_BIT);
                cleared = true;
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
                        cleared = true;
                    }
                    FrameCommand::FillRect { rect, color } => {
                        ensure_solid_pipeline(solid_program, solid_vbo)?;
                        draw_solid_rects(
                            *solid_program,
                            *solid_vbo,
                            width,
                            height,
                            &[(*rect, *color)],
                        )?;
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
                    FrameCommand::Line {
                        from,
                        to,
                        color,
                        thickness,
                    } => {
                        ensure_solid_pipeline(solid_program, solid_vbo)?;
                        draw_line(
                            *solid_program,
                            *solid_vbo,
                            width,
                            height,
                            *from,
                            *to,
                            *color,
                            *thickness,
                        )?;
                    }
                    FrameCommand::Circle {
                        center,
                        radius,
                        color,
                    } => {
                        ensure_solid_pipeline(solid_program, solid_vbo)?;
                        draw_circle(
                            *solid_program,
                            *solid_vbo,
                            width,
                            height,
                            *center,
                            *radius,
                            *color,
                        )?;
                    }
                    FrameCommand::Arc {
                        center,
                        radius,
                        start_angle,
                        sweep_angle,
                        color,
                        thickness,
                    } => {
                        ensure_solid_pipeline(solid_program, solid_vbo)?;
                        draw_arc(
                            *solid_program,
                            *solid_vbo,
                            width,
                            height,
                            *center,
                            *radius,
                            *start_angle,
                            *sweep_angle,
                            *color,
                            *thickness,
                        )?;
                    }
                    FrameCommand::Polyline {
                        points,
                        color,
                        thickness,
                        closed,
                    } => {
                        ensure_solid_pipeline(solid_program, solid_vbo)?;
                        draw_polyline(
                            *solid_program,
                            *solid_vbo,
                            width,
                            height,
                            points.as_slice(),
                            *color,
                            *thickness,
                            *closed,
                        )?;
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
                    FrameCommand::ParticleBatch { .. } => {}
                    FrameCommand::Text(_) => {}
                }
            }

            if eglSwapBuffers(display, surface) == EGL_FALSE {
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
        gpu_textures: &mut std::collections::HashMap<String, GlTexture>,
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

    pub fn destroy_texture(texture: &GlTexture) {
        unsafe {
            glDeleteTextures(1, texture as *const u32);
        }
    }

    pub fn destroy(display: EglDisplay, context: EglContext, surface: EglSurface) {
        unsafe {
            let _ = eglMakeCurrent(display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
            let _ = eglDestroySurface(display, surface);
            let _ = eglDestroyContext(display, context);
            let _ = eglTerminate(display);
        }
    }

    fn last_egl_error(label: &str) -> RendererError {
        let code = unsafe { eglGetError() };
        RendererError::Backend(format!("{label} failed with EGL error 0x{code:04x}"))
    }

    fn ensure_solid_pipeline(
        solid_program: &mut u32,
        solid_vbo: &mut u32,
    ) -> Result<(), RendererError> {
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
                    return Err(RendererError::Backend(format!(
                        "glLinkProgram failed: {message}"
                    )));
                }
                *solid_program = program;
            }

            if *solid_vbo == 0 {
                glGenBuffers(1, solid_vbo as *mut u32);
                if *solid_vbo == 0 {
                    return Err(RendererError::Backend(
                        "glGenBuffers failed for GLES solid rect VBO".to_string(),
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
                    return Err(RendererError::Backend(format!(
                        "glLinkProgram failed: {message}"
                    )));
                }
                *textured_program = program;
            }

            if *textured_vbo == 0 {
                glGenBuffers(1, textured_vbo as *mut u32);
                if *textured_vbo == 0 {
                    return Err(RendererError::Backend(
                        "glGenBuffers failed for GLES textured quad VBO".to_string(),
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

            let u_color_name = b"u_color\0";
            let color_location = glGetUniformLocation(program, u_color_name.as_ptr().cast());
            if color_location < 0 {
                return Err(RendererError::Backend(
                    "u_color uniform not found in GLES solid rect program".to_string(),
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

    fn draw_polyline(
        program: u32,
        vbo: u32,
        width: i32,
        height: i32,
        points: &[Point],
        color: Color,
        thickness: i32,
        closed: bool,
    ) -> Result<(), RendererError> {
        let vertices = super::polyline_triangle_vertices(points, thickness, closed, width, height);
        if vertices.is_empty() {
            return Ok(());
        }

        unsafe {
            glUseProgram(program);
            glBindBuffer(GL_ARRAY_BUFFER, vbo);
            glEnableVertexAttribArray(0);
            glVertexAttribPointer(0, 2, GL_FLOAT, 0, 0, ptr::null());

            let u_color_name = b"u_color\0";
            let color_location = glGetUniformLocation(program, u_color_name.as_ptr().cast());
            if color_location < 0 {
                return Err(RendererError::Backend(
                    "u_color uniform not found in GLES solid polyline program".to_string(),
                ));
            }

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
            glDrawArrays(GL_TRIANGLES, 0, (vertices.len() / 2) as i32);

            glDisableVertexAttribArray(0);
            glBindBuffer(GL_ARRAY_BUFFER, 0);
        }

        Ok(())
    }

    fn draw_line(
        program: u32,
        vbo: u32,
        width: i32,
        height: i32,
        from: Point,
        to: Point,
        color: Color,
        thickness: i32,
    ) -> Result<(), RendererError> {
        let vertices = super::line_triangle_vertices(from, to, thickness, width, height);
        draw_solid_vertices(program, vbo, &vertices, color, "GLES solid line")
    }

    fn draw_circle(
        program: u32,
        vbo: u32,
        width: i32,
        height: i32,
        center: Point,
        radius: f32,
        color: Color,
    ) -> Result<(), RendererError> {
        let vertices = super::circle_triangle_vertices(center, radius, width, height);
        draw_solid_vertices(program, vbo, &vertices, color, "GLES solid circle")
    }

    fn draw_arc(
        program: u32,
        vbo: u32,
        width: i32,
        height: i32,
        center: Point,
        radius: f32,
        start_angle: f32,
        sweep_angle: f32,
        color: Color,
        thickness: i32,
    ) -> Result<(), RendererError> {
        let points = super::arc_polyline_points(center, radius, start_angle, sweep_angle);
        let vertices = super::polyline_triangle_vertices(&points, thickness, false, width, height);
        draw_solid_vertices(program, vbo, &vertices, color, "GLES solid arc")
    }

    fn draw_solid_vertices(
        program: u32,
        vbo: u32,
        vertices: &[f32],
        color: Color,
        label: &str,
    ) -> Result<(), RendererError> {
        if vertices.is_empty() {
            return Ok(());
        }

        unsafe {
            glUseProgram(program);
            glBindBuffer(GL_ARRAY_BUFFER, vbo);
            glEnableVertexAttribArray(0);
            glVertexAttribPointer(0, 2, GL_FLOAT, 0, 0, ptr::null());

            let u_color_name = b"u_color\0";
            let color_location = glGetUniformLocation(program, u_color_name.as_ptr().cast());
            if color_location < 0 {
                return Err(RendererError::Backend(format!(
                    "u_color uniform not found in {label} program"
                )));
            }

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
            glDrawArrays(GL_TRIANGLES, 0, (vertices.len() / 2) as i32);

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
        gpu_textures: &mut std::collections::HashMap<String, GlTexture>,
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
                    "GLES textured shader uniforms are unavailable".to_string(),
                ));
            }

            glActiveTexture(GL_TEXTURE0);
            glUniform1i(u_tex, 0);

            for request in images {
                let Some(resource) = image_resources.get(request.image_key.as_str()) else {
                    continue;
                };
                let texture =
                    ensure_gpu_texture(request.image_key.as_str(), resource, gpu_textures)?;
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
        gpu_textures: &mut std::collections::HashMap<String, GlTexture>,
    ) -> Result<GlTexture, RendererError> {
        if let Some(texture) = gpu_textures.get(key) {
            return Ok(*texture);
        }

        unsafe {
            let mut texture = 0;
            glGenTextures(1, &mut texture);
            if texture == 0 {
                return Err(RendererError::Backend(
                    "glGenTextures failed for Android GLES image".to_string(),
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
        let x0 = rect.x as f32;
        let y0 = rect.y as f32;
        let x1 = (rect.x + rect.width) as f32;
        let y1 = (rect.y + rect.height) as f32;

        let to_clip_x = |x: f32| (x / width) * 2.0 - 1.0;
        let to_clip_y = |y: f32| 1.0 - (y / height) * 2.0;

        let left = to_clip_x(x0);
        let right = to_clip_x(x1);
        let top = to_clip_y(y0);
        let bottom = to_clip_y(y1);

        [
            left, top, right, top, left, bottom, left, bottom, right, top, right, bottom,
        ]
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
                return Err(RendererError::Backend(format!(
                    "glCompileShader failed: {message}"
                )));
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
            glGetShaderInfoLog(
                shader,
                length,
                ptr::null_mut(),
                buffer.as_mut_ptr().cast::<i8>(),
            );
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
            glGetProgramInfoLog(
                program,
                length,
                ptr::null_mut(),
                buffer.as_mut_ptr().cast::<i8>(),
            );
            String::from_utf8_lossy(&buffer)
                .trim_end_matches(char::from(0))
                .to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headless_backend_records_clear_command() {
        let mut backend = GlesBackend::new_headless();
        backend.begin_frame().unwrap();
        backend
            .submit(&[FrameCommand::Clear {
                color: Color::rgba(1, 2, 3, 255),
            }])
            .unwrap();
        backend.end_frame().unwrap();
        assert_eq!(backend.recorded_commands().len(), 1);
    }

    #[test]
    fn backend_accepts_solid_rect_commands() {
        let mut backend = GlesBackend::new_headless();
        backend.begin_frame().unwrap();
        backend
            .submit(&[FrameCommand::FillRect {
                rect: ui_core::geometry::Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 10.0,
                    height: 10.0,
                },
                color: Color::rgba(4, 5, 6, 255),
            }])
            .unwrap();
        backend.end_frame().unwrap();
    }

    #[test]
    fn stroke_rects_expands_stroke_rects() {
        let rects = stroke_rects(
            ui_core::geometry::Rect {
                x: 2.0,
                y: 4.0,
                width: 20.0,
                height: 10.0,
            },
            Color::rgba(7, 8, 9, 255),
            2,
        );
        assert_eq!(rects.len(), 4);
    }

    #[test]
    fn backend_accepts_polyline_commands() {
        let backend = GlesBackend::new_headless();
        assert!(backend.supports_commands(&[FrameCommand::Polyline {
            points: vec![
                Point { x: 10.0, y: 10.0 },
                Point { x: 30.0, y: 20.0 },
                Point { x: 50.0, y: 10.0 },
            ],
            color: Color::rgba(0x55, 0xaa, 0xff, 0xff),
            thickness: 3,
            closed: false,
        }]));
    }

    #[test]
    fn backend_accepts_arc_commands() {
        let backend = GlesBackend::new_headless();
        assert!(backend.supports_commands(&[FrameCommand::Arc {
            center: Point { x: 24.0, y: 24.0 },
            radius: 12.0,
            start_angle: 0.0,
            sweep_angle: std::f32::consts::PI * 0.75,
            color: Color::rgba(0x55, 0xaa, 0xff, 0xff),
            thickness: 3,
        }]));
    }

    #[test]
    fn polyline_triangle_vertices_emit_two_triangles_per_segment() {
        let vertices = polyline_triangle_vertices(
            &[
                Point { x: 10.0, y: 10.0 },
                Point { x: 30.0, y: 20.0 },
                Point { x: 50.0, y: 10.0 },
            ],
            4,
            false,
            100,
            100,
        );
        assert_eq!(vertices.len(), 24);
    }

    #[test]
    fn closed_polyline_adds_closing_segment_vertices() {
        let vertices = polyline_triangle_vertices(
            &[
                Point { x: 10.0, y: 10.0 },
                Point { x: 30.0, y: 20.0 },
                Point { x: 50.0, y: 10.0 },
            ],
            4,
            true,
            100,
            100,
        );
        assert_eq!(vertices.len(), 36);
    }

    #[test]
    fn line_triangle_vertices_emit_one_quad() {
        let vertices = line_triangle_vertices(
            Point { x: 10.0, y: 10.0 },
            Point { x: 30.0, y: 20.0 },
            4,
            100,
            100,
        );
        assert_eq!(vertices.len(), 12);
    }

    #[test]
    fn circle_triangle_vertices_emit_triangle_fan_geometry() {
        let vertices = circle_triangle_vertices(Point { x: 50.0, y: 50.0 }, 12.0, 100, 100);
        assert!(vertices.len() >= 12 * 6);
        assert_eq!(vertices.len() % 6, 0);
    }

    #[test]
    fn arc_polyline_points_emit_a_progressive_curve() {
        let points = arc_polyline_points(
            Point { x: 50.0, y: 50.0 },
            12.0,
            0.0,
            std::f32::consts::PI * 0.5,
        );
        assert!(points.len() >= 9);
        assert_ne!(points.first(), points.last());
    }

    #[test]
    fn image_resource_change_detects_new_pixels_for_same_key() {
        let previous = GlesImageResource {
            width: 2,
            height: 2,
            rgba8: Arc::<[u8]>::from(vec![1u8; 16]),
            identity: 1,
        };
        let next = GlesImageResource {
            width: 2,
            height: 2,
            rgba8: Arc::<[u8]>::from(vec![2u8; 16]),
            identity: 2,
        };
        assert!(image_resource_changed(&previous, &next));
    }

    #[test]
    fn image_resource_change_allows_reusing_same_pixels() {
        let rgba = Arc::<[u8]>::from(vec![3u8; 16]);
        let previous = GlesImageResource {
            width: 2,
            height: 2,
            rgba8: Arc::clone(&rgba),
            identity: 7,
        };
        let next = GlesImageResource {
            width: 2,
            height: 2,
            rgba8: rgba,
            identity: 7,
        };
        assert!(!image_resource_changed(&previous, &next));
    }

    #[test]
    fn image_resource_change_allows_rebuilt_pixels_for_same_identity() {
        let previous = GlesImageResource {
            width: 2,
            height: 2,
            rgba8: Arc::<[u8]>::from(vec![4u8; 16]),
            identity: 11,
        };
        let next = GlesImageResource {
            width: 2,
            height: 2,
            rgba8: Arc::<[u8]>::from(vec![4u8; 16]),
            identity: 11,
        };
        assert!(!image_resource_changed(&previous, &next));
    }
}
