//! Avoid opening a known half-duplex `default` in its absent direction.
//! This is config inspection, not a replacement for ALSA's open/validation:
//! unknown plugins and config errors still go through the normal probe.

use std::ffi::{CStr, CString};
use std::ptr;

use crate::backend::Direction;

use super::ffi;

struct Root(*mut ffi::SndConfig);

impl Drop for Root {
    fn drop(&mut self) {
        // SAFETY: owned reference returned by snd_config_update_ref.
        unsafe { ffi::snd_config_unref(self.0) };
    }
}

struct Definition(*mut ffi::SndConfig);

impl Drop for Definition {
    fn drop(&mut self) {
        // SAFETY: owned expanded definition, or test-created config tree.
        unsafe { ffi::snd_config_delete(self.0) };
    }
}

fn definition(root: *mut ffi::SndConfig, name: &CStr) -> Option<Definition> {
    let mut config = ptr::null_mut();
    // SAFETY: root and strings are live; ALSA allocates the returned tree.
    let code = unsafe {
        ffi::snd_config_search_definition(root, c"pcm".as_ptr(), name.as_ptr(), &mut config)
    };
    if code >= 0 && !config.is_null() {
        Some(Definition(config))
    } else {
        None
    }
}

fn search(config: *mut ffi::SndConfig, key: &CStr) -> Option<*mut ffi::SndConfig> {
    let mut child = ptr::null_mut();
    // SAFETY: config is live for this call; returned child is borrowed from it.
    let code = unsafe { ffi::snd_config_search(config, key.as_ptr(), &mut child) };
    (code >= 0 && !child.is_null()).then_some(child)
}

fn string(config: *mut ffi::SndConfig) -> Option<CString> {
    let mut value = ptr::null();
    // SAFETY: config is live; copy its borrowed string before returning.
    if unsafe { ffi::snd_config_get_string(config, &mut value) } < 0 || value.is_null() {
        return None;
    }
    Some(unsafe { CStr::from_ptr(value) }.to_owned())
}

fn lacks_direction(
    root: *mut ffi::SndConfig,
    config: *mut ffi::SndConfig,
    direction: Direction,
    depth: usize,
) -> bool {
    // ALSA config aliases may cycle. Leave unfamiliar/deep routes to ALSA.
    if depth >= 16 {
        return false;
    }
    if let Some(alias) = string(config) {
        return definition(root, &alias)
            .is_some_and(|config| lacks_direction(root, config.0, direction, depth + 1));
    }
    let Some(kind) = search(config, c"type").and_then(string) else {
        return false;
    };
    match kind.to_bytes() {
        b"asym" => {
            let key = match direction {
                Direction::Input => c"capture",
                Direction::Output => c"playback",
            };
            search(config, key).is_none()
        }
        // ALSA's standard `default` wraps the card definition in `empty`.
        // `plug` also forwards the direction to its slave unchanged.
        b"empty" | b"plug" => search(config, c"slave.pcm")
            .is_some_and(|slave| lacks_direction(root, slave, direction, depth + 1)),
        _ => false,
    }
}

pub(super) fn default_lacks_direction(direction: Direction) -> bool {
    let mut root = ptr::null_mut();
    // SAFETY: live out-pointer; release the reference with Root's Drop.
    if unsafe { ffi::snd_config_update_ref(&mut root) } < 0 || root.is_null() {
        return false;
    }
    let root = Root(root);
    definition(root.0, c"default")
        .is_some_and(|config| lacks_direction(root.0, config.0, direction, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[link(name = "asound")]
    extern "C" {
        fn snd_config_top(config: *mut *mut ffi::SndConfig) -> i32;
        fn snd_config_load_string(
            config: *mut *mut ffi::SndConfig,
            text: *const std::ffi::c_char,
            size: usize,
        ) -> i32;
    }

    fn assert_directions(text: &str, missing_input: bool, missing_output: bool) {
        let text = CString::new(text).unwrap();
        let mut root = ptr::null_mut();
        let mut config = ptr::null_mut();
        // SAFETY: valid out-pointers and C string; wrappers free both trees.
        unsafe {
            assert_eq!(snd_config_top(&mut root), 0);
            assert_eq!(snd_config_load_string(&mut config, text.as_ptr(), 0), 0);
        }
        let root = Definition(root);
        let config = Definition(config);
        assert_eq!(
            lacks_direction(root.0, config.0, Direction::Input, 0),
            missing_input
        );
        assert_eq!(
            lacks_direction(root.0, config.0, Direction::Output, 0),
            missing_output
        );
    }

    #[test]
    fn half_duplex_configs_skip_only_the_absent_direction() {
        assert_directions("type asym playback.pcm null", true, false);
        assert_directions("type asym capture.pcm null", false, true);
        assert_directions("type asym playback.pcm null capture.pcm null", false, false);
        // Named slave definitions are valid too, without a `.pcm` child.
        assert_directions(
            "type asym playback named_slave capture named_slave",
            false,
            false,
        );
    }

    #[test]
    fn standard_default_wrappers_preserve_half_duplex_detection() {
        assert_directions(
            "type empty slave.pcm { type asym playback.pcm null }",
            true,
            false,
        );
        assert_directions(
            "type plug slave.pcm { type asym capture.pcm null }",
            false,
            true,
        );
    }

    #[test]
    fn other_plugins_still_get_probed() {
        assert_directions("type pipewire", false, false);
        assert_directions("type hw card 0", false, false);
        assert_directions("type empty", false, false);
    }

    #[test]
    fn pcm_aliases_are_followed_and_cycles_are_bounded() {
        let text = c"pcm.playback { type asym playback.pcm null }
            pcm.wrapped { type empty slave.pcm playback }
            pcm.alias wrapped
            pcm.cycle1 { type empty slave.pcm cycle2 }
            pcm.cycle2 { type empty slave.pcm cycle1 }";
        let mut root = ptr::null_mut();
        // SAFETY: live out-pointer and C string; Definition owns the tree.
        unsafe { assert_eq!(snd_config_load_string(&mut root, text.as_ptr(), 0), 0) };
        let root = Definition(root);
        for name in [c"wrapped", c"alias"] {
            let config = definition(root.0, name).unwrap();
            assert!(lacks_direction(root.0, config.0, Direction::Input, 0));
            assert!(!lacks_direction(root.0, config.0, Direction::Output, 0));
        }
        let config = definition(root.0, c"cycle1").unwrap();
        assert!(!lacks_direction(root.0, config.0, Direction::Input, 0));
    }

    #[test]
    fn repeated_enumeration_skips_missing_capture_but_keeps_open_errors_visible() {
        use super::super::{devices, pcm::Pcm};

        const CHILD: &str = "LOADNGO_ALSA_HALF_DUPLEX_TEST";
        if std::env::var_os(CHILD).is_some() {
            for _ in 0..5 {
                let inputs = devices::enumerate(Direction::Input);
                if let Some((first, pcm)) = inputs.first() {
                    assert!(first.is_default);
                    assert_eq!(devices::resolve(Direction::Input, None).unwrap(), *pcm);
                }
                devices::enumerate(Direction::Output);
                assert_eq!(
                    devices::resolve(Direction::Output, None).unwrap(),
                    "default"
                );
            }
            // An actual invalid stream request must still print its diagnostic.
            assert!(Pcm::open("default", true, true).is_err());
            return;
        }

        let config = std::env::temp_dir().join(format!(
            "loadngo-alsa-half-duplex-{}-{}.conf",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(
            &config,
            "pcm.!default { type empty slave.pcm { type asym playback.pcm { type null } } }",
        )
        .unwrap();
        // Isolate ALSA's process-wide config cache and capture actual libasound
        // stderr, rather than asserting only our config parser's return value.
        let test_name = concat!(
            module_path!(),
            "::repeated_enumeration_skips_missing_capture_but_keeps_open_errors_visible"
        )
        .split_once("::")
        .unwrap()
        .1;
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--nocapture"])
            .env(CHILD, "1")
            .env("ALSA_CONFIG_PATH", &config)
            .output();
        std::fs::remove_file(config).unwrap();
        let output = output.unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "{stderr}\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_eq!(
            stderr.matches("capture slave is not defined").count(),
            1,
            "{stderr}"
        );
    }
}
