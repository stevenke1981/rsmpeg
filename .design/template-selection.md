# Gate A - Template Selection

## Routing declaration

- Mode: `redesign` + `template-first`
- Target stack: `acmeui-native`
- Template: `media-studio`
- Visual recipe: `studio-neutral`
- Shell: `ubuntu25_template`
- Framework source: `E:\AcmeUI-Native`
- Required AcmeUI features: `foundations`, `inputs`, `layout`, `overlay`, `desktop`, `browser`
- Validation: rsmpeg Cargo gates, AcmeUI component/theme tests, and real winit/wgpu visual QA at 960x600, 1280x720, and 480x320

## User goal

Replace the rsmpeg GUI's egui/eframe presentation with AcmeUI Native while preserving the working `rsmpeg-player` command/event boundary. The application must become a focused native media workspace built from `WidgetNode<M>`, AcmeUI components and primitives, Taffy layout, cosmic-text, winit event handling, and wgpu rendering.

Opening a file, playback, pause, stop, volume, replacement, timeline scrubbing, warnings, and errors must remain understandable and non-blocking. Demux, decode, scale, resample, seek, and worker shutdown must not run on the winit UI/event thread.

The immediate product remains a single-file player, not a nonlinear editor. The `media-studio` template supplies preview, command, status, inspector, and timeline responsibilities; it does not justify fake projects, assets, clips, tracks, or export workflows.

## Selected template

**Template ID:** `media-studio`

**Why it matches:**

- The decoded media preview is the dominant object.
- Transport and time navigation are the highest-frequency actions.
- Timeline drag needs continuous pointer input, a locally owned scrub target, throttled preview seeks, and an exact final commit.
- Playback state and backend failures need stable presentation near the preview and controls.
- Planned capabilities such as speed, codec/media information, track selection, fullscreen, aspect policy, A/V drift, and dropped-frame diagnostics fit an optional inspector.

The starter in `templates/media-studio.rs` is an information-architecture reference. Its placeholder cards and export/project messages must be replaced with real rsmpeg state and `PlayerMessage` wiring.

## Region mapping

| Template region | Decision | AcmeUI Native responsibility |
|---|---|---|
| `command-bar` | Adapt and keep | Compact `row`/button/icon-button command region for file identity, Open/Replace, inspector/fullscreen toggles, and overflow. Do not use the stock searchable `command_bar` unless its mandatory search field is made optional. |
| `asset-rail` | Remove | rsmpeg has no project bin, playlist, reorder, or persistence model. Do not render an ornamental empty rail. |
| `preview` | Keep as primary | A framework-owned dynamic RGBA image primitive rendered by wgpu with contain/aspect-fit behavior, stable texture identity, and overlays for loading, audio-only, drag-hover, warning, error, and ended states. |
| `inspector` | Adapt, closed by default | `drawer` or a responsive side region containing real `descriptions` data for file, codec, video, audio, playback, and diagnostics. |
| `timeline` | Keep and specialize | A native pointer/keyboard seek control plus timecode, transport, volume, and later speed/frame-step actions. Preserve the 75 ms preview throttle and exact pointer-up commit. |
| `status` | Add as a cross-region contract | `alert`, `toast`, `indicator`, or inline semantic labels selected by severity and persistence. Error/warning meaning must not depend on color alone. |

## State, message, and view contract

State mutation belongs in the update/event layer; the view is a pure projection into `WidgetNode<PlayerMessage>`.

```text
PlayerAppState
  window/theme/layout state
  file and player ownership
  latest video-frame handle/metadata
  position/duration/volume/playback state
  scrub target + drag state + preview throttle timestamp
  warning/error/status

PlayerMessage
  OpenRequested / FileDropped(path)
  Play / Pause / Stop / Shutdown
  SeekDragStarted / SeekPreview(value) / SeekCommitted(value)
  VolumeChanged(value)
  ToggleInspector / ToggleFullscreen / SetTheme(mode)
  PlayerEventReceived(event)

view(&PlayerAppState) -> WidgetNode<PlayerMessage>
update(&mut PlayerAppState, PlayerMessage) -> effects/commands
runtime: winit events + player polling + wgpu render scheduling
```

Dynamic controls and lists require stable keys. Public application/component APIs must not expose wgpu/winit platform types; those remain inside Acme platform/render/runtime integration.

## Required states

| Product state | Entry/evidence | Required presentation and behavior |
|---|---|---|
| `no-media` | No input/player | Open is primary; winit file drop remains available; invalid transport/timeline actions are disabled or absent. |
| `file-drag-hover` | winit hover/drop event | Stable preview/empty geometry with semantic `info` target; no layout shift. |
| `opening` | Input accepted; no usable frame yet | Stable preview with `spinner`/truthful indeterminate progress; only valid recovery actions remain enabled. |
| `ready-paused` | Player ready and not playing | Current image remains; Play is primary; seek preview updates the image and stays paused. |
| `playing` | Snapshot/player state reports playing | Pause replaces Play in the same fixed control slot; frame/position updates schedule redraw without rebuilding unrelated GPU resources. |
| `scrubbing` | Native seek control drag active | Local target owns thumb/timecode; stale events cannot overwrite it; preview seeks are throttled to about 75 ms; release commits once at the exact target. |
| `audio-only` | Player explicitly reports audio with no video stream | Truthful audio identity replaces video loading; timeline and transport remain usable. |
| `warning` | `PlayerEvent::Warning` | Non-blocking semantic warning with readable details; playback may continue. |
| `error` | Open/build failure or `PlayerEvent::Error` | Concrete reason plus recovery; invalid ownership is cleared; never leave a perpetual loading state. |
| `ended` | `PlayerEvent::Ended` | Keep the final frame when valid, stop playback-rate redraw, and offer Replay/Open. |
| `shutdown` | Stop, replace, close | UI remains non-blocking; repeated commands are disabled; worker completion is observable. |
| `surface-recovery` | wgpu surface lost/outdated/timeout/occluded | Follow Acme renderer recovery semantics, preserve app/player state, and avoid a false playback error when only the surface is unavailable. |

## Dependency and migration boundary

- Remove egui/eframe UI ownership and map the app to AcmeUI Native runtime, layout, widgets, theme, accessibility, and renderer crates.
- The initial integration is expected to use path dependencies rooted at `E:\AcmeUI-Native`; this is machine-local and must have an explicit packaging/CI replacement before a portable release.
- rsmpeg currently declares MSRV 1.75; AcmeUI Native declares Rust 1.85 and edition 2024. The migration therefore requires an explicit workspace MSRV decision and validation with Rust 1.85 or newer.
- A file picker may remain a platform integration if needed, but it must not retain egui/eframe. Winit `HoveredFile`, `HoveredFileCancelled`, and `DroppedFile` events own drag/drop routing.
- Preserve `rsmpeg-player` as the sole playback command/event API. UI migration must not fork playback logic.

## Technical risks

1. **Missing real image primitive:** AcmeUI's current `image_view` is a placeholder container with alt text; it does not render supplied RGBA pixels. Playback requires a framework-level image draw primitive, texture registry/lifetime, wgpu upload/update path, clipping, and `ImageFit::Contain`.
2. **Texture performance:** recreating a texture or cloning a full RGBA frame every update will be expensive. Same-size frames must reuse texture allocation; resolution changes and stale generations require deterministic replacement.
3. **Seek input semantics:** AcmeUI's current slider builds a fill/remainder row and wires an `on_click`; it does not yet provide the continuous pointer drag/start/end/value contract required by video scrubbing.
4. **Path dependency portability:** `E:\AcmeUI-Native` works only on this machine/layout. CI, collaborators, packaging, and publishing need a workspace/vendor/git dependency strategy and version pin.
5. **MSRV mismatch:** rsmpeg is Rust 1.75/edition 2021 while AcmeUI is Rust 1.85/edition 2024. Dependency resolution and CI must enforce the chosen 1.85 floor without claiming older compiler support.
6. **Runtime integration:** player polling, winit events, redraw scheduling, file drop, DPI changes, resize, close, and wgpu surface recovery must coexist without blocking the event loop.
7. **Accessibility/input:** pointer capture, keyboard seek, focus traversal, tooltips, icon accessible names, and Windows IME cannot be claimed until manually verified.
8. **Truthful diagnostics:** inspector metrics not present in player snapshots/events cannot be inferred or filled with placeholders.

