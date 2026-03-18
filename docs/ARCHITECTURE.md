# loadngo Architecture

## Goals
- Keep GUI components platform agnostic.
- Isolate platform-specific APIs behind small host shims.
- Keep runtime/editor logic independent of any single windowing/render library.

## Layer model
1. `ui-core`
- Owns widget models and behavior (`ButtonModel`, `ListCombo`, `TreeControl`, tabs, list state, pointer/key event types).
- Produces paint operations (`PaintOp`) and widget actions.
- No platform API dependencies.

2. `host-core` (`loadngo-host-core`)
- Owns cross-platform host contracts:
  - Window metadata (`WindowDescriptor`)
  - Per-frame timing/surface/input (`HostFrame`, `InputSnapshot`, `TouchPoint`)
  - Render protocol (`RenderOp`, `RenderTextStyle`)
    - text alignment (`Left`/`Center`/`Right`, `Top`/`Middle`/`Bottom`)
    - text layout mode (`SingleLine`, `MultiLine`)
    - single-line overflow policy (`Clip`, `EllipsisEnd`, `EllipsisMiddle`)
  - Texture/image seams (`DecodedImage`, `ImageRegistry`)
  - Split host seams:
    - `DesktopPlatformBackend`
    - `AssetIoBackend`
    - `DesktopGraphicsBackend`
  - Compatibility trait (`DesktopHostBackend`)
- Intended as the stable boundary between app logic and platform implementation.

3. `renderer` (`loadngo-renderer`)
- Owns renderer-facing frame command encoding and execution boundaries.
- Owns text/image rendering contracts independent of a specific GPU API.
- Carries multilingual text metadata (direction, script, language) so shaping/fallback can stay in `loadngo`.
- Owns deterministic text contract mapping from widget/app paint ops to backend frame commands.

4. Host implementations (`gui-win32`, `host-mac`, future backends)
- Translate native platform window/input/render to `host-core` and `ui-core` data.
- Handle concrete APIs for:
  - Window bootstrap / event loop
  - Input polling or event translation
  - GPU surface / device access
  - Presentation
  - Platform integration

5. App/runtime layers (`task` and external consumers like `sng-rusty`)
- Build higher-level domain UI and state transitions.
- Consume host/input/render abstractions; avoid direct platform API usage.

## Ownership boundaries
- Components (`Button`, `ListCombo`, `TreeCombo`, etc.) belong in `ui-core`/`gui`, not in host backends.
- Host backends should only adapt platform APIs and execute render/input contracts.
- App logic should emit actions/state updates and request rendering through host abstractions.

## Input model
- `InputSnapshot` carries mouse + touch + key state per frame.
- Pointer helpers in `host-core` (`pointer_in_rect`, `pointer_pressed_in_rect`, `pointer_released`) provide shared hit-testing semantics across platforms.
- Backends map native input to `InputSnapshot`; app/UI code consumes the normalized form.

## Render model
- UI/app code emits geometry/text/image operations.
- `loadngo-renderer` converts those operations into renderer-owned frame commands.
- GPU backends execute renderer commands using platform graphics APIs.
- Texture and glyph cache policy should live under the renderer, not ad hoc host adapters.
- Platform hosting and graphics execution should be replaceable independently.

## Recommended migration sequence
1. Define missing abstractions in `host-core` first.
2. Convert app/editor/runtime code to consume those abstractions.
3. Keep existing backend as adapter while behavior is validated by tests.
4. Swap backend implementation without changing app/UI logic.
