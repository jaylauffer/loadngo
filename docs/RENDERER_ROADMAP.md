# loadngo Renderer Roadmap

Date: 2026-03-15

## Goal

`loadngo` owns rendering as a first-class subsystem.
Platform backends provide windowing, GPU device/surface integration, and presentation, while the
renderer itself owns:

- frame encoding
- text and image draw planning
- multilingual text contracts
- batching and cache policy
- GPU abstraction boundaries

## Target layering

1. `ui-core`
- widget behavior and `PaintOp`

2. `host-core`
- platform-neutral input/window/frame contracts

3. `renderer` (`loadngo-renderer`)
- frame command encoding from `PaintOp` and `RenderOp`
- renderer-owned text requests with language/script/direction metadata
- renderer-owned backend interface

4. Future GPU backends
- `loadngo-gfx-metal`
- `loadngo-gfx-d3d11` or `loadngo-gfx-d3d12`
- `loadngo-gfx-gl` and/or `loadngo-gfx-vulkan`

5. Future platform hosts
- macOS
- iOS
- Android
- Linux
- Windows

## macOS-first plan

1. Build a Metal backend first.
2. Keep `loadngo-renderer` responsible for command encoding and resource ownership rules.
3. Limit the Metal backend to:
- swapchain/surface
- texture and buffer creation
- shader pipeline setup
- command submission
- frame presentation

## Multilingual support requirements

The renderer must treat multilingual text as a core feature rather than a later add-on.

Required capabilities:

- explicit text direction support (`Auto`, LTR, RTL)
- script metadata per request
- language tags per request
- font fallback chains
- shaping for scripts that need contextual shaping or ligatures
- measurement that matches shaped output

Planned renderer text pipeline:

1. text request
2. script/language/direction resolution
3. shaping
4. glyph atlas population
5. quad generation
6. backend draw submission

## Current gap summary

- `host-desktop` still directly executes rendering
- text shaping and fallback are not owned by `loadngo`
- image blit commands are not fully enforced by a renderer-owned execution layer
- there is no GPU backend under `loadngo` yet

## Acceptance milestones

### Milestone 1
- `loadngo-renderer` owns command encoding
- backend crates consume renderer commands instead of ad hoc render conversion

### Milestone 2
- macOS Metal backend renders rectangles, lines, images, and text through renderer-owned commands

### Milestone 3
- same renderer core reused by iOS, Android, Linux, and Windows backends

### Milestone 4
- Macroquad removed from `loadngo` and `sng-rusty`
