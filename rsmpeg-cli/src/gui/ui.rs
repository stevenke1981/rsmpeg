//! AcmeUI Native presentation and app-level hit testing.

use acme_core::{DrawCommand, ImageKey, ImagePrimitive, Rgba8Image, Scene};
use acme_layout::LayoutSnapshot;
use acme_platform::{FrameContext, PlatformKey, WindowId};
use acme_render_wgpu::{scene_from_frame, Frame, Quad, TextRun};
use acme_text::{FontSystem, GlyphAtlas, TextConstraints, TextStyle};
use acme_theme::packs::one_dark::OneDarkPack;
use acme_theme::packs::ThemePack;
use acme_theme::{Theme, ThemeColor};
use acme_ui::{
    button, column, label, row, ButtonState, ButtonVariant, LayoutEngine, NodeId, WidgetNode,
};

use super::VideoPreview;

const CONTROL_HEIGHT: f32 = 38.0;
const TOOLBAR_HEIGHT: f32 = 54.0;
const TIMELINE_HEIGHT: f32 = 52.0;
const CONTENT_PADDING: f32 = 16.0;
const VIDEO_IMAGE_KEY: u64 = 0x7273_6d70_6567_0001;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum UiIntent {
    OpenFile,
    TogglePlayback,
    Stop,
    Seek { fraction: f32, final_commit: bool },
    Volume(f32),
}

#[derive(Clone, Debug)]
pub(crate) struct UiModel {
    pub(crate) has_media: bool,
    pub(crate) file_name: Option<String>,
    pub(crate) status: String,
    pub(crate) playing: bool,
    pub(crate) position_sec: f64,
    pub(crate) duration_sec: f64,
    pub(crate) volume: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Rect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

impl Rect {
    const ZERO: Self = Self {
        x: 0.0,
        y: 0.0,
        width: 0.0,
        height: 0.0,
    };

    fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width: width.max(0.0),
            height: height.max(0.0),
        }
    }

    fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.x + self.width && y >= self.y && y <= self.y + self.height
    }

    fn as_array(self) -> [f32; 4] {
        [self.x, self.y, self.width, self.height]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Control {
    Open,
    TogglePlayback,
    Stop,
    Timeline,
    Volume,
}

#[derive(Clone, Copy, Debug)]
struct UiLayout {
    preview: Rect,
    toolbar: Rect,
    open: Rect,
    toggle: Rect,
    stop: Rect,
    timeline: Rect,
    volume: Rect,
}

impl Default for UiLayout {
    fn default() -> Self {
        Self {
            preview: Rect::ZERO,
            toolbar: Rect::ZERO,
            open: Rect::ZERO,
            toggle: Rect::ZERO,
            stop: Rect::ZERO,
            timeline: Rect::ZERO,
            volume: Rect::ZERO,
        }
    }
}

impl UiLayout {
    fn for_window(width: f32, height: f32, has_player: bool) -> Self {
        if !has_player {
            let button_width = 180.0;
            return Self {
                open: Rect::new(
                    ((width - button_width) * 0.5).max(CONTENT_PADDING),
                    (height * 0.58).min(height - 112.0).max(108.0),
                    button_width,
                    CONTROL_HEIGHT,
                ),
                ..Self::default()
            };
        }

        let safe_width = width.max(320.0);
        let safe_height = height.max(220.0);
        let toolbar_y = safe_height - TOOLBAR_HEIGHT;
        let timeline_y = toolbar_y - TIMELINE_HEIGHT;
        let button_y = toolbar_y + (TOOLBAR_HEIGHT - CONTROL_HEIGHT) * 0.5;
        let compact = safe_width < 400.0;
        let toggle_width = if compact { 68.0 } else { 92.0 };
        let stop_width = if compact { 54.0 } else { 72.0 };
        let volume_width = if compact { 70.0 } else { 120.0 };
        let volume_x = if compact {
            CONTENT_PADDING + toggle_width + 70.0
        } else {
            (safe_width - 250.0).max(CONTENT_PADDING + 180.0)
        };
        let timeline = Rect::new(
            CONTENT_PADDING + 58.0,
            timeline_y + 16.0,
            (safe_width - CONTENT_PADDING * 2.0 - 116.0).max(80.0),
            18.0,
        );
        Self {
            preview: Rect::new(
                CONTENT_PADDING,
                CONTENT_PADDING,
                safe_width - CONTENT_PADDING * 2.0,
                (timeline_y - CONTENT_PADDING * 2.0).max(60.0),
            ),
            toolbar: Rect::new(0.0, toolbar_y, safe_width, TOOLBAR_HEIGHT),
            open: Rect::new(safe_width - 54.0, button_y, 38.0, CONTROL_HEIGHT),
            toggle: Rect::new(CONTENT_PADDING, button_y, toggle_width, CONTROL_HEIGHT),
            stop: Rect::new(
                CONTENT_PADDING + toggle_width + 8.0,
                button_y,
                stop_width,
                CONTROL_HEIGHT,
            ),
            timeline,
            volume: Rect::new(volume_x, button_y + 10.0, volume_width, 18.0),
        }
    }

    fn from_widget_tree(
        tree: &WidgetNode<LayoutMessage>,
        engine: &mut LayoutEngine,
        fonts: &mut FontSystem,
        scale_factor: f32,
        width: f32,
        height: f32,
        has_player: bool,
    ) -> Option<Self> {
        let root = tree.to_layout(NodeId::new(1));
        let snapshot = engine
            .compute_with_text(&root, (width, height), fonts, scale_factor)
            .ok()?;
        if !has_player {
            return Some(Self {
                open: layout_rect(&snapshot, root.children.get(3)?.id)?,
                ..Self::default()
            });
        }

        let preview = root.children.get(1)?;
        let timeline_row = root.children.get(2)?;
        let toolbar = root.children.get(3)?;
        let layout = Self {
            preview: layout_rect(&snapshot, preview.id)?,
            toolbar: layout_rect(&snapshot, toolbar.id)?,
            open: layout_rect(&snapshot, toolbar.children.get(5)?.id)?,
            toggle: layout_rect(&snapshot, toolbar.children.first()?.id)?,
            stop: layout_rect(&snapshot, toolbar.children.get(1)?.id)?,
            timeline: layout_rect(&snapshot, timeline_row.children.get(1)?.id)?,
            volume: layout_rect(&snapshot, toolbar.children.get(4)?.id)?,
        };
        let preview_bottom = layout.preview.y + layout.preview.height;
        let timeline_bottom = layout.timeline.y + layout.timeline.height;
        let toolbar_bottom = layout.toolbar.y + layout.toolbar.height;
        (preview_bottom <= layout.timeline.y
            && timeline_bottom <= layout.toolbar.y
            && toolbar_bottom <= height
            && layout.toolbar.width <= width)
            .then_some(layout)
    }

    fn hit(self, x: f32, y: f32) -> Option<Control> {
        [
            (Control::Open, self.open),
            (Control::TogglePlayback, self.toggle),
            (Control::Stop, self.stop),
            (Control::Timeline, self.timeline),
            (Control::Volume, self.volume),
        ]
        .into_iter()
        .find_map(|(control, rect)| rect.contains(x, y).then_some(control))
    }

    fn matches_stable_geometry(self, stable: Self) -> bool {
        const TOLERANCE: f32 = 8.0;
        (self.preview.y - stable.preview.y).abs() <= TOLERANCE
            && (self.preview.height - stable.preview.height).abs() <= TOLERANCE
            && (self.timeline.y - stable.timeline.y).abs() <= TOLERANCE
            && (self.toolbar.y - stable.toolbar.y).abs() <= TOLERANCE
    }
}

#[derive(Clone, Copy, Debug)]
enum LayoutMessage {
    Open,
    Toggle,
    Stop,
}

pub(crate) struct PlayerUi {
    cursor: (f32, f32),
    active: Option<Control>,
    focused: Option<Control>,
    layout: UiLayout,
    fonts: FontSystem,
    atlas: GlyphAtlas,
    layout_engine: LayoutEngine,
}

impl PlayerUi {
    pub(crate) fn new() -> Self {
        Self {
            cursor: (-1.0, -1.0),
            active: None,
            focused: Some(Control::Open),
            layout: UiLayout::default(),
            fonts: FontSystem::new(),
            atlas: GlyphAtlas::new(4096, 4096),
            layout_engine: LayoutEngine::new(),
        }
    }

    pub(crate) fn pointer_moved(&mut self, x: f32, y: f32) -> Vec<UiIntent> {
        self.cursor = (x, y);
        match self.active {
            Some(Control::Timeline) => vec![UiIntent::Seek {
                fraction: fraction_at(self.layout.timeline, x),
                final_commit: false,
            }],
            Some(Control::Volume) => {
                vec![UiIntent::Volume(fraction_at(self.layout.volume, x))]
            }
            _ => Vec::new(),
        }
    }

    pub(crate) fn pointer_button(&mut self, pressed: bool, x: f32, y: f32) -> Vec<UiIntent> {
        self.cursor = (x, y);
        if pressed {
            self.active = self.layout.hit(x, y);
            if self.active.is_some() {
                self.focused = self.active;
            }
            return match self.active {
                Some(Control::Timeline) => vec![UiIntent::Seek {
                    fraction: fraction_at(self.layout.timeline, x),
                    final_commit: false,
                }],
                Some(Control::Volume) => {
                    vec![UiIntent::Volume(fraction_at(self.layout.volume, x))]
                }
                _ => Vec::new(),
            };
        }

        let active = self.active.take();
        match active {
            Some(Control::Timeline) => vec![UiIntent::Seek {
                fraction: fraction_at(self.layout.timeline, x),
                final_commit: true,
            }],
            Some(Control::Volume) => vec![UiIntent::Volume(fraction_at(self.layout.volume, x))],
            Some(control) if self.layout.hit(x, y) == Some(control) => vec![match control {
                Control::Open => UiIntent::OpenFile,
                Control::TogglePlayback => UiIntent::TogglePlayback,
                Control::Stop => UiIntent::Stop,
                Control::Timeline | Control::Volume => unreachable!(),
            }],
            _ => Vec::new(),
        }
    }

    pub(crate) fn key_pressed(
        &mut self,
        key: PlatformKey,
        shift: bool,
        model: &UiModel,
    ) -> Vec<UiIntent> {
        let controls = if model.has_media {
            &[
                Control::TogglePlayback,
                Control::Stop,
                Control::Timeline,
                Control::Volume,
                Control::Open,
            ][..]
        } else {
            &[Control::Open][..]
        };
        if !controls.contains(&self.focused.unwrap_or(Control::Open)) {
            self.focused = controls.first().copied();
        }
        match key {
            PlatformKey::Tab => {
                let current = controls
                    .iter()
                    .position(|control| Some(*control) == self.focused)
                    .unwrap_or(0);
                let next = if shift {
                    (current + controls.len() - 1) % controls.len()
                } else {
                    (current + 1) % controls.len()
                };
                self.focused = Some(controls[next]);
                Vec::new()
            }
            PlatformKey::Enter | PlatformKey::Space => match self.focused {
                Some(Control::Open) => vec![UiIntent::OpenFile],
                Some(Control::TogglePlayback) => vec![UiIntent::TogglePlayback],
                Some(Control::Stop) => vec![UiIntent::Stop],
                _ => Vec::new(),
            },
            PlatformKey::ArrowLeft | PlatformKey::ArrowRight => {
                let direction = if key == PlatformKey::ArrowRight {
                    1.0
                } else {
                    -1.0
                };
                match self.focused {
                    Some(Control::Timeline) => {
                        let current = timeline_fraction(model.position_sec, model.duration_sec);
                        vec![UiIntent::Seek {
                            fraction: (current + direction * 0.01).clamp(0.0, 1.0),
                            final_commit: true,
                        }]
                    }
                    Some(Control::Volume) => {
                        vec![UiIntent::Volume(
                            (model.volume + direction * 0.05).clamp(0.0, 1.0),
                        )]
                    }
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    }

    pub(crate) fn on_gpu_recovered(&mut self, _window: WindowId) {
        self.atlas.clear();
    }

    pub(crate) fn frame(
        &mut self,
        context: FrameContext,
        model: &UiModel,
        preview: Option<&VideoPreview>,
    ) -> Scene {
        let fallback = UiLayout::for_window(
            context.logical_width,
            context.logical_height,
            model.has_media,
        );
        let tree = build_widget_tree(context.logical_width, context.logical_height, model);
        let _semantic_layout = UiLayout::from_widget_tree(
            &tree,
            &mut self.layout_engine,
            &mut self.fonts,
            context.scale_factor,
            context.logical_width,
            context.logical_height,
            model.has_media,
        )
        .filter(|layout| layout.matches_stable_geometry(fallback));
        self.layout = fallback;
        let theme = OneDarkPack.dark();
        let mut base = Frame {
            clear: rgba(theme.colors.background),
            ..Frame::default()
        };
        let mut text = Frame::default();

        if model.has_media {
            self.render_player(&mut base, &mut text, context.scale_factor, &theme, model);
        } else {
            self.render_welcome(&mut base, &mut text, context.scale_factor, &theme, model);
        }

        let mut scene = scene_from_frame(&base);
        self.render_preview(&mut scene, preview);
        let text_scene = scene_from_frame(&text);
        for command in text_scene.commands().iter().cloned() {
            scene.push(command);
        }
        scene
    }

    fn render_welcome(
        &mut self,
        base: &mut Frame,
        text: &mut Frame,
        scale: f32,
        theme: &Theme,
        model: &UiModel,
    ) {
        let colors = theme.colors;
        let center_x = self.layout.open.x + self.layout.open.width * 0.5;
        let title_y = (self.layout.open.y - 100.0).max(12.0);
        self.add_centered_text(
            text,
            "rsmpeg",
            (center_x, title_y, 32.0),
            colors.foreground,
            scale,
        );
        self.add_centered_text(
            text,
            "Native Rust multimedia player",
            (center_x, title_y + 44.0, theme.typography.body),
            colors.muted_foreground,
            scale,
        );
        self.render_button(
            base,
            text,
            theme,
            (
                self.layout.open,
                Control::Open,
                "Open file",
                ButtonVariant::Primary,
            ),
            scale,
        );
        self.add_centered_text(
            text,
            &model.status,
            (center_x, self.layout.open.y + 58.0, theme.typography.label),
            colors.muted_foreground,
            scale,
        );
        self.add_centered_text(
            text,
            "You can also drop a media file anywhere in this window",
            (center_x, self.layout.open.y + 86.0, theme.typography.label),
            colors.muted_foreground,
            scale,
        );
    }

    fn render_player(
        &mut self,
        base: &mut Frame,
        text: &mut Frame,
        scale: f32,
        theme: &Theme,
        model: &UiModel,
    ) {
        let colors = theme.colors;
        base.quads.push(Quad {
            rect: self.layout.preview.as_array(),
            color: rgba(colors.surface),
            radius: theme.radii.sm,
            border_width: 1.0,
            border_color: rgba(colors.border),
        });
        base.quads.push(Quad::solid(
            [
                0.0,
                self.layout.toolbar.y - TIMELINE_HEIGHT,
                self.layout.toolbar.width,
                TIMELINE_HEIGHT + TOOLBAR_HEIGHT,
            ],
            rgba(colors.surface),
        ));
        base.quads.push(Quad::solid(
            [
                0.0,
                self.layout.toolbar.y - TIMELINE_HEIGHT,
                self.layout.toolbar.width,
                1.0,
            ],
            rgba(colors.border),
        ));

        self.render_slider(
            base,
            theme,
            self.layout.timeline,
            timeline_fraction(model.position_sec, model.duration_sec),
            Control::Timeline,
        );
        self.render_slider(
            base,
            theme,
            self.layout.volume,
            model.volume,
            Control::Volume,
        );

        self.render_button(
            base,
            text,
            theme,
            (
                self.layout.toggle,
                Control::TogglePlayback,
                if model.playing { "Pause" } else { "Play" },
                ButtonVariant::Primary,
            ),
            scale,
        );
        self.render_button(
            base,
            text,
            theme,
            (
                self.layout.stop,
                Control::Stop,
                "Stop",
                ButtonVariant::Secondary,
            ),
            scale,
        );
        self.render_button(
            base,
            text,
            theme,
            (
                self.layout.open,
                Control::Open,
                "Open",
                ButtonVariant::Ghost,
            ),
            scale,
        );

        let timeline_y = self.layout.timeline.y - 1.0;
        self.add_text(
            text,
            &format_time(model.position_sec),
            [CONTENT_PADDING, timeline_y],
            theme.typography.label,
            colors.muted_foreground,
            scale,
        );
        self.add_text(
            text,
            &format_time(model.duration_sec),
            [
                self.layout.timeline.x + self.layout.timeline.width + 12.0,
                timeline_y,
            ],
            theme.typography.label,
            colors.muted_foreground,
            scale,
        );
        if self.layout.toolbar.width >= 600.0 {
            self.add_text(
                text,
                "Volume",
                [self.layout.volume.x - 62.0, self.layout.toolbar.y + 20.0],
                theme.typography.label,
                colors.muted_foreground,
                scale,
            );
        }

        let file_name = model.file_name.as_deref().unwrap_or("Media");
        self.add_text(
            text,
            file_name,
            [CONTENT_PADDING, 4.0],
            theme.typography.label,
            colors.foreground,
            scale,
        );
        if self.layout.toolbar.width >= 600.0 {
            self.add_text(
                text,
                &model.status,
                [CONTENT_PADDING + 180.0, self.layout.toolbar.y + 20.0],
                theme.typography.label,
                colors.muted_foreground,
                scale,
            );
        }
    }

    fn render_button(
        &mut self,
        base: &mut Frame,
        text: &mut Frame,
        theme: &Theme,
        spec: (Rect, Control, &str, ButtonVariant),
        scale: f32,
    ) {
        let (rect, control, label, variant) = spec;
        let state = ButtonState {
            hovered: rect.contains(self.cursor.0, self.cursor.1),
            pressed: self.active == Some(control),
            focused: self.focused == Some(control),
        };
        let resolved = button::<()>("button", label)
            .variant(variant)
            .resolve_style(theme, state);
        base.quads.push(Quad {
            rect: rect.as_array(),
            color: rgba(resolved.background),
            radius: theme.radii.md,
            border_width: 1.0,
            border_color: rgba(resolved.border),
        });
        self.add_centered_text(
            text,
            label,
            (
                rect.x + rect.width * 0.5,
                rect.y + 10.0,
                theme.typography.label,
            ),
            resolved.foreground,
            scale,
        );
    }

    fn render_slider(
        &self,
        frame: &mut Frame,
        theme: &Theme,
        rect: Rect,
        fraction: f32,
        control: Control,
    ) {
        let colors = theme.colors;
        let track_y = rect.y + rect.height * 0.5 - 2.0;
        frame.quads.push(Quad {
            rect: [rect.x, track_y, rect.width, 4.0],
            color: rgba(colors.input),
            radius: 2.0,
            border_width: 0.0,
            border_color: rgba(colors.input),
        });
        frame.quads.push(Quad {
            rect: [rect.x, track_y, rect.width * fraction.clamp(0.0, 1.0), 4.0],
            color: rgba(colors.primary),
            radius: 2.0,
            border_width: 0.0,
            border_color: rgba(colors.primary),
        });
        let thumb_x = rect.x + rect.width * fraction.clamp(0.0, 1.0) - 7.0;
        frame.quads.push(Quad {
            rect: [thumb_x, rect.y + 2.0, 14.0, 14.0],
            color: rgba(if self.active == Some(control) {
                colors.primary_pressed
            } else if rect.contains(self.cursor.0, self.cursor.1) {
                colors.primary_hover
            } else {
                colors.primary
            }),
            radius: 7.0,
            border_width: 2.0,
            border_color: rgba(colors.surface),
        });
    }

    fn render_preview(&self, scene: &mut Scene, preview: Option<&VideoPreview>) {
        let Some(preview) = preview else {
            return;
        };
        let Ok(image) = Rgba8Image::new(
            ImageKey::new(VIDEO_IMAGE_KEY),
            preview.revision,
            preview.width,
            preview.height,
            preview.rgba.clone(),
        ) else {
            return;
        };
        scene.push(DrawCommand::Image(ImagePrimitive::contain(
            image,
            acme_core::Rect::new(
                self.layout.preview.x,
                self.layout.preview.y,
                self.layout.preview.width,
                self.layout.preview.height,
            ),
        )));
    }

    fn add_centered_text(
        &mut self,
        frame: &mut Frame,
        value: &str,
        placement: (f32, f32, f32),
        color: ThemeColor,
        scale: f32,
    ) {
        let (center_x, y, font_size) = placement;
        let style = text_style(font_size);
        let measured = self
            .fonts
            .measure(value, &style, TextConstraints::default());
        self.add_text(
            frame,
            value,
            [center_x - measured.width * 0.5, y],
            font_size,
            color,
            scale,
        );
    }

    fn add_text(
        &mut self,
        frame: &mut Frame,
        value: &str,
        origin: [f32; 2],
        font_size: f32,
        color: ThemeColor,
        scale: f32,
    ) {
        let layout = self.fonts.shape(
            value,
            &text_style(font_size),
            TextConstraints::default(),
            scale,
        );
        frame.text.push(TextRun {
            prepared: self.fonts.prepare(&layout, &mut self.atlas),
            origin,
            color: rgba(color),
            clip: None,
        });
    }
}

fn build_widget_tree(width: f32, height: f32, model: &UiModel) -> WidgetNode<LayoutMessage> {
    let width = width.max(320.0);
    let height = height.max(220.0);
    let body_width = width - CONTENT_PADDING * 2.0;
    if !model.has_media {
        return column::<LayoutMessage>()
            .key("welcome")
            .width(width)
            .height(height)
            .padding(CONTENT_PADDING)
            .gap(12.0)
            .child(column::<LayoutMessage>().height((height * 0.32).max(96.0)))
            .child(label("rsmpeg"))
            .child(label("Native Rust multimedia player"))
            .child(
                button("open", "Open file")
                    .primary()
                    .on_click(LayoutMessage::Open),
            )
            .child(label(model.status.as_str()))
            .child(label("Drop a media file anywhere in this window"))
            .build();
    }

    let time_width = 48.0;
    let timeline_width = (body_width - time_width * 2.0 - 24.0).max(120.0);
    let status_width = (body_width - 92.0 - 72.0 - 120.0 - 38.0 - 56.0).max(80.0);
    let preview_height =
        (height - CONTENT_PADDING * 2.0 - 24.0 - TIMELINE_HEIGHT - TOOLBAR_HEIGHT - 24.0).max(60.0);

    column::<LayoutMessage>()
        .key("player")
        .width(width)
        .height(height)
        .padding(CONTENT_PADDING)
        .gap(8.0)
        .child(label(model.file_name.as_deref().unwrap_or("Media preview")))
        .child(
            column::<LayoutMessage>()
                .key("preview")
                .width(body_width)
                .height(preview_height),
        )
        .child(
            row::<LayoutMessage>()
                .key("timeline-row")
                .width(body_width)
                .height(TIMELINE_HEIGHT)
                .gap(12.0)
                .child(column::<LayoutMessage>().width(time_width).height(18.0))
                .child(
                    column::<LayoutMessage>()
                        .key("timeline-slider")
                        .width(timeline_width)
                        .height(18.0),
                )
                .child(column::<LayoutMessage>().width(time_width).height(18.0)),
        )
        .child(
            row::<LayoutMessage>()
                .key("transport")
                .width(body_width)
                .height(TOOLBAR_HEIGHT)
                .gap(8.0)
                .child(
                    button("toggle", if model.playing { "Pause" } else { "Play" })
                        .primary()
                        .on_click(LayoutMessage::Toggle),
                )
                .child(button("stop", "Stop").on_click(LayoutMessage::Stop))
                .child(
                    column::<LayoutMessage>()
                        .key("status")
                        .width(status_width)
                        .height(CONTROL_HEIGHT),
                )
                .child(
                    column::<LayoutMessage>()
                        .key("volume-label")
                        .width(56.0)
                        .height(CONTROL_HEIGHT),
                )
                .child(
                    column::<LayoutMessage>()
                        .key("volume-slider")
                        .width(120.0)
                        .height(18.0),
                )
                .child(button("open", "Open").on_click(LayoutMessage::Open)),
        )
        .build()
}

fn layout_rect(snapshot: &LayoutSnapshot, id: NodeId) -> Option<Rect> {
    let rect = snapshot.get(id)?;
    Some(Rect::new(rect.x, rect.y, rect.width, rect.height))
}

fn text_style(font_size: f32) -> TextStyle {
    TextStyle {
        font_size,
        line_height: font_size * 1.35,
        ..TextStyle::default()
    }
}

fn fraction_at(rect: Rect, x: f32) -> f32 {
    if rect.width <= 0.0 {
        return 0.0;
    }
    ((x - rect.x) / rect.width).clamp(0.0, 1.0)
}

fn timeline_fraction(position: f64, duration: f64) -> f32 {
    if !position.is_finite() || !duration.is_finite() || duration <= 0.0 {
        return 0.0;
    }
    (position / duration).clamp(0.0, 1.0) as f32
}

fn format_time(seconds: f64) -> String {
    let total = if seconds.is_finite() {
        seconds.max(0.0) as u64
    } else {
        0
    };
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

fn rgba(color: ThemeColor) -> [f32; 4] {
    [color.red, color.green, color.blue, color.alpha]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slider_fraction_clamps_to_track() {
        let track = Rect::new(10.0, 0.0, 100.0, 18.0);
        assert_eq!(fraction_at(track, -20.0), 0.0);
        assert_eq!(fraction_at(track, 60.0), 0.5);
        assert_eq!(fraction_at(track, 140.0), 1.0);
    }

    #[test]
    fn timeline_fraction_handles_invalid_media_duration() {
        assert_eq!(timeline_fraction(5.0, 0.0), 0.0);
        assert_eq!(timeline_fraction(f64::NAN, 10.0), 0.0);
        assert_eq!(timeline_fraction(5.0, 10.0), 0.5);
        assert_eq!(timeline_fraction(20.0, 10.0), 1.0);
    }

    #[test]
    fn timeline_release_always_emits_final_commit() {
        let mut ui = PlayerUi::new();
        ui.layout = UiLayout::for_window(960.0, 600.0, true);
        let x = ui.layout.timeline.x + ui.layout.timeline.width * 0.25;
        let y = ui.layout.timeline.y + 4.0;

        let pressed = ui.pointer_button(true, x, y);
        assert_eq!(
            pressed,
            vec![UiIntent::Seek {
                fraction: 0.25,
                final_commit: false
            }]
        );
        let released = ui.pointer_button(false, x, y);
        assert_eq!(
            released,
            vec![UiIntent::Seek {
                fraction: 0.25,
                final_commit: true
            }]
        );
    }

    #[test]
    fn time_format_uses_hours_only_when_needed() {
        assert_eq!(format_time(65.0), "01:05");
        assert_eq!(format_time(3661.0), "01:01:01");
        assert_eq!(format_time(f64::NAN), "00:00");
    }

    #[test]
    fn keyboard_focus_and_slider_controls_emit_intents() {
        let mut ui = PlayerUi::new();
        let model = UiModel {
            has_media: true,
            file_name: Some("clip.mp4".into()),
            status: "Playing".into(),
            playing: true,
            position_sec: 50.0,
            duration_sec: 100.0,
            volume: 0.5,
        };

        ui.focused = Some(Control::Timeline);
        assert_eq!(
            ui.key_pressed(PlatformKey::ArrowRight, false, &model),
            vec![UiIntent::Seek {
                fraction: 0.51,
                final_commit: true
            }]
        );
        ui.focused = Some(Control::Volume);
        assert_eq!(
            ui.key_pressed(PlatformKey::ArrowLeft, false, &model),
            vec![UiIntent::Volume(0.45)]
        );
        ui.focused = Some(Control::TogglePlayback);
        assert_eq!(
            ui.key_pressed(PlatformKey::Space, false, &model),
            vec![UiIntent::TogglePlayback]
        );
    }

    #[test]
    fn compact_welcome_content_stays_inside_minimum_height() {
        let layout = UiLayout::for_window(320.0, 220.0, false);
        assert!(layout.open.y >= 0.0);
        assert!(layout.open.y + 86.0 + 16.0 <= 220.0);
    }
}
