# Gesture Programming & Live Streaming

## Overview
The goal is to transform the `loadngo` IDE into a live-streaming environment where the developer can interact with the code using a **Gesture Programming Language (GPL)**. This involves integrating the camera feed directly into the IDE and layering a gesture-recognition system on top of it.

## Architecture

### 1. Camera Integration
The IDE will utilize the `loadngo-camera` crate to maintain a live `CaptureStream`. 
- **Integration:** The camera feed will be integrated into the main IDE window as a dedicated panel.
- **Visual Feedback:** The feed will use the `camera/live` image key to ensure the renderer updates the texture every frame.
- **Streaming Output:** To support live streaming, the IDE will provide a way to route this feed (including overlays) to a virtual camera or a streaming service.

### 2. Gesture Programming Language (GPL)
GPL is a non-textual input method where physical gestures are mapped to IDE commands.

#### Recognition Pipeline
1. **Frame Capture:** Raw frames are pulled from `CaptureStream`.
2. **Landmark Detection:** A computer vision layer (e.g., MediaPipe or OpenCV) identifies hand landmarks and skeletal structure.
3. **Gesture Classification:** A set of predefined patterns (e.g., "Pinch", "Swipe", "Circle") are recognized from the landmark stream.
4. **Command Mapping:** Recognized gestures are dispatched as IDE commands.

#### Example Mappings (Proposed)
| Gesture | IDE Action | Description |
| :--- | :--- | :--- |
| **Pinch** | `Select_Symbol` | Selects the symbol under the cursor or a specific UI element. |
| **Swipe Left/Right** | `Navigate_History` | Moves back and forth through the undo/redo or navigation stack. |
| **Circle** | `Refactor_Extract` | Triggers a refactoring action on the selected block. |
| **Open Palm** | `Pause_Stream` | Pauses the live stream or mutes the feed. |

### 3. The Streaming Experience
To make the programming "performative" and clear to the audience:
- **Gesture Overlays:** The IDE will render visual markers (bounding boxes, gesture labels) directly over the camera feed.
- **Action Log:** A visible log of "Gesture $\rightarrow$ Action" will be displayed on the stream so viewers can follow the logic of the gesture-based edits.

## Implementation Roadmap

- [ ] **Phase 1: IDE Integration**
    - Move `camera_preview` logic into the main IDE GUI.
    - Implement the `camera/live` texture update loop in the main window.
- [ ] **Phase 2: Vision Layer**
    - Integrate a landmark detection library.
    - Implement basic gesture recognition (Pinch, Swipe).
- [ ] **Phase 3: Command Mapping**
    - Create a configuration system for mapping gestures to `loadngo` commands.
- [ ] **Phase 4: Streaming HUD**
    - Implement the visual overlay system for the camera feed.
    - Implement virtual camera output for OBS/streaming.
