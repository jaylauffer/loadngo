use std::ffi::c_void;
use std::ptr;

use loadngo_renderer::RendererError;
use ndk::native_window::NativeWindow;

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
    fn eglDestroySurface(display: EglDisplay, surface: EglSurface) -> EglBoolean;
    fn eglDestroyContext(display: EglDisplay, context: EglContext) -> EglBoolean;
    fn eglTerminate(display: EglDisplay) -> EglBoolean;
    fn eglGetError() -> EglInt;
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
        let context = eglCreateContext(display, config, EGL_NO_CONTEXT, context_attribs.as_ptr());
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
