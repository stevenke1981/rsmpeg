# Gate B - Art Direction

## Selection

- Recipe: `studio-neutral`
- Palette: `studio-neutral`
- Theme pack: `acme_theme::packs::OneDarkPack`
- Shell: `ubuntu25_template`
- Runtime theme model: `acme_theme::Theme` and semantic tokens only
- Density: high in controls/diagnostics, low around media
- Container model: rails-canvas-inspector-timeline, reduced to command bar + open preview + optional inspector + timeline for the current single-file product

`OneDarkPack` is the existing theme-pack implementation selected by the palette library. It is not permission to make the whole UI a single blue-gray/purple field. Black is reserved for video letterboxing; semantic surfaces, borders, text, focus, and status tones must remain perceptibly distinct.

## Primary visual direction

The application should feel like a focused native media player: quiet chrome, precise time feedback, direct manipulation, and an honest large preview. Media content provides the visual richness while AcmeUI chrome recedes.

Build the root from `ubuntu25_template` and replace starter placeholders with state-derived `WidgetNode<PlayerMessage>` regions. Do not reproduce the starter's card-per-region composition: use `row`, `column`, `stack`, `split_panel`/responsive layout, separators, and only genuine overlays/drawers.

## Focus hierarchy

1. **Primary focus:** wgpu-rendered decoded media or truthful audio-only/loading/error state.
2. **Secondary focus:** seek timeline and current/total timecode.
3. **Primary action by state:** Open in no-media, Pause while playing, Play while paused/ended, Retry/Open after an error.
4. **Supporting actions:** Stop, volume, Replace/Open, speed, fullscreen, and inspector, collapsing into overflow when width is constrained.

File identity and status establish context but do not outrank preview or timeline.

## Information density

- Preview stays sparse and receives only meaningful overlays.
- Timeline/transport use `ControlSize::Sm` or `Md` with fixed geometry.
- Command bar uses one compact row at desktop sizes; long file identity truncates without displacing actions.
- Inspector uses compact `descriptions`, labels, rows, separators, and scroll behavior, not a card for each datum.
- At 480x320, preserve preview, Play/Pause, timeline, current time, volume access, and Open/Replace. Move lower-frequency actions/details into `dropdown_menu`, `popover`, or `drawer`.

## Typography

Use `theme.typography` tokens through Acme text rendering; do not scale fonts with viewport width.

| Role | Token direction |
|---|---|
| App/file identity | Heading only in empty state; compact strong body/label in command bar |
| Timecode | Stable-width numeric text, tabular figures when supported by the active cosmic-text font |
| Controls | Compact label or `icon_button` with tooltip/accessibility name |
| Inspector labels | Muted label/small token; value in readable body token |
| Status/error | Body token, concise/actionable; strong weight for blocking error title only |

Letter spacing is zero. Long paths/errors wrap or truncate within bounded layout nodes and never overlap controls.

## Container model

- Root uses a full-window template/layout tree with stable command, preview, and timeline tracks.
- Preview is an open unframed canvas region; it is not placed in a decorative Card.
- Timeline is a full-width anchored band below the preview.
- Inspector uses a true responsive side region or `drawer`, closed by default.
- `stack` owns preview overlays without changing preview dimensions.
- Metadata uses descriptions/rows and separators rather than nested cards.
- Radius is taken from `theme.radii`; use compact values for controls and avoid giant rounded panels.
- All responsive sizes are computed through Acme/Taffy layout constraints, not absolute viewport-specific font scaling.

## Semantic color contract

All component colors resolve from `theme.colors`; widget-local RGB/hex literals are prohibited.

| Acme semantic token | Use |
|---|---|
| `background` | Window shell outside video |
| `surface` | Command, transport, and inspector regions |
| `surface_alt`/secondary equivalent | Hover/selected rows and secondary controls |
| `foreground` | Primary text and enabled icons |
| `muted_foreground` | Secondary labels and inactive metadata |
| `border` | Region separators and inactive outlines |
| `primary` / `primary_foreground` | State-appropriate primary action and active timeline |
| `accent` / `accent_foreground` | Selected/hovered surface only |
| `ring` | Keyboard focus ring |
| `disabled_text` | Disabled labels, supplemented by non-color affordance |
| `success` / `success_soft` | Completed/healthy result |
| `warning` / `warning_soft` | Recoverable playback warning |
| `danger` / `danger_soft` | Blocking playback error |
| `info` / `info_soft` | File-drop target and informational notice |

Resolve the pack with `theme_by_name("one-dark", mode)` or the `ThemePack` methods `OneDarkPack.light()` / `OneDarkPack.dark()`. Both variants must pass `meets_wcag_aa()`; record `wcag_report()` output. If the studio-neutral palette becomes a new pack rather than using One Dark directly, both light/dark variants require the same tests.

The actual media pixels are not theme colors. The preview image primitive accepts decoded RGBA data, while letterbox and overlay chrome use semantic tokens (with black letterbox as the media presentation exception).

## Icons

- Prefer Acme `IconName` + `icon_button` for play, pause, stop, volume/mute, open, fullscreen, overflow, and inspector when the catalog contains an unambiguous icon.
- Add missing icons to the framework icon catalog once, not as app-local Unicode/emoji strings.
- Every icon-only button needs tooltip, accessible label, focus behavior, and a stable target of at least 32x32 logical pixels.
- Play/Pause and Volume/Mute share fixed boxes so dynamic state never shifts adjacent controls.
- Use concise text buttons until the actual icon is available; raw emoji are not production transport icons.

## Motion and interaction

- Motion communicates playback, playhead movement, loading, file-drop acceptance, inspector transition, or short status feedback only.
- Timeline pointer drag captures input, shows local target/timecode, emits bounded preview seeks, and commits exactly once at release/cancel resolution.
- Hover, pressed, focus, disabled, loading, and error states must be visually distinct without geometry changes.
- Do not animate preview size, transport tracks, or error regions.
- Redraw continuously only while playing, scrubbing, loading animation, or an active transition requires it. Paused, ended, occluded, and settled error states stop high-frequency redraw.
- Wgpu surface recovery is a runtime state, not decorative animation.
- Honor reduced motion where platform/runtime support exists.

## Image presentation

- The framework image primitive accepts a stable image/texture key, RGBA8 dimensions/data or a renderer-owned handle, accessible description, clipping, and fit mode.
- Default fit is contain; preserve source aspect ratio and center within the preview.
- Do not upscale above source resolution unless an explicit user preference enables it.
- Same-size video frames update the existing wgpu texture; size/format changes replace it predictably.
- Loading, audio-only, drag-hover, warning, error, and ended overlays are sibling nodes in a clipped `stack`, not baked into video pixels.
- Invalid dimensions, incorrect byte length, stale generation, or failed GPU upload produce a structured error and preserve the last valid frame where safe.

## Prohibited treatments

- Marketing hero composition, feature-tour copy, or split landing-page layout.
- Glassmorphism, blur panels, blue-purple gradients, neon glow, bokeh/orbs, or large purple fields.
- Giant rounded panels, floating page-section cards, nested cards, or a card for every metadata row.
- Fake waveform, buffered range, codec/FPS metric, export state, asset rail, project, or clip timeline.
- Oversized panel typography or viewport-scaled font size.
- App-local raw emoji for transport icons.
- Color-only warning/error/focus communication.
- Hardcoded widget colors or application exposure of wgpu/winit public types.

## Visual acceptance direction

Gate E requires real winit/wgpu screenshots and interaction evidence for:

- 960x600 default, 1280x720, and 480x320 minimum
- no-media, file-drag-hover, opening, playing, paused, scrubbing, warning/error, ended, audio-only, and surface-recovery where reproducible
- light and dark semantic themes plus `wcag_report()`
- keyboard focus on Open, Play/Pause, timeline, volume, overflow, and inspector
- long filename/error text, DPI scaling, resize, occlusion/minimize/restore
- nonblank decoded frame with correct colors, aspect ratio, clipping, and no overlay/control overlap

Build success alone is not visual completion. No visual score is valid before observed screenshots and interactions.
