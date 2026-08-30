//! Shared JNI plumbing for Android host code. Attaching to the JVM and
//! calling a Java method with exception-checking is the same handful of
//! steps whether the caller is `audio.rs`'s `MediaPlayer` wrapper or the
//! `WindowInsets`/immersive-mode/gesture-exclusion code in `android.rs` —
//! this module is the one place that logic lives. See
//! `loadngo`'s `docs/ANDROID_HOST.md` for the on-demand JNI access pattern
//! this builds on (`ndk_context::android_context()`, callable from any
//! thread at any time, not just from inside an `ANativeActivityCallbacks`
//! callback).

use jni::{
    objects::{JObject, JValue},
    JNIEnv, JavaVM,
};
use ndk_context::android_context;

pub(crate) fn with_env<T>(f: impl FnOnce(&mut JNIEnv) -> Result<T, String>) -> Result<T, String> {
    let ctx = android_context();
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }
        .map_err(|err| format!("Android JavaVM unavailable: {err}"))?;
    let mut env = vm
        .attach_current_thread()
        .map_err(|err| format!("Failed to attach Android thread: {err}"))?;
    f(&mut env)
}

pub(crate) fn take_java_exception(env: &mut JNIEnv) -> Option<String> {
    match env.exception_check() {
        Ok(true) => {
            let _ = env.exception_describe();
            let message = match env.exception_occurred() {
                Ok(exception) => {
                    let _ = env.exception_clear();
                    match env.call_method(&exception, "toString", "()Ljava/lang/String;", &[]) {
                        Ok(value) => match value.l() {
                            Ok(obj) => {
                                let string = jni::objects::JString::from(obj);
                                env.get_string(&string)
                                    .map(|value| value.to_string_lossy().into_owned())
                                    .unwrap_or_else(|_| {
                                        "Java exception (failed to decode message)".to_string()
                                    })
                            }
                            Err(_) => "Java exception (failed to read message object)".to_string(),
                        },
                        Err(_) => "Java exception (failed to stringify throwable)".to_string(),
                    }
                }
                Err(_) => {
                    let _ = env.exception_clear();
                    "Java exception (failed to fetch throwable)".to_string()
                }
            };
            Some(message)
        }
        Ok(false) => None,
        Err(err) => Some(format!("Failed to inspect Java exception state: {err}")),
    }
}

pub(crate) fn call_void(
    env: &mut JNIEnv,
    obj: &JObject,
    name: &str,
    sig: &str,
    args: &[JValue],
) -> Result<(), String> {
    if let Err(err) = env.call_method(obj, name, sig, args) {
        let detail = take_java_exception(env)
            .map(|detail| format!(" ({detail})"))
            .unwrap_or_default();
        return Err(format!("Android {name} failed: {err}{detail}"));
    }
    if let Some(detail) = take_java_exception(env) {
        return Err(format!("Android {name} raised Java exception: {detail}"));
    }
    Ok(())
}

pub(crate) fn call_bool(
    env: &mut JNIEnv,
    obj: &JObject,
    name: &str,
    sig: &str,
    args: &[JValue],
) -> Result<bool, String> {
    let value = match env.call_method(obj, name, sig, args) {
        Ok(value) => value,
        Err(err) => {
            let detail = take_java_exception(env)
                .map(|detail| format!(" ({detail})"))
                .unwrap_or_default();
            return Err(format!("Android {name} failed: {err}{detail}"));
        }
    };
    if let Some(detail) = take_java_exception(env) {
        return Err(format!("Android {name} raised Java exception: {detail}"));
    }
    value
        .z()
        .map_err(|err| format!("Android {name} return decode failed: {err}"))
}

pub(crate) fn call_int(
    env: &mut JNIEnv,
    obj: &JObject,
    name: &str,
    sig: &str,
    args: &[JValue],
) -> Result<i32, String> {
    let value = match env.call_method(obj, name, sig, args) {
        Ok(value) => value,
        Err(err) => {
            let detail = take_java_exception(env)
                .map(|detail| format!(" ({detail})"))
                .unwrap_or_default();
            return Err(format!("Android {name} failed: {err}{detail}"));
        }
    };
    if let Some(detail) = take_java_exception(env) {
        return Err(format!("Android {name} raised Java exception: {detail}"));
    }
    value
        .i()
        .map_err(|err| format!("Android {name} return decode failed: {err}"))
}

/// Calls a method that returns a Java object reference. A `null` return is
/// reported as `Ok(None)`, not an error — legitimate for methods like
/// `WindowInsets::getDisplayCutout()` that are nullable by design.
pub(crate) fn call_object<'e>(
    env: &mut JNIEnv<'e>,
    obj: &JObject,
    name: &str,
    sig: &str,
    args: &[JValue],
) -> Result<Option<JObject<'e>>, String> {
    let value = match env.call_method(obj, name, sig, args) {
        Ok(value) => value,
        Err(err) => {
            let detail = take_java_exception(env)
                .map(|detail| format!(" ({detail})"))
                .unwrap_or_default();
            return Err(format!("Android {name} failed: {err}{detail}"));
        }
    };
    if let Some(detail) = take_java_exception(env) {
        return Err(format!("Android {name} raised Java exception: {detail}"));
    }
    let obj = value
        .l()
        .map_err(|err| format!("Android {name} return decode failed: {err}"))?;
    Ok((!obj.is_null()).then_some(obj))
}

/// Calls a static method returning `int` (e.g. `WindowInsets.Type.systemBars()`).
pub(crate) fn call_static_int(
    env: &mut JNIEnv,
    class: &str,
    name: &str,
    sig: &str,
    args: &[JValue],
) -> Result<i32, String> {
    let value = match env.call_static_method(class, name, sig, args) {
        Ok(value) => value,
        Err(err) => {
            let detail = take_java_exception(env)
                .map(|detail| format!(" ({detail})"))
                .unwrap_or_default();
            return Err(format!(
                "Android static {class}.{name} failed: {err}{detail}"
            ));
        }
    };
    if let Some(detail) = take_java_exception(env) {
        return Err(format!(
            "Android static {class}.{name} raised Java exception: {detail}"
        ));
    }
    value
        .i()
        .map_err(|err| format!("Android static {class}.{name} return decode failed: {err}"))
}

pub(crate) fn get_static_int_field(
    env: &mut JNIEnv,
    class: &str,
    name: &str,
) -> Result<i32, String> {
    let value = env
        .get_static_field(class, name, "I")
        .map_err(|err| format!("Android static field {class}.{name} unavailable: {err}"))?;
    value
        .i()
        .map_err(|err| format!("Android static field {class}.{name} decode failed: {err}"))
}

/// Reads an instance `int` field (e.g. `android.graphics.Insets.left`,
/// which — unlike most values this module reads — is a public field, not a
/// method).
pub(crate) fn get_int_field(env: &mut JNIEnv, obj: &JObject, name: &str) -> Result<i32, String> {
    let value = env
        .get_field(obj, name, "I")
        .map_err(|err| format!("Android field {name} unavailable: {err}"))?;
    value
        .i()
        .map_err(|err| format!("Android field {name} decode failed: {err}"))
}
