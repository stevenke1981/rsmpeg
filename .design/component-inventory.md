# Gate C - Component Inventory

## Stack and ownership

The replacement UI is AcmeUI Native:

- `WidgetNode<PlayerMessage>` and fluent builders for the state-derived view
- `acme-ui` high-level components
- `acme-widgets` primitives
- `acme-layout`/Taffy layout
- `acme-text`/cosmic-text
- `acme-theme` semantic tokens
- `acme-platform`/winit events and accessibility bridge
- `acme-render-wgpu` rendering

The player application must not retain egui/eframe or expose wgpu/winit types in public app/component APIs. `rsmpeg-player` remains the sole playback command/event layer.

Classification:

- **Direct:** existing AcmeUI component/primitives can be used as-is.
- **Composed:** rsmpeg-specific `WidgetNode<PlayerMessage>` composition using existing primitives.
- **Framework missing:** AcmeUI lacks behavior/render capability required by any real media app; implement and test it at the correct Acme framework layer.
- **App missing:** player/app state or product wiring is not available yet.

## Application shell and responsive layout

**Direct:** `ubuntu25_template`, `row`, `column`, `stack`, `separator`, `scroll_area`/`scroll_view`, `split_panel`, `drawer`, `LayoutStyle`, `Length`, `Overflow`.

**Composed:** `PlayerWorkspace`, a stable-key WidgetNode tree with command, preview, optional inspector, and timeline tracks. Viewport decisions derive from state/context and Taffy constraints.

**Framework missing:** confirm or add a reusable responsive/overflow decision surface that can move low-priority command nodes without manual child-index styling.

| State | Contract |
|---|---|
| Default | Stable preview and anchored transport; inspector closed |
| Hover | Only interactive child changes tone/border |
| Pressed | No geometry shift |
| Focus | `theme.colors.ring` follows logical keyboard order |
| Disabled | Layout remains stable; unavailable actions visibly inactive |
| Loading | Only valid recovery/cancel actions remain interactive |
| Error | Open/Retry stays reachable; preview shell does not collapse |

## Command region

**Direct:** `button`, `icon_button`, `tooltip`, `dropdown_menu`, `popover`, `separator`, `label`.

The current stock `command_bar` always creates a search input and wraps content in an outlined Card, so it is not a direct fit for this player.

**Composed:**

- `FileIdentity`: bounded filename, semantic muted path/detail, full-value tooltip.
- `OpenReplaceAction`: single message/effect path shared by picker and winit drop.
- `PlayerOverflowMenu`: only wired actions for speed, scale, inspector, diagnostics, and future controls.

**Framework missing:** add any missing media `IconName` values and accessible-name/tooltip support to `icon_button`; do not ship app-local emoji.

**App missing:** observable asynchronous shutdown/replacement result.

| State | Contract |
|---|---|
| Default | File identity readable; Open/Replace available |
| Hover | Semantic hover plus tooltip |
| Pressed | Immediate response; duplicate dialogs/actions cannot stack |
| Focus | File/Open, inspector, fullscreen, overflow order |
| Disabled | Worker-dependent actions disabled during opening/shutdown |
| Loading | Identity persists with compact `spinner` if appropriate |
| Error | Open/Replace remains enabled; details stay bounded |

## Empty/open/drop region

**Direct:** `empty_state`, `button`, `label`, `icon`, `alert`, `stack`.

**Composed:** `MediaDropTarget`, driven by winit `HoveredFile`, `HoveredFileCancelled`, and `DroppedFile` events and sharing validation/load effects with Open.

**Framework missing:** none for visual composition; verify Acme platform runtime exposes the required winit file events without leaking platform types through the component API.

**App missing:** file-picker integration and a single canonical supported-media filter/error path.

| State | Contract |
|---|---|
| Default | rsmpeg identity and one primary Open action |
| Hover | `Tone::Info` target with release instruction, no layout shift |
| Pressed | Standard button pressed state |
| Focus | Open supports keyboard activation and visible ring |
| Disabled | Only disabled while re-entry is unsafe |
| Loading | Transitions into stable preview shell |
| Error | Rejected/open failure uses `alert` with concrete Retry/Open |

## Dynamic video image primitive

**Direct:** `ImageFit` expresses `Contain`, `Cover`, `Fill`, and `None`; `stack`, layout nodes, clipping, and semantic overlays are reusable.

**Important:** current `desktop::image_view` is only a placeholder Card containing label/description. It does not carry pixels, emit an image draw command, or create/update a wgpu media texture. It cannot display video.

**Framework missing (required):**

1. A framework-owned image source/handle type for validated RGBA8 data and stable identity without public wgpu types.
2. A `WidgetNode` image/image-view representation with width/height, accessible description, `ImageFit`, clipping, disabled/loading/error semantics, and stable key.
3. An `acme-core` scene draw command for non-glyph images.
4. `acme-render-wgpu` texture allocation, bind group/sampler ownership, upload/update, batching, clipping, and surface-recovery support.
5. Same-size `queue.write_texture` reuse; deterministic recreate on size/format change; stale-generation rejection; bounded texture/resource lifetime.
6. Validation/tests for zero/overflow dimensions, `width * height * 4` byte contract, aspect-fit math, RGBA colors, clipping, replacement, device/surface recovery, and nonblank output.

The existing glyph RGBA atlas is for text/color glyph uploads and is not a substitute for arbitrary full-frame video textures.

**Composed:**

- `VideoSurface`: dynamic image node using contain fit, centered no-upscale policy, and stable video key.
- `PreviewOverlay`: loading, audio-only, drag-hover, warning, error, ended, and GPU-recovery siblings inside a clipped `stack`.

**App missing:** explicit audio-only/video capability, latest-frame handoff/handle ownership, and scale/fullscreen preferences.

| State | Contract |
|---|---|
| Default | Latest valid frame, correct aspect and colors, semantic shell with black letterbox |
| Hover | No decoration unless an actual preview action exists |
| Pressed | Reserved; no accidental playback toggle |
| Focus | Excluded from tab order unless it gains an action |
| Disabled | Retain last valid frame when safe |
| Loading | Stable dimensions plus `spinner`; never infinite video loading for audio-only |
| Error | Structured image/upload/playback error with recovery; preserve last safe frame |

## Timeline and timecode

**Direct:** `slider` provides static min/max/value/step visualization; `row`, `column`, `label`, semantic focus primitives, and stable keys are reusable.

**Important:** current Acme slider is a fill/remainder row with optional click message. It lacks pointer-to-value mapping, drag capture, drag start/update/end, hover time, keyboard value changes, and a value-carrying callback suitable for scrubbing.

**Framework missing (required):**

- Native range-input interaction with pointer capture and normalized/local coordinates.
- Messages/events carrying current value for hover/drag/commit.
- Keyboard Arrow/Page/Home/End semantics, focus, disabled state, and accessibility value/range.
- Deterministic hover/fill/thumb rendering from semantic tokens without layout movement.
- Cancellation behavior when pointer capture/window focus is lost.

**Composed:**

- `SeekTimeline`: maps values to duration, owns local scrub state, emits preview/commit messages, and never invents a buffered range.
- `Timecode`: finite-safe `MM:SS`/`HH:MM:SS` with stable layout width.

**App missing:** UI regression harness for stale events, 75 ms throttling, pointer cancellation, and exact one-time final commit.

| State | Contract |
|---|---|
| Default | Current position/total with clear thumb/fill |
| Hover | Target time visible without changing committed position |
| Pressed | Captured drag; local target owns timecode |
| Focus | Ring plus documented keyboard increments |
| Disabled | Unknown/zero duration prevents seek truthfully |
| Loading | Disabled until duration/seek capability is known |
| Error | No command is sent to disconnected/shut-down worker |

Scrubbing invariant: preview dispatch is limited to roughly one every 75 ms, player generation filtering rejects stale media state, and release sends the exact final target even if the last move did not change the value.

## Transport and volume

**Direct:** `button`, `icon_button`, `button_group`, `slider`, `tooltip`, `dropdown_menu`, `ControlSize`.

**Composed:**

- `PlayPauseButton`: one fixed control slot with state-derived icon/action.
- `StopButton`: non-blocking stop/shutdown message and repeated-action guard.
- `VolumeControl`: value-carrying native slider plus mute icon/action.
- `TransportBar`: responsive ordering and overflow.

**Framework missing:** volume needs the same value-carrying slider input capability; add missing media icons/accessibility behavior once at framework level.

**App missing:** mute/restore volume; GUI playback-rate wiring; fullscreen/frame-step commands; consistent command error mapping for queue-full, disconnected, and shut down.

| State | Contract |
|---|---|
| Default | Play/Pause primary; Stop/volume/Open secondary |
| Hover | Tooltip/tone, fixed width |
| Pressed | Immediate visual feedback, later reconciled to player event |
| Focus | Predictable left-to-right order and semantic ring |
| Disabled | Invalid actions disabled rather than ignored |
| Loading | Only recovery/cancel actions active |
| Error | Command failure maps to actionable notice; Open remains available |

## Inspector and diagnostics

**Direct:** `drawer`, `descriptions`, `description_item`, `scroll_area`, `label`, `separator`, `copy_button`.

**Composed:** `MediaInspector`, closed by default and grouped into File, Video, Audio, and Playback. It is omitted when no real values exist.

**Framework missing:** validate drawer focus trapping/return, dismissal, overlay hit-testing, responsive width, and accessibility before relying on it as a production inspector.

**App missing:** snapshot/event fields for codec, resolution/FPS, audio format, tracks, A/V drift, dropped frames, and capability flags.

| State | Contract |
|---|---|
| Default | Closed; toggle shown only when useful |
| Hover | Subtle row/control feedback |
| Pressed | Toggle/disclosure without unexpected preview resize |
| Focus | Drawer entry, traversal, dismissal, and focus return |
| Disabled | Unsupported control explains its state |
| Loading | Concise pending text, no fake skeleton metrics |
| Error | Field-level unavailable reason; playback error stays in notice/preview |

## Status, warning, error, and loading

**Direct:** `alert`, `toast`, `indicator`, `spinner`, `progress`, `label`, `copy_button`, `Tone`.

**Composed:**

- `StatusLine`: short persistent state adjacent to controls.
- `PlaybackNotice`: severity, detail, recovery action, and deduplication policy.
- `PreviewStatus`: blocking state rendered over preview without changing layout.

**Framework missing:** verify toast lifetime/queue, keyboard access, focus behavior, and screen-reader announcement. Use persistent `alert` where transient behavior is not proven.

**App missing:** structured error code/severity/recovery data and bounded diagnostic history.

| State | Contract |
|---|---|
| Default | Quiet status or no notice |
| Hover | Tooltip reveals truncated detail |
| Pressed | Copy/Details/Retry responds immediately |
| Focus | Notice actions keyboard reachable |
| Disabled | Unavailable recovery explains why |
| Loading | Truthful indeterminate status; no fake percentage |
| Error | `Tone::Danger`, text/icon, detail, and concrete recovery |

## Theme and accessibility

**Direct:** `OneDarkPack`, `Theme`, `ThemeMode`, `theme.colors.*`, `resolve_tone`, `focus_ring`, `meets_wcag_aa()`, `wcag_report()`, Acme accessibility bridge.

**Composed:** app theme state selects tested light/dark variants and passes semantic theme through layout/rendering.

**Framework missing:** none assumed for token lookup; manual evidence is still required for Windows keyboard focus, screen reader names, DPI, and Traditional Chinese IME before support is claimed.

**App missing:** theme preference persistence only if product requirements request it.

## Runtime and dependency integration

**Direct:** `acme-platform`, winit 0.30, `acme-render-wgpu`/wgpu 29, layout/text/theme crates, Acme renderer surface actions.

**Composed:** rsmpeg runtime adapter that:

- converts winit input/window events into `PlayerMessage`
- polls player events without blocking
- schedules redraw while playing/scrubbing/loading only
- renders the WidgetNode-derived scene
- handles resize/DPI/occlusion/surface lost/outdated/timeout
- starts non-blocking player shutdown on replace/close

**App missing/risks:**

- Path dependencies rooted at `E:\AcmeUI-Native` are non-portable; pinning and release/CI strategy are required.
- AcmeUI requires Rust 1.85 while rsmpeg advertises 1.75; workspace MSRV must be raised or GUI isolated behind an explicit compiler/feature boundary.
- The Acme feature set must include `foundations`, `inputs`, `layout`, `overlay`, `desktop`, and `browser`.
- Removing eframe/egui may change transitive window/GPU dependencies and release size; validate clean builds and packaging.

## Component creation decision

Required framework work before an AcmeUI player can be called functional:

1. Real dynamic image primitive from `WidgetNode`/scene through wgpu texture rendering.
2. Value-carrying pointer/keyboard slider/range input for seek and volume.
3. Any missing media icons plus tooltip/accessibility-name support.
4. Production validation/fixes for drawer, toast, file-drop routing, focus, and surface recovery.

Required app work:

1. Acme `PlayerAppState` / `PlayerMessage` / pure view / update/runtime separation.
2. Latest-frame ownership and GPU upload scheduling.
3. Responsive command/transport overflow.
4. Truthful audio-only and structured media/diagnostic state.
5. Observable non-blocking shutdown/replacement.

Do not create decorative components or duplicate player logic while these foundational contracts are absent.

