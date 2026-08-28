# Android Host: System Space Integration

`loadngo-host-desktop`'s Android backend (`android.rs`) implements the game
loop on top of `NativeActivity`, driven by `ANativeActivityCallbacks`. This
document covers the parts of that host responsible for cooperating with
Android's own reserved screen space and window behavior, rather than input
or rendering. Applications decide *whether* to use these (immersive mode,
gesture exclusion); `loadngo` owns *how* — the JNI mechanics of asking the
platform for it.

## JNI access pattern

Android callbacks (`onCreate`, `onPause`, `onResume`, ...) run on the
platform's own thread and hand back a raw `ANativeActivity*`. At `onCreate`,
`ndk_context::initialize_android_context(vm, activity_clazz)` registers the
process-global `JavaVM`/`Activity` pair with the `ndk-context` crate. From
that point on, **any** code on **any** thread can reach the JVM on demand via
`ndk_context::android_context()` — it is not limited to the callback that's
currently executing. `with_env(|env| ...)` (`android_jni.rs`) is the shared
entry point: it attaches the calling thread and hands back a `JNIEnv` for
the duration of the closure. `call_void`/`call_bool`/`call_int`/
`call_object` are thin, exception-checked wrappers around
`JNIEnv::call_method`, and `get_static_int_field` reads a static field (used
for `Build.VERSION.SDK_INT` gating). `MediaPlayerHandle` (`audio.rs`) and
the insets/immersive/gesture-exclusion code below both build on this same
small set of helpers rather than each rolling their own JNI plumbing.

## Safe-area insets

`HostFrame.insets: SafeAreaInsets` (host-core) carries the device's real
reserved screen space in pixels, refreshed on `onResume` and
`onWindowFocusChanged(true)` (the same moments immersive mode is
reapplied — see below) and cached on `AndroidAppState`, not re-queried every
frame. Derivation:

`Activity.getWindow().getDecorView().getRootWindowInsets()`, then, on
`SDK_INT >= 30`, `WindowInsets.getInsets(WindowInsets.Type.systemBars())` —
an `android.graphics.Insets` object with public `left`/`top`/`right`/`bottom`
**fields** (not methods; read via a dedicated `get_int_field` JNI helper).
Below API 30, the four legacy `getSystemWindowInset{Left,Top,Right,Bottom}()`
methods.

This deliberately excludes `WindowInsets.getDisplayCutout()`'s safe
insets — a display cutout sits at one point along an edge (the front
camera's position), but a cutout-avoidance margin is a conservative margin
for the *entire* edge, which is wrong for content anchored somewhere else
on that edge. This took two rounds to actually achieve, both found through
on-device verification rather than API reading:

1. The first version took the max of `getDisplayCutout()`'s `getSafeInset*`
   and the legacy system-bar inset per edge. On-device, this pushed a
   bottom-anchored touch control inward by the cutout's full width even
   though the cutout itself was nowhere near the bottom of the screen —
   visibly wasted space with nothing to actually avoid there.
2. The fix looked obvious: just drop the `getDisplayCutout()` call and
   keep only the legacy `getSystemWindowInset*` methods. It didn't work —
   the *same* lopsided gap persisted. A diagnostic log of the actual
   queried values showed why: `getSystemWindowInsetLeft()` itself returned
   the cutout's width, not a bars-only figure. That legacy API was never
   purely bar-driven to begin with; on this platform version it already
   folds the cutout in as part of "system window decoration." Its
   `getInsets(WindowInsets.Type.systemBars())` replacement (API 30+) is
   the one actually scoped to bars alone, so that's the primary path now;
   the legacy methods remain only as the `<30` fallback and still carry
   some of the original over-conservative-corner problem there.

A future top-anchored element that actually needs cutout-awareness should
query `DisplayCutout`'s own bounding rects directly rather than reach for
either of these blanket scalars.

A null `getRootWindowInsets()` (window not yet attached) or an unsupported
platform reports `SafeAreaInsets::default()` (all zero) — callers should
treat zero as "no better information available," not "this device has no
reserved space." Every non-Android platform's `capture_frame()` reports
zero unconditionally, the same "not every platform implements this yet"
convention `HostFrame.foreground` already established.

## Immersive mode

```rust
loadngo_host_desktop::set_immersive_mode(true);
```

Requests hidden status and navigation bars, swipe-to-reveal. On
`Build.VERSION.SDK_INT >= 30`, this goes through `WindowInsetsController`
(`View.getWindowInsetsController()` → `hide(WindowInsets.Type.systemBars())`
+ `setSystemBarsBehavior(BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE)`) — the
framework's own replacement, and the only reliable way to hide the
navigation bar at this crate's `targetSdkVersion` 34. Below API 30 it falls
back to the legacy `View.setSystemUiVisibility` sticky-immersive flag
combination (`LAYOUT_STABLE | LAYOUT_HIDE_NAVIGATION | LAYOUT_FULLSCREEN |
HIDE_NAVIGATION | FULLSCREEN | IMMERSIVE_STICKY`), which is deprecated but
still the only option below 30.

This two-path split was not the original design — the first version used
only the legacy flags on the theory that one code path was simpler and
still valid across the whole `minSdkVersion` 26 → `targetSdkVersion` 34
range. On-device verification (Android 14 / API 34) disproved that: the
status bar hid correctly, but the navigation bar stayed visible — apps
targeting API 30+ get that specific legacy flag silently ignored by the
framework. `WindowInsetsController` is the only method that actually works
on a real API 30+ device.

The request is a **level**, not an **event**: `loadngo` remembers whether
immersive mode was requested and reapplies the flags itself on `onResume`
and `onWindowFocusChanged(true)` — Android clears sticky-immersive flags
whenever the window regains focus (returning from the recents screen,
unlocking), and a caller that only set the flags once at startup would see
system bars silently reappear. Call `set_immersive_mode(true)` once; do not
call it every frame or on every resume.

## Gesture-exclusion rects

```rust
loadngo_host_desktop::set_gesture_exclusion_rects(&[left_stick_rect, right_stick_rect]);
```

On gesture-navigation devices (Android 10+), swiping in from the left or
right screen edge triggers system "back." `View.
setSystemGestureExclusionRects(List<Rect>)` (`Build.VERSION.SDK_INT >= 29`,
no-op below that) tells the platform to prioritize the app's own touch
handling over that gesture within the given rects — the natural fit for
persistent on-screen thumbstick zones that would otherwise sit right in the
back-swipe area. Pass screen-space (device pixel) rects in the same
coordinate space as `HostFrame.surface`; call again only when the rects
actually change, since building the backing `Rect`/`ArrayList` objects on
every frame is unnecessary JNI/GC churn for geometry that's typically
constant for a session.

## Display cutout mode

Letting the app draw into the cutout area rather than being letterboxed
around it by the OS is a manifest/theme concern, not a runtime one: see
`sng-roguelite`'s `scripts/android_packager.sh`, which generates a
`res/values/styles.xml` theme setting
`android:windowLayoutInDisplayCutoutMode` and applies it via
`android:theme` on the activity. This is independent of `HostFrame.insets`
(above), which deliberately doesn't track the cutout at all — a game that
places content where a cutout actually is needs to query
`DisplayCutout`'s own bounding rects itself, not rely on the generic
system-bar insets.

## Backend status

- Insets, immersive mode, and gesture exclusion are Android-only; there is
  no stub on other platforms (callers `#[cfg(target_os = "android")]`-gate
  the two `set_*` calls themselves, following the same convention already
  used for `android_log_error`). `HostFrame.insets` is the one field every
  platform must supply — non-Android platforms supply
  `SafeAreaInsets::default()`.
- Foldable/multi-window re-query on `onConfigurationChanged` is not
  implemented — insets are queried once per resume/focus-gain, which is
  correct for a fixed-size, non-resizable target. Add it if a resizable
  form factor is ever in scope.
