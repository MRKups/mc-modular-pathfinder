//! Session visuals are a bounded native component tree, with no browser surface.
use crate::theme::{accent_color, content};
use gpui::{
    AnyElement, Bounds, Context, Div, EventEmitter, FontWeight, Hsla, IntoElement, MouseButton,
    PathBuilder, Pixels, Point, Render, Role, SharedString, Stateful, Task, Window, canvas, div,
    fill, point, prelude::*, px, relative, size,
};
use monocode_core::visual::{
    Camera, ContextMenuItem, Node, SceneObject, Shape, TextSize, ThemeColor, VisualDocument,
    VisualRuntime,
};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet, VecDeque},
    rc::Rc,
    sync::{Arc, Mutex},
};

mod charts;
pub(crate) mod dither;
mod scene;
#[cfg(test)]
mod tests;

/// Reading width for plugin pages. The rail sits to the left of this column.
const PAGE_CONTENT_MAX_WIDTH: f32 = 800.;

/// The transcript must remeasure this row when its content changes.
pub(crate) struct VisualChanged;
pub(crate) type RemoteEvent = Rc<dyn Fn(&str, Value)>;
pub(crate) struct VisualView {
    document: VisualDocument,
    runtime: Option<VisualRuntime>,
    /// Plugin surfaces forward interactions to an external process instead of
    /// running a QuickJS program inside the host.
    remote: Option<RemoteEvent>,
    tree: Option<Node>,
    error: Option<String>,
    width: Rc<Cell<f32>>,
    tracks: HashMap<String, Rc<Cell<Option<Bounds<Pixels>>>>>,
    slider_drag: Option<SliderDrag>,
    orbits: HashMap<String, scene::Orbit>,
    scene_drag: Option<(String, Point<Pixels>)>,
    inputs: HashMap<String, InputSlot>,
    editors: HashMap<String, CodeEditorSlot>,
    editor_menu: Option<OpenEditorMenu>,
    active: HashSet<String>,
    revision: u64,
    scene_cache: HashMap<String, Rc<RefCell<Option<SceneCache>>>>,
    /// Placed in an answer's prose: no title bar, its caption underneath.
    inline: Option<String>,
    /// When the visual first appeared, for its charts' entrances.
    motion: dither::Motion,
}
impl EventEmitter<VisualChanged> for VisualView {}
struct SliderDrag {
    key: String,
    handler: String,
    min: f64,
    max: f64,
    step: f64,
}
struct InputSlot {
    input: gpui::Entity<crate::input::Input>,
    handler: String,
    submit_handler: Option<String>,
    source_value: String,
    last_remote: String,
    pending: VecDeque<String>,
    _subscriptions: Vec<gpui::Subscription>,
}
struct CodeEditorSlot {
    input: gpui::Entity<crate::input::Input>,
    handler: String,
    submit_handler: Option<String>,
    selection_handler: Option<String>,
    context_menu: Vec<ContextMenuItem>,
    source_value: String,
    last_remote: String,
    last_selection: String,
    pending: VecDeque<String>,
    height: f32,
    read_only: bool,
    path: String,
    syntax: Option<Arc<Mutex<Option<crate::editor_syntax::Session>>>>,
    _analysis: Option<Task<()>>,
    _subscriptions: Vec<gpui::Subscription>,
}
struct OpenEditorMenu {
    editor_key: String,
    anchor: Point<Pixels>,
    selected_text: String,
    fallback_handlers: Vec<String>,
    menu: gpui::Entity<crate::popover::Menu>,
    _events: gpui::Subscription,
}
struct SceneCache {
    revision: u64,
    dimensions: [f32; 2],
    orbit: scene::Orbit,
    projection: Rc<scene::Projection>,
}

fn color(token: &ThemeColor) -> Hsla {
    // Semantic hues need darker variants when the interface uses dark ink.
    let light_surface = content(1.).l < 0.5;
    let hue = |dark, light| gpui::rgb(if light_surface { light } else { dark }).into();
    // Series 1 is the user's accent. A later series of the same hue would be
    // indistinguishable, so it takes the first spare hue that differs.
    let series = |dark, light| {
        let accent = accent_color();
        let near = |c: Hsla| {
            let d = (c.h - accent.h).abs();
            accent.s > 0.25 && d.min(1. - d) < 30. / 360.
        };
        let own: Hsla = hue(dark, light);
        if !near(own) {
            return own;
        }
        [
            (0x2dd4bf, 0x0d7f73),
            (0xe2c541, 0x8a6d00),
            (0xfc5855, 0xd01d21),
        ]
        .into_iter()
        .map(|(dark, light)| hue(dark, light))
        .find(|c| !near(*c))
        .unwrap_or(own)
    };
    match token {
        ThemeColor::Ink => content(1.),
        ThemeColor::Muted => content(0.6),
        ThemeColor::Faint => content(0.2),
        ThemeColor::Accent | ThemeColor::Series1 => accent_color(),
        // Series hues, tuned separately for dark and light surfaces.
        // Semantic colours keep their meaning even when close to the accent.
        ThemeColor::Positive => hue(0x4fd57f, 0x0a924b),
        ThemeColor::Negative => hue(0xfc5855, 0xd01d21),
        ThemeColor::Warning => hue(0xfea247, 0xd96700),
        ThemeColor::Series2 => series(0x4fd57f, 0x0a924b),
        ThemeColor::Series3 => series(0xb77df8, 0x863ad2),
        ThemeColor::Series4 => series(0xfea247, 0xd96700),
        ThemeColor::Series5 => series(0xfa78b2, 0xd12c80),
        ThemeColor::Series6 => series(0x3faff3, 0x0077d7),
    }
}
fn control(id: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id.into())
        .role(Role::Button)
        .focusable()
        .tab_stop(true)
        .h(px(26.))
        .px(px(9.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .text_size(px(12.))
        .line_height(px(16.))
        .cursor_pointer()
}
fn button(id: impl Into<SharedString>) -> Stateful<Div> {
    control(id)
        .border_1()
        .border_color(content(0.12))
        .text_color(content(0.8))
        .hover(|s| s.bg(content(0.06)).text_color(content(1.)))
        .focus(|s| s.border_color(accent_color()))
}
fn tab_button(id: impl Into<SharedString>, selected: bool) -> Stateful<Div> {
    let light = content(1.).l < 0.5;
    let selected_fill: Hsla = gpui::rgb(if light { 0x000000 } else { 0xffffff }).into();
    let selected_text: Hsla = gpui::rgb(if light { 0xffffff } else { 0x141414 }).into();
    control(id)
        .text_color(if selected {
            selected_text
        } else {
            content(0.6)
        })
        .when(selected, |d| d.bg(selected_fill))
        .hover(move |s| {
            if selected {
                s.bg(selected_fill).text_color(selected_text)
            } else {
                s.bg(content(0.06)).text_color(content(1.))
            }
        })
        .focus_visible(move |s| {
            s.bg(if selected {
                selected_fill
            } else {
                content(0.08)
            })
        })
}

fn activated(event: &gpui::KeyDownEvent) -> bool {
    matches!(event.keystroke.key.as_str(), "enter" | "space" | " ")
}

impl VisualView {
    pub(crate) fn new(document: VisualDocument, _cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            document,
            runtime: None,
            remote: None,
            tree: None,
            error: None,
            width: Rc::new(Cell::new(640.)),
            tracks: HashMap::new(),
            slider_drag: None,
            orbits: HashMap::new(),
            scene_drag: None,
            inputs: HashMap::new(),
            editors: HashMap::new(),
            editor_menu: None,
            active: HashSet::new(),
            revision: 0,
            scene_cache: HashMap::new(),
            inline: None,
            motion: dither::Motion(std::time::Instant::now()),
        };
        this.initialize();
        this
    }
    /// A native tree owned by an external plugin. No script runs in the host;
    /// control events are forwarded to the plugin process by `on_event`.
    pub(crate) fn remote(id: String, title: String, tree: Node, on_event: RemoteEvent) -> Self {
        Self {
            document: VisualDocument {
                id,
                title,
                source: "plugin".into(),
            },
            runtime: None,
            remote: Some(on_event),
            tree: Some(tree),
            error: None,
            width: Rc::new(Cell::new(640.)),
            tracks: HashMap::new(),
            slider_drag: None,
            orbits: HashMap::new(),
            scene_drag: None,
            inputs: HashMap::new(),
            editors: HashMap::new(),
            editor_menu: None,
            active: HashSet::new(),
            revision: 0,
            scene_cache: HashMap::new(),
            // The plugin host draws its own surface header.
            inline: Some(String::new()),
            motion: dither::Motion(std::time::Instant::now()),
        }
    }
    /// Replace a remote tree after the plugin process sends an update.
    pub(crate) fn set_tree(&mut self, tree: Node, cx: &mut Context<Self>) {
        self.apply_tree(tree);
        self.changed(cx);
    }
    /// Draws the visual inside an answer, captioned, or back in its own row.
    pub(crate) fn set_inline(&mut self, caption: Option<String>, cx: &mut Context<Self>) {
        if self.inline != caption {
            self.inline = caption;
            self.changed(cx);
        }
    }
    fn initialize(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.error = None;
        self.tree = None;
        self.runtime = None;
        match self
            .document
            .validate()
            .and_then(|()| VisualRuntime::new(&self.document.source))
            .and_then(|mut runtime| {
                let tree = runtime.render()?;
                Ok((runtime, tree))
            }) {
            Ok((runtime, tree)) => {
                self.runtime = Some(runtime);
                self.tree = Some(tree);
            }
            Err(error) => self.error = Some(error),
        }
    }
    fn changed(&self, cx: &mut Context<Self>) {
        cx.emit(VisualChanged);
        cx.notify();
    }
    fn dispatch(&mut self, handler: &str, value: Value, cx: &mut Context<Self>) {
        if self.remote.is_some() {
            for slot in self.inputs.values_mut() {
                slot.pending.clear();
            }
            for slot in self.editors.values_mut() {
                slot.pending.clear();
            }
        }
        self.dispatch_raw(handler, value, cx);
    }
    fn dispatch_raw(&mut self, handler: &str, value: Value, cx: &mut Context<Self>) {
        if let Some(remote) = self.remote.clone() {
            remote(handler, value);
            return;
        }
        let Some(runtime) = self.runtime.as_mut() else {
            return;
        };
        match runtime.dispatch(handler, value) {
            Ok(tree) => self.apply_tree(tree),
            Err(error) => {
                self.error = Some(error);
                self.slider_drag = None;
                self.scene_drag = None;
            }
        }
        self.changed(cx);
    }
    fn apply_tree(&mut self, tree: Node) {
        self.revision = self.revision.wrapping_add(1);
        // A render refreshes callback capabilities. A pointer drag and a
        // focused input survive that render, so refresh their bindings too.
        if let Some(drag) = self.slider_drag.as_mut() {
            if let Some(Node::Slider {
                handler,
                min,
                max,
                step,
                ..
            }) = node_at(&tree, &drag.key)
            {
                drag.handler = handler.clone();
                drag.min = *min;
                drag.max = *max;
                drag.step = *step;
            } else {
                self.slider_drag = None;
            }
        }
        for (key, slot) in &mut self.inputs {
            if let Some(Node::Input {
                handler,
                submit_handler,
                ..
            }) = node_at(&tree, key)
            {
                slot.handler = handler.clone();
                slot.submit_handler = submit_handler.clone();
            }
        }
        for (key, slot) in &mut self.editors {
            if let Some(Node::CodeEditor {
                handler,
                submit_handler,
                selection_handler,
                context_menu,
                ..
            }) = node_at(&tree, key)
            {
                slot.handler = handler.clone();
                slot.submit_handler = submit_handler.clone();
                slot.selection_handler = selection_handler.clone();
                slot.context_menu = context_menu.clone();
            }
        }
        self.tree = Some(tree);
        self.error = None;
    }
    fn open_editor_menu(
        &mut self,
        key: &str,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(slot) = self.editors.get(key) else {
            return;
        };
        if slot.context_menu.is_empty() {
            return;
        }
        let selected_text = slot.input.read(cx).selected_text();
        let has_selection = !selected_text.trim().is_empty();
        let mut items: Vec<crate::popover::MenuItem> = Vec::with_capacity(slot.context_menu.len());
        let mut fallback_handlers = Vec::with_capacity(slot.context_menu.len());
        for (idx, item) in slot.context_menu.iter().enumerate() {
            let disabled = item.disabled || (item.requires_selection && !has_selection);
            items.push(
                crate::popover::MenuAction::new(format!("{idx}"), item.label.clone())
                    .disabled(disabled)
                    .into(),
            );
            fallback_handlers.push(item.handler.clone());
        }
        self.editor_menu = None;
        let menu =
            cx.new(|cx| crate::popover::Menu::new("Editor actions", items, window, cx).width(196.));
        let events = cx.subscribe_in(
            &menu,
            window,
            |this, _, event: &crate::popover::MenuEvent, _, cx| {
                let Some(open) = this.editor_menu.take() else {
                    return;
                };
                if let crate::popover::MenuEvent::Picked(id, _) = event
                    && let Ok(index) = id.parse::<usize>()
                {
                    let live_handler = this
                        .editors
                        .get(&open.editor_key)
                        .and_then(|slot| slot.context_menu.get(index))
                        .map(|item| item.handler.clone())
                        .or_else(|| open.fallback_handlers.get(index).cloned());
                    if let Some(handler) = live_handler {
                        let payload = if open.selected_text.is_empty() {
                            Value::Null
                        } else {
                            json!(open.selected_text)
                        };
                        this.dispatch(&handler, payload, cx);
                    }
                }
                cx.notify();
            },
        );
        self.editor_menu = Some(OpenEditorMenu {
            editor_key: key.to_owned(),
            anchor: at,
            selected_text,
            fallback_handlers,
            menu,
            _events: events,
        });
        cx.notify();
    }
    fn sync_editor_selection(&mut self, key: &str, cx: &mut Context<Self>) {
        let next = {
            let Some(slot) = self.editors.get_mut(key) else {
                return;
            };
            let selected = slot.input.read(cx).selected_text();
            if selected == slot.last_selection {
                return;
            }
            slot.last_selection = selected.clone();
            slot.selection_handler.clone().map(|h| (h, selected))
        };
        if let Some((handler, selected)) = next {
            self.dispatch_raw(&handler, json!(selected), cx);
        }
    }
    fn slide(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some(drag) = &self.slider_drag else {
            return;
        };
        let Some(bounds) = self.tracks.get(&drag.key).and_then(|b| b.get()) else {
            return;
        };
        let fraction = ((x - bounds.left()).as_f32() / bounds.size.width.as_f32().max(1.))
            .clamp(0., 1.) as f64;
        let raw = drag.min + (drag.max - drag.min) * fraction;
        let value = (drag.min + ((raw - drag.min) / drag.step).round() * drag.step)
            .clamp(drag.min, drag.max);
        let handler = drag.handler.clone();
        self.dispatch(&handler, json!(value), cx);
    }
    fn analyze_editor(&mut self, key: &str, text: String, cx: &mut Context<Self>) {
        let Some(slot) = self.editors.get(key) else {
            return;
        };
        let Some(syntax) = slot.syntax.clone() else {
            return;
        };
        let input = slot.input.clone();
        let path = slot.path.clone();
        let task = cx.spawn(async move |_, cx| {
            let work = cx.background_executor().spawn({
                let syntax = syntax.clone();
                let path = path.clone();
                let text = text.clone();
                async move { crate::editor_syntax::analyze(&syntax, &path, &text, None) }
            });
            if let Ok(analysis) = work.await {
                input.update(cx, |input, cx| input.set_editor_analysis(analysis, cx));
            }
        });
        if let Some(slot) = self.editors.get_mut(key) {
            slot._analysis = Some(task);
        }
    }
    fn node(&mut self, node: &Node, key: String, cx: &mut Context<Self>) -> AnyElement {
        self.active.insert(key.clone());
        match node {
            Node::Spacer => div().h(px(4.)).into_any_element(),
            Node::Column { .. }
            | Node::Row { .. }
            | Node::Grid { .. }
            | Node::Card { .. } => self.render_layout(node, key, cx),
            Node::Text {
                text,
                size,
                color: token,
                monospace,
            } => {
                let font_size = match size {
                    TextSize::Caption => 11.,
                    TextSize::Small => 12.,
                    TextSize::Body => 14.,
                    TextSize::Subtitle => 17.,
                    TextSize::Title => 22.,
                    TextSize::Hero => 34.,
                };
                div()
                    .min_w_0()
                    .text_size(px(font_size))
                    .line_height(px(font_size * 1.45))
                    .text_color(color(token))
                    .when(*monospace, |d| d.font_family(crate::fonts::mono()))
                    .when(
                        matches!(size, TextSize::Subtitle | TextSize::Title | TextSize::Hero),
                        |d| d.font_weight(FontWeight::SEMIBOLD),
                    )
                    .child(text.clone())
                    .into_any_element()
            }
            Node::Stat {
                label,
                value,
                detail,
                color: token,
                delta,
                trend,
                lower_is_better,
            } => {
                let (delta, detail) = match (delta, detail) {
                    (Some(delta), detail) => (Some(delta.clone()), detail.clone()),
                    (None, Some(detail)) => match charts::split_delta(detail) {
                        Some((delta, rest)) => (
                            Some(delta.to_owned()),
                            (!rest.is_empty()).then(|| rest.to_owned()),
                        ),
                        None => (None, Some(detail.clone())),
                    },
                    (None, None) => (None, None),
                };
                let tint = if matches!(
                    token,
                    ThemeColor::Ink | ThemeColor::Muted | ThemeColor::Faint
                ) {
                    accent_color()
                } else {
                    color(token)
                };
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(content(0.6))
                            .truncate()
                            .child(label.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(30.))
                            .line_height(px(38.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .font_features(crate::right_panel::tabular())
                            .text_color(color(token))
                            .child(value.clone()),
                    )
                    .when(delta.is_some() || detail.is_some(), |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_x_2()
                                .children(
                                    delta.as_deref().map(|d| charts::delta(d, *lower_is_better)),
                                )
                                .children(detail.map(|detail| {
                                    div()
                                        .text_size(px(12.))
                                        .text_color(content(0.55))
                                        .child(detail)
                                })),
                        )
                    })
                    .when(trend.len() > 1, |d| {
                        d.child(
                            div()
                                .pt_2()
                                .child(charts::trend(trend.clone(), tint, self.motion)),
                        )
                    })
                    .into_any_element()
            }
            Node::Table { .. } => self.render_table(node, key, cx),
            Node::Button {
                label,
                handler,
                disabled,
            } => {
                let handler = handler.clone();
                let keyboard = handler.clone();
                button(SharedString::from(format!("visual-button-{key}")))
                    .debug_selector(move || format!("visual-button-{key}"))
                    .aria_label(label.clone())
                    .child(label.clone())
                    .when(*disabled, |d| d.opacity(0.4).cursor_default())
                    .when(!disabled, |d| {
                        d.on_click(cx.listener(move |this, _, _, cx| {
                            this.dispatch(&handler, Value::Null, cx)
                        }))
                        .on_key_down(cx.listener(
                            move |this, event, _, cx| {
                                if activated(event) {
                                    this.dispatch(&keyboard, Value::Null, cx);
                                    cx.stop_propagation();
                                }
                            },
                        ))
                    })
                    .into_any_element()
            }
            Node::Toggle {
                label,
                value,
                handler,
            } => {
                let next = !*value;
                let handler = handler.clone();
                let keyboard = handler.clone();
                div()
                    .id(SharedString::from(format!("visual-toggle-{key}")))
                    .debug_selector(move || format!("visual-toggle-{key}"))
                    .role(Role::Switch)
                    .aria_label(label.clone())
                    .aria_toggled(if *value {
                        gpui::Toggled::True
                    } else {
                        gpui::Toggled::False
                    })
                    .focusable()
                    .tab_stop(true)
                    .flex()
                    .items_center()
                    .gap_2()
                    .py_1()
                    .cursor_pointer()
                    .child(
                        div()
                            .w(px(30.))
                            .h(px(18.))
                            .rounded_full()
                            .bg(if *value {
                                accent_color()
                            } else {
                                content(0.18)
                            })
                            .relative()
                            .child(
                                div()
                                    .absolute()
                                    .top(px(3.))
                                    .left(px(if *value { 15. } else { 3. }))
                                    .size(px(12.))
                                    .rounded_full()
                                    .bg(content(1.)),
                            ),
                    )
                    .child(label.clone())
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.dispatch(&handler, json!(next), cx)),
                    )
                    .on_key_down(cx.listener(move |this, event, _, cx| {
                        if activated(event) {
                            this.dispatch(&keyboard, json!(next), cx);
                            cx.stop_propagation();
                        }
                    }))
                    .into_any_element()
            }
            Node::Slider { .. } => self.render_slider(node, key, cx),
            Node::Tabs {
                labels,
                selected,
                handler,
            } => {
                let mut tabs = div().flex().flex_wrap().gap_1();
                for (i, label) in labels.iter().enumerate() {
                    let handler = handler.clone();
                    let keyboard = handler.clone();
                    tabs =
                        tabs.child(
                            tab_button(
                                SharedString::from(format!("visual-tab-{key}-{i}")),
                                i == *selected,
                            )
                            .debug_selector({
                                let key = key.clone();
                                move || format!("visual-tab-{key}-{i}")
                            })
                            .aria_label(label.clone())
                            .aria_toggled(if i == *selected {
                                gpui::Toggled::True
                            } else {
                                gpui::Toggled::False
                            })
                            .child(label.clone())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.dispatch(&handler, json!(i), cx)
                            }))
                            .on_key_down(cx.listener(
                                move |this, event, _, cx| {
                                    if activated(event) {
                                        this.dispatch(&keyboard, json!(i), cx);
                                        cx.stop_propagation();
                                    }
                                },
                            )),
                        );
                }
                tabs.into_any_element()
            }
            Node::PageTabs { .. } => self.render_page_tabs(node, key, cx),
            Node::Input { .. } => self.render_input(node, key, cx),
            Node::CodeEditor { .. } => self.render_code_editor(node, key, cx),
            Node::Select {
                label,
                options,
                selected,
                handler,
            } => {
                // The options name themselves; the label stays for assistive
                // technology only.
                let mut choices = div()
                    .id(SharedString::from(format!("visual-select-{key}")))
                    .role(Role::Group)
                    .aria_label(label.clone())
                    .flex()
                    .flex_wrap()
                    .gap_1();
                for (i, option) in options.iter().enumerate() {
                    let handler = handler.clone();
                    let keyboard = handler.clone();
                    choices = choices.child(
                        button(SharedString::from(format!("visual-option-{key}-{i}")))
                            .debug_selector({
                                let key = key.clone();
                                move || format!("visual-option-{key}-{i}")
                            })
                            .aria_label(option.clone())
                            .child(option.clone())
                            .when(i == *selected, |d| {
                                d.bg(content(0.09))
                                    .border_color(accent_color())
                                    .text_color(content(1.))
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.dispatch(&handler, json!(i), cx)
                            }))
                            .on_key_down(cx.listener(move |this, event, _, cx| {
                                if activated(event) {
                                    this.dispatch(&keyboard, json!(i), cx);
                                    cx.stop_propagation();
                                }
                            })),
                    );
                }
                choices.into_any_element()
            }
            Node::Chart {
                chart_type,
                series,
                labels,
                height,
            } => charts::chart(
                *chart_type,
                series.clone(),
                labels.clone(),
                *height,
                self.motion,
                format!("visual-chart-{key}").into(),
            ),
            Node::Gauge {
                label,
                value,
                min,
                max,
                unit,
                color: token,
            } => charts::gauge(
                label.clone(),
                *value,
                *min,
                *max,
                unit.clone(),
                *token,
                self.motion,
            ),
            Node::Donut {
                slices,
                label,
                value,
            } => charts::donut(
                slices.clone(),
                label.clone(),
                value.clone(),
                self.motion,
                format!("visual-donut-{key}").into(),
            ),
            Node::Heatmap {
                values,
                rows,
                columns,
                color: token,
            } => charts::heatmap(
                values.clone(),
                rows.clone(),
                columns.clone(),
                *token,
                self.motion,
                format!("visual-heatmap-{key}").into(),
            ),
            Node::Progress {
                label,
                value,
                max,
                detail,
                color: token,
            } => charts::progress(
                label.clone(),
                *value,
                *max,
                detail.clone(),
                *token,
                self.motion,
            ),
            Node::Canvas {
                width,
                height,
                shapes,
            } => drawing(*width, *height, shapes.clone()),
            Node::Scene {
                width: _,
                height,
                objects,
                camera,
            } => self.scene(key, *height, objects.clone(), camera.clone(), cx),
        }
    }
    #[inline(never)]
    fn render_layout(&mut self, node: &Node, key: String, cx: &mut Context<Self>) -> AnyElement {
        let (children, gap, column_width) = match node {
            Node::Column {
                children,
                gap,
                width,
                ..
            } => (children, *gap, *width),
            Node::Row { children, gap }
            | Node::Grid { children, gap, .. }
            | Node::Card { children, gap } => (children, *gap, None),
            _ => return div().into_any_element(),
        };
        let mut layout = div().min_w_0().gap(px(gap));
        if let Some(w) = column_width {
            layout = layout.w(px(w)).flex_none();
        } else {
            layout = layout.w_full();
        }
        if matches!(node, Node::Card { .. }) {
            layout = layout
                .p_4()
                .rounded_xl()
                .bg(content(0.03))
                .border_1()
                .border_color(content(0.08));
        }
        let is_row = matches!(node, Node::Row { .. });
        if let Node::Grid { columns, .. } = node {
            let responsive =
                ((*columns).min((self.width.get() / 180.).floor().max(1.) as usize)).max(1);
            layout = layout.grid().grid_cols(responsive as u16);
        } else if is_row {
            let inline_only = children.iter().all(|child| {
                matches!(
                    child,
                    Node::Button { .. }
                        | Node::Toggle { .. }
                        | Node::Input { .. }
                        | Node::Select { .. }
                        | Node::Tabs { .. }
                        | Node::Text { .. }
                        | Node::Spacer
                )
            });
            layout = layout.flex().flex_wrap();
            layout = if inline_only {
                layout.items_center()
            } else {
                layout.items_start()
            };
        } else {
            layout = layout.flex().flex_col();
        }
        for (i, child) in children.iter().enumerate() {
            if is_row && matches!(child, Node::Spacer) {
                self.active.insert(format!("{key}/{i}"));
                layout = layout.child(div().flex_1().min_w_0());
                continue;
            }
            let element = self.node(child, format!("{key}/{i}"), cx);
            let wrapper = if is_row {
                match child {
                    Node::Column {
                        width: Some(w), ..
                    } => div().w(px(*w)).flex_none().child(element),
                    Node::Column {
                        weight: Some(weight),
                        ..
                    } => div()
                        .min_w_0()
                        .flex_grow(*weight)
                        .flex_shrink(1.)
                        .flex_basis(px(0.))
                        .min_w(px(180.))
                        .child(element),
                    Node::Button { .. }
                    | Node::Toggle { .. }
                    | Node::Select { .. }
                    | Node::Tabs { .. } => div().flex_none().child(element),
                    Node::Text { .. } => div().min_w_0().child(element),
                    _ => div().min_w_0().flex_1().min_w(px(120.)).child(element),
                }
            } else {
                div().min_w_0().child(element)
            };
            layout = layout.child(wrapper);
        }
        layout.into_any_element()
    }
    #[inline(never)]
    fn render_table(&mut self, node: &Node, key: String, cx: &mut Context<Self>) -> AnyElement {
        let Node::Table {
            columns,
            rows,
            weights,
            handler,
            selected,
        } = node
        else {
            return div().into_any_element();
        };
        let mut table = div()
            .id(SharedString::from(format!("visual-table-{key}")))
            .debug_selector({
                let key = key.clone();
                move || format!("visual-table-{key}")
            })
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .text_size(px(12.))
            .font_features(crate::right_panel::tabular());
        let fallback = 1.;
        let weights = (0..columns.len())
            .map(|index| weights.get(index).copied().unwrap_or(fallback))
            .collect::<Vec<_>>();
        let render_cells = |cells: &[String]| {
            cells
                .iter()
                .enumerate()
                .map(|(index, cell)| {
                    div()
                        .min_w_0()
                        .flex_grow(weights[index])
                        .flex_shrink(1.)
                        .flex_basis(px(0.))
                        .overflow_hidden()
                        .px(px(8.))
                        .py(px(5.))
                        .child(div().min_w_0().truncate().child(cell.clone()))
                })
                .collect::<Vec<_>>()
        };
        table = table.child(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .border_b_1()
                .border_color(content(0.1))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(content(0.65))
                .children(render_cells(columns)),
        );
        for (row_index, cells) in rows.iter().enumerate() {
            let is_selected = *selected == Some(row_index);
            if let Some(handler) = handler.clone() {
                let keyboard = handler.clone();
                let row_id = format!("visual-table-row-{key}-{row_index}");
                let selector_id = row_id.clone();
                let label = cells.join(" ");
                table = table.child(
                    div()
                        .id(SharedString::from(row_id))
                        .debug_selector(move || selector_id.clone())
                        .role(Role::Button)
                        .aria_label(label)
                        .focusable()
                        .tab_stop(true)
                        .w_full()
                        .min_w_0()
                        .flex()
                        .border_b_1()
                        .border_color(content(0.08))
                        .cursor_pointer()
                        .when(is_selected, |d| d.bg(content(0.08)))
                        .hover(move |s| s.bg(content(if is_selected { 0.1 } else { 0.05 })))
                        .focus_visible(|s| s.bg(content(0.06)))
                        .children(render_cells(cells))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.dispatch(&handler, json!(row_index), cx)
                        }))
                        .on_key_down(cx.listener(move |this, event, _, cx| {
                            if activated(event) {
                                this.dispatch(&keyboard, json!(row_index), cx);
                                cx.stop_propagation();
                            }
                        })),
                );
            } else {
                table = table.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .border_b_1()
                        .border_color(content(0.08))
                        .when(is_selected, |d| d.bg(content(0.08)))
                        .children(render_cells(cells)),
                );
            }
        }
        table.into_any_element()
    }
    #[inline(never)]
    fn render_slider(&mut self, node: &Node, key: String, cx: &mut Context<Self>) -> AnyElement {
        let Node::Slider {
            label,
            value,
            min,
            max,
            step,
            handler,
        } = node
        else {
            return div().into_any_element();
        };
        let fraction = ((*value - *min) / (*max - *min).max(f64::EPSILON)).clamp(0., 1.) as f32;
        let bounds = self
            .tracks
            .entry(key.clone())
            .or_insert_with(|| Rc::new(Cell::new(None)))
            .clone();
        let min = *min;
        let max = *max;
        let step = *step;
        let current = *value;
        let handler = handler.clone();
        let keyboard = handler.clone();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .min_w(px(120.))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .gap_3()
                    .text_size(px(12.))
                    .child(label.clone())
                    .child(format_number(*value)),
            )
            .child(
                div()
                    .id(SharedString::from(format!("visual-slider-{key}")))
                    .debug_selector({
                        let key = key.clone();
                        move || format!("visual-slider-{key}")
                    })
                    .role(Role::Slider)
                    .aria_label(label.clone())
                    .focusable()
                    .tab_stop(true)
                    .w_full()
                    .h(px(20.))
                    .flex()
                    .items_center()
                    .relative()
                    .cursor_pointer()
                    .child(
                        canvas(move |rect, _, _| bounds.set(Some(rect)), |_, _, _, _| {})
                            .absolute()
                            .size_full(),
                    )
                    .child(div().w_full().h(px(4.)).rounded_full().bg(content(0.12)))
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .top(px(8.))
                            .h(px(4.))
                            .w(relative(fraction))
                            .rounded_full()
                            .bg(accent_color()),
                    )
                    .child(
                        div()
                            .absolute()
                            .top(px(4.))
                            .left(relative(fraction))
                            .ml(px(-6.))
                            .size(px(12.))
                            .rounded_full()
                            .bg(accent_color()),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                            this.slider_drag = Some(SliderDrag {
                                key: key.clone(),
                                handler: handler.clone(),
                                min,
                                max,
                                step,
                            });
                            this.slide(event.position.x, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .on_key_down(
                        cx.listener(move |this, event: &gpui::KeyDownEvent, _, cx| {
                            let next = match event.keystroke.key.as_str() {
                                "left" | "down" => current - step,
                                "right" | "up" => current + step,
                                "home" => min,
                                "end" => max,
                                _ => return,
                            };
                            this.dispatch(&keyboard, json!(next.clamp(min, max)), cx);
                            cx.stop_propagation();
                        }),
                    ),
            )
            .into_any_element()
    }
    #[inline(never)]
    fn render_page_tabs(&mut self, node: &Node, key: String, cx: &mut Context<Self>) -> AnyElement {
        let Node::PageTabs {
            labels,
            selected,
            handler,
            nav_width,
            right_margin,
            children,
        } = node
        else {
            return div().into_any_element();
        };
        // Keep the rail usable in narrow windows without letting it
        // take over the page content.
        let rail_width = (*nav_width)
            .min((self.width.get() * 0.32).max(140.))
            .max(140.);
        let mut rail = div()
            .id(SharedString::from(format!("visual-page-tabs-{key}")))
            .role(Role::TabList)
            .w(px(rail_width))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(1.))
            .pt(px(1.));
        for (i, label) in labels.iter().enumerate() {
            let active = i == *selected;
            let handler = handler.clone();
            let keyboard = handler.clone();
            rail = rail.child(
                div()
                    .id(SharedString::from(format!("visual-page-tab-{key}-{i}")))
                    .debug_selector({
                        let key = key.clone();
                        move || format!("visual-page-tab-{key}-{i}")
                    })
                    .role(Role::Tab)
                    .focusable()
                    .tab_stop(true)
                    .aria_label(label.clone())
                    .aria_selected(active)
                    .h(px(30.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .border_l_2()
                    .border_color(if active {
                        accent_color()
                    } else {
                        gpui::transparent_black()
                    })
                    .text_size(px(13.))
                    .line_height(px(18.))
                    .text_color(if active { content(1.) } else { content(0.55) })
                    .when(active, |d| d.font_weight(FontWeight::MEDIUM))
                    .cursor_pointer()
                    .hover(|s| s.text_color(content(1.)))
                    .focus_visible(|s| s.bg(content(0.05)))
                    .child(label.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.dispatch(&handler, json!(i), cx)
                    }))
                    .on_key_down(cx.listener(move |this, event, _, cx| {
                        if activated(event) {
                            this.dispatch(&keyboard, json!(i), cx);
                            cx.stop_propagation();
                        }
                    })),
            );
        }
        let content = children
            .first()
            .map(|child| self.node(child, format!("{key}/0"), cx))
            .unwrap_or_else(|| div().into_any_element());
        // Center the reading column. The matching spacer on the right
        // makes the column itself centered while the TOC sits beside it.
        let gap = 24.;
        let group_width = (rail_width * 2.) + (gap * 2.) + PAGE_CONTENT_MAX_WIDTH;
        let panel_max_width = if *right_margin {
            PAGE_CONTENT_MAX_WIDTH + gap + rail_width
        } else {
            PAGE_CONTENT_MAX_WIDTH
        };
        let mut page_layout = div()
            .id(SharedString::from(format!("visual-page-layout-{key}")))
            .debug_selector({
                let key = key.clone();
                move || format!("visual-page-layout-{key}")
            })
            .w_full()
            .min_w_0()
            .max_w(px(group_width))
            .flex()
            .items_start()
            .gap(px(gap))
            .child(rail)
            .child(
                div()
                    .id(SharedString::from(format!("visual-page-panel-{key}")))
                    .debug_selector({
                        let key = key.clone();
                        move || format!("visual-page-panel-{key}")
                    })
                    .role(Role::TabPanel)
                    .flex_1()
                    .min_w_0()
                    .max_w(px(panel_max_width))
                    .child(content),
            );
        if !*right_margin {
            page_layout = page_layout.child(div().w(px(rail_width)).flex_none());
        }
        div()
            .w_full()
            .min_w_0()
            .flex()
            .justify_center()
            .child(page_layout)
            .into_any_element()
    }
    #[inline(never)]
    fn render_input(&mut self, node: &Node, key: String, cx: &mut Context<Self>) -> AnyElement {
        let Node::Input {
            label,
            value,
            placeholder,
            handler,
            submit_handler,
        } = node
        else {
            return div().into_any_element();
        };
        let placeholder_text = placeholder.clone().unwrap_or_default();
        if !self.inputs.contains_key(&key) {
            let input = cx.new(|cx| {
                let mut input = crate::input::Input::field(
                    label.clone(),
                    placeholder_text.clone(),
                    content(0.4),
                    cx,
                );
                input.set_font_size(12.5);
                input.set_max_chars(4096);
                input.set_text(value.clone(), cx);
                input
            });
            let changed_key = key.clone();
            let changed_sub =
                cx.subscribe(&input, move |this, input, _: &crate::input::Changed, cx| {
                    let value = input.read(cx).content.to_string();
                    let is_remote = this.remote.is_some();
                    let Some(slot) = this.inputs.get_mut(&changed_key) else {
                        return;
                    };
                    if value == slot.source_value {
                        return;
                    }
                    slot.source_value = value.clone();
                    if is_remote {
                        if slot.pending.len() >= 64 {
                            slot.pending.pop_front();
                        }
                        slot.pending.push_back(value.clone());
                    }
                    let handler = slot.handler.clone();
                    this.dispatch_raw(&handler, json!(value), cx);
                });
            let submit_key = key.clone();
            let submit_sub = cx.subscribe(
                &input,
                move |this, _, _: &crate::input::Submitted, cx| {
                    let Some(handler) = this
                        .inputs
                        .get(&submit_key)
                        .and_then(|slot| slot.submit_handler.clone())
                    else {
                        return;
                    };
                    this.dispatch(&handler, Value::Null, cx);
                },
            );
            self.inputs.insert(
                key.clone(),
                InputSlot {
                    input,
                    handler: handler.clone(),
                    submit_handler: submit_handler.clone(),
                    source_value: value.clone(),
                    last_remote: value.clone(),
                    pending: VecDeque::new(),
                    _subscriptions: vec![changed_sub, submit_sub],
                },
            );
        }
        let slot = self.inputs.get_mut(&key).unwrap();
        slot.handler = handler.clone();
        slot.submit_handler = submit_handler.clone();
        slot.input
            .update(cx, |input, cx| input.set_placeholder(placeholder_text, cx));
        let current = slot.input.read(cx).content.to_string();
        if accept_remote_text(&mut slot.pending, &mut slot.last_remote, &current, value) {
            slot.source_value = value.clone();
            slot.input
                .update(cx, |input, cx| input.set_text(value.clone(), cx));
        }
        // The placeholder says what to type; the label stays for
        // assistive technology only.
        div()
            .id(SharedString::from(format!("visual-input-{key}")))
            .debug_selector({
                let key = key.clone();
                move || format!("visual-input-{key}")
            })
            .aria_label(label.clone())
            .min_w_0()
            .h(px(26.))
            .px_2()
            .flex()
            .items_center()
            .rounded_md()
            .border_1()
            .border_color(content(0.14))
            .child(slot.input.clone())
            .into_any_element()
    }
    #[inline(never)]
    fn render_code_editor(
        &mut self,
        node: &Node,
        key: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Node::CodeEditor {
            label,
            value,
            language,
            height,
            handler,
            read_only,
            submit_handler,
            selection_handler,
            context_menu,
        } = node
        else {
            return div().into_any_element();
        };
        if !self.editors.contains_key(&key) {
            let path = editor_path(language);
            let syntax = crate::editor_syntax::supports(&path).then(|| Arc::new(Mutex::new(None)));
            let input = cx.new(|cx| {
                let mut input = crate::input::Input::editor(cx);
                input.set_editor_path(path.clone());
                input.set_max_chars(64 * 1024);
                input.set_read_only(*read_only);
                input.set_min_height((*height - 2.).max(20.));
                input.set_max_height((*height - 2.).max(20.));
                input.set_text(value.clone(), cx);
                input
            });
            let changed_key = key.clone();
            let changed_sub =
                cx.subscribe(&input, move |this, input, _: &crate::input::Changed, cx| {
                    let value = input.read(cx).content.to_string();
                    let is_remote = this.remote.is_some();
                    let handler = {
                        let Some(slot) = this.editors.get_mut(&changed_key) else {
                            return;
                        };
                        if value == slot.source_value {
                            this.sync_editor_selection(&changed_key, cx);
                            return;
                        }
                        slot.source_value = value.clone();
                        if is_remote {
                            if slot.pending.len() >= 64 {
                                slot.pending.pop_front();
                            }
                            slot.pending.push_back(value.clone());
                        }
                        slot.handler.clone()
                    };
                    this.dispatch_raw(&handler, json!(value.clone()), cx);
                    this.analyze_editor(&changed_key, value, cx);
                    this.sync_editor_selection(&changed_key, cx);
                });
            let selection_key = key.clone();
            let selection_sub = cx.subscribe(
                &input,
                move |this, _, _: &crate::input::SelectionChanged, cx| {
                    this.sync_editor_selection(&selection_key, cx);
                },
            );
            let submit_key = key.clone();
            let submit_sub = cx.subscribe(
                &input,
                move |this, input, _: &crate::input::Submitted, cx| {
                    let Some(handler) = this
                        .editors
                        .get(&submit_key)
                        .and_then(|slot| slot.submit_handler.clone())
                    else {
                        return;
                    };
                    let selected = input.read(cx).selected_text();
                    let payload = if selected.trim().is_empty() {
                        Value::Null
                    } else {
                        json!(selected)
                    };
                    this.dispatch(&handler, payload, cx);
                },
            );
            self.editors.insert(
                key.clone(),
                CodeEditorSlot {
                    input: input.clone(),
                    handler: handler.clone(),
                    submit_handler: submit_handler.clone(),
                    selection_handler: selection_handler.clone(),
                    context_menu: context_menu.clone(),
                    source_value: value.clone(),
                    last_remote: value.clone(),
                    last_selection: String::new(),
                    pending: VecDeque::new(),
                    height: *height,
                    read_only: *read_only,
                    path: path.clone(),
                    syntax,
                    _analysis: None,
                    _subscriptions: vec![changed_sub, selection_sub, submit_sub],
                },
            );
            self.analyze_editor(&key, value.clone(), cx);
        }
        let mut changed = false;
        let new_path = editor_path(language);
        {
            let Some(slot) = self.editors.get_mut(&key) else {
                return div().into_any_element();
            };
            slot.handler = handler.clone();
            slot.submit_handler = submit_handler.clone();
            slot.selection_handler = selection_handler.clone();
            slot.context_menu = context_menu.clone();
            if (slot.height - *height).abs() > 0.5 || slot.read_only != *read_only {
                slot.height = *height;
                slot.read_only = *read_only;
                let inner = (*height - 2.).max(20.);
                slot.input.update(cx, |input, cx| {
                    input.set_read_only(*read_only);
                    input.set_min_height(inner);
                    input.set_max_height(inner);
                    cx.notify();
                });
            }
            if slot.path != new_path {
                slot.path = new_path.clone();
                slot.syntax =
                    crate::editor_syntax::supports(&new_path).then(|| Arc::new(Mutex::new(None)));
                slot.input
                    .update(cx, |input, _| input.set_editor_path(new_path.clone()));
                changed = true;
            }
            let current = slot.input.read(cx).content.to_string();
            if accept_remote_text(&mut slot.pending, &mut slot.last_remote, &current, value) {
                slot.source_value = value.clone();
                slot.input
                    .update(cx, |input, cx| input.set_text(value.clone(), cx));
                changed = true;
            }
        }
        if changed {
            self.analyze_editor(&key, value.clone(), cx);
        }
        let input = self.editors[&key].input.clone();
        let has_menu = !self.editors[&key].context_menu.is_empty();
        div()
            .id(SharedString::from(format!("visual-code-editor-{key}")))
            .debug_selector({
                let key = key.clone();
                move || format!("visual-code-editor-{key}")
            })
            .role(Role::Group)
            .aria_label(label.clone())
            .w_full()
            .min_w_0()
            .h(px(*height))
            .overflow_hidden()
            .rounded_md()
            .border_1()
            .border_color(content(0.14))
            .when(has_menu, |el| {
                let menu_key = key.clone();
                el.on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                        this.open_editor_menu(&menu_key, event.position, window, cx);
                        cx.stop_propagation();
                    }),
                )
            })
            .child(input)
            .into_any_element()
    }
    fn scene(
        &mut self,
        key: String,
        height: f32,
        objects: Vec<SceneObject>,
        camera: Camera,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let orbit = *self.orbits.entry(key.clone()).or_default();
        let wheel_key = key.clone();
        let reset_key = key.clone();
        let keyboard_key = key.clone();
        let reset_keyboard = key.clone();
        let cache = self.scene_cache.entry(key.clone()).or_default().clone();
        let revision = self.revision;
        div()
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .child(
                div()
                    .id(SharedString::from(format!("visual-scene-{key}")))
                    .debug_selector({
                        let key = key.clone();
                        move || format!("visual-scene-{key}")
                    })
                    .w_full()
                    .focusable()
                    .tab_stop(true)
                    .aria_label(
                        "3D scene; arrow keys orbit, plus and minus zoom, zero resets camera",
                    )
                    .h(px(height))
                    .overflow_hidden()
                    .rounded_lg()
                    .border_1()
                    .border_color(content(0.08))
                    .cursor(gpui::CursorStyle::OpenHand)
                    .child(
                        canvas(
                            |_, _, _| {},
                            move |bounds, _, window, _| {
                                let dimensions =
                                    [bounds.size.width.as_f32(), bounds.size.height.as_f32()];
                                let mut cached = cache.borrow_mut();
                                if cached.as_ref().is_none_or(|c| {
                                    c.revision != revision
                                        || c.dimensions != dimensions
                                        || c.orbit != orbit
                                }) {
                                    *cached = Some(SceneCache {
                                        revision,
                                        dimensions,
                                        orbit,
                                        projection: Rc::new(scene::project(
                                            &objects,
                                            &camera,
                                            orbit,
                                            dimensions[0],
                                            dimensions[1],
                                        )),
                                    });
                                }
                                let projection = cached.as_ref().unwrap().projection.clone();
                                drop(cached);
                                window.with_content_mask(
                                    Some(gpui::ContentMask { bounds }),
                                    |window| {
                                        for surface in &projection.order {
                                            match surface {
                                                scene::Surface::Face(index) => {
                                                    let face = &projection.faces[*index];
                                                    let mut fill = color(&face.color);
                                                    fill.l *= face.light;
                                                    paint_path(
                                                        &face.points,
                                                        bounds.origin,
                                                        1.,
                                                        0.,
                                                        true,
                                                        fill,
                                                        window,
                                                    );
                                                }
                                                scene::Surface::Stroke(index) => {
                                                    let stroke = &projection.strokes[*index];
                                                    paint_path(
                                                        &stroke.points,
                                                        bounds.origin,
                                                        1.,
                                                        stroke.width,
                                                        false,
                                                        color(&stroke.color),
                                                        window,
                                                    );
                                                }
                                            }
                                        }
                                    },
                                );
                            },
                        )
                        .size_full(),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                            this.scene_drag = Some((key.clone(), event.position));
                            cx.stop_propagation();
                        }),
                    )
                    .on_scroll_wheel(cx.listener(
                        move |this, event: &gpui::ScrollWheelEvent, _, cx| {
                            // Modifier zoom leaves ordinary transcript scrolling available.
                            if event.modifiers.control || event.modifiers.platform {
                                let orbit = this.orbits.entry(wheel_key.clone()).or_default();
                                orbit.zoom = (orbit.zoom
                                    * (event.delta.pixel_delta(px(16.)).y.as_f32() * 0.008).exp())
                                .clamp(0.15, 8.);
                                cx.notify();
                                cx.stop_propagation();
                            }
                        },
                    ))
                    .on_key_down(cx.listener(move |this, event: &gpui::KeyDownEvent, _, cx| {
                        let orbit = this.orbits.entry(keyboard_key.clone()).or_default();
                        match event.keystroke.key.as_str() {
                            "left" => orbit.yaw -= 0.12,
                            "right" => orbit.yaw += 0.12,
                            "up" => orbit.pitch = (orbit.pitch - 0.12).max(-1.45),
                            "down" => orbit.pitch = (orbit.pitch + 0.12).min(1.45),
                            "+" | "=" => orbit.zoom = (orbit.zoom / 1.2).max(0.15),
                            "-" => orbit.zoom = (orbit.zoom * 1.2).min(8.),
                            "0" => *orbit = scene::Orbit::default(),
                            _ => return,
                        }
                        cx.notify();
                        cx.stop_propagation();
                    })),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(11.))
                    .text_color(content(0.45))
                    .child(
                        div()
                            .flex_1()
                            .child("Drag to orbit · ⌘/Ctrl + scroll to zoom"),
                    )
                    .child(
                        button(SharedString::from(format!(
                            "visual-camera-reset-{reset_key}"
                        )))
                        .aria_label("Reset scene camera")
                        .child("Reset camera")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.orbits.remove(&reset_key);
                            cx.notify();
                        }))
                        .on_key_down(cx.listener(
                            move |this, event, _, cx| {
                                if activated(event) {
                                    this.orbits.remove(&reset_keyboard);
                                    cx.notify();
                                    cx.stop_propagation();
                                }
                            },
                        )),
                    ),
            )
            .into_any_element()
    }
}

impl Render for VisualView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self.document.title.clone();
        let tree = self.tree.clone();
        let width = self.width.clone();
        let entity = cx.entity().downgrade();
        self.active.clear();
        let body = if let Some(tree) = tree {
            self.node(&tree, "root".into(), cx)
        } else {
            div().into_any_element()
        };
        self.inputs.retain(|key, _| self.active.contains(key));
        self.editors.retain(|key, _| self.active.contains(key));
        if self
            .editor_menu
            .as_ref()
            .is_some_and(|open| !self.active.contains(&open.editor_key))
        {
            self.editor_menu = None;
        }
        self.tracks.retain(|key, _| self.active.contains(key));
        self.orbits.retain(|key, _| self.active.contains(key));
        self.scene_cache.retain(|key, _| self.active.contains(key));
        let popup = self.editor_menu.as_ref().map(|open| {
            crate::popover::menu_popover(&open.menu, crate::popover::Anchor::Point(open.anchor), cx)
                .gap(0.)
        });
        let inline = self.inline.clone();
        // A visual in its own row is titled; placed in prose, it is captioned.
        let header = inline.is_none().then(|| {
            div()
                .min_w_0()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title)
        });
        div()
            .id("native-session-visual")
            .debug_selector(|| "native-session-visual".into())
            .relative()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_3()
            .text_size(px(13.))
            .text_color(content(1.))
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let old = (width.get() / 180.).floor();
                        width.set(bounds.size.width.as_f32());
                        if old != (width.get() / 180.).floor() {
                            let _ = entity.update(cx, |this, cx| this.changed(cx));
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .w_full()
                .h(px(1.)),
            )
            .children(header)
            .children(self.error.iter().map(|error| {
                div()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .border_1()
                    .border_color(color(&ThemeColor::Negative))
                    .text_size(px(12.))
                    .text_color(color(&ThemeColor::Negative))
                    .child(format!("Visual could not update: {error}"))
            }))
            .child(
                div()
                    .id("visual-content")
                    .debug_selector(|| "visual-content".into())
                    .w_full()
                    .min_w_0()
                    .flex_shrink_0()
                    .child(body),
            )
            .children(inline.as_ref().filter(|c| !c.is_empty()).map(|caption| {
                div()
                    .text_size(px(12.))
                    .text_color(content(0.5))
                    .child(caption.clone())
            }))
            .children(popup)
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                if !event.dragging() {
                    this.slider_drag = None;
                    this.scene_drag = None;
                    return;
                }
                if this.slider_drag.is_some() {
                    this.slide(event.position.x, cx);
                    cx.stop_propagation();
                }
                if let Some((key, last)) = &mut this.scene_drag {
                    let delta = event.position - *last;
                    *last = event.position;
                    let orbit = this.orbits.entry(key.clone()).or_default();
                    orbit.yaw += delta.x.as_f32() * 0.01;
                    orbit.pitch = (orbit.pitch + delta.y.as_f32() * 0.01).clamp(-1.45, 1.45);
                    cx.notify();
                    cx.stop_propagation();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.slider_drag = None;
                    this.scene_drag = None;
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.slider_drag = None;
                    this.scene_drag = None;
                }),
            )
    }
}

fn accept_remote_text(
    pending: &mut VecDeque<String>,
    last_remote: &mut String,
    current: &str,
    incoming: &str,
) -> bool {
    if let Some(pos) = pending.iter().position(|sent| sent == incoming) {
        *last_remote = incoming.to_owned();
        pending.drain(0..=pos);
        return false;
    }
    if !pending.is_empty() && incoming == last_remote {
        return false;
    }
    *last_remote = incoming.to_owned();
    pending.clear();
    current != incoming
}

fn editor_path(language: &str) -> String {
    let extension = match language.trim().to_ascii_lowercase().as_str() {
        "js" | "javascript" | "node" => "js",
        "jsx" => "jsx",
        "ts" | "typescript" => "ts",
        "tsx" => "tsx",
        "py" | "python" => "py",
        "rs" | "rust" => "rs",
        "go" | "golang" => "go",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "html" => "html",
        "css" => "css",
        "sh" | "bash" | "shell" => "sh",
        "c" => "c",
        "cpp" | "c++" => "cpp",
        "java" => "java",
        "rb" | "ruby" => "rb",
        _ => "txt",
    };
    format!("snippet.{extension}")
}

fn node_at<'a>(mut node: &'a Node, key: &str) -> Option<&'a Node> {
    let mut parts = key.split('/');
    if parts.next() != Some("root") {
        return None;
    }
    for part in parts {
        let index = part.parse::<usize>().ok()?;
        node = match node {
            Node::Column { children, .. }
            | Node::Row { children, .. }
            | Node::Grid { children, .. }
            | Node::Card { children, .. }
            | Node::PageTabs { children, .. } => children.get(index)?,
            _ => return None,
        };
    }
    Some(node)
}

fn format_number(value: f64) -> String {
    if value.abs() >= 1_000_000. {
        format!("{:.1}m", value / 1_000_000.)
    } else if value.abs() >= 10_000. {
        format!("{:.1}k", value / 1_000.)
    } else if value.fract().abs() < 0.00001 {
        format!("{value:.0}")
    } else {
        format!("{value:.2}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_owned()
    }
}
fn paint_path(
    points: &[[f32; 2]],
    origin: Point<Pixels>,
    scale: f32,
    width: f32,
    closed: bool,
    color: Hsla,
    window: &mut Window,
) {
    if points.is_empty() {
        return;
    }
    let mut path = if closed {
        PathBuilder::fill()
    } else {
        PathBuilder::stroke(px(width))
    };
    for (i, p) in points.iter().enumerate() {
        let p = origin + point(px(p[0] * scale), px(p[1] * scale));
        if i == 0 {
            path.move_to(p);
        } else {
            path.line_to(p);
        }
    }
    if closed {
        path.close();
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}
fn paint_label(
    text: String,
    at: Point<Pixels>,
    token: &ThemeColor,
    font_size: f32,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    let style = window.text_style();
    let run = gpui::TextRun {
        len: text.len(),
        font: style.font(),
        color: color(token),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window
        .text_system()
        .shape_line(text.into(), px(font_size), &[run], None);
    let _ = line.paint(
        at,
        px(font_size * 1.4),
        gpui::TextAlign::Left,
        None,
        window,
        cx,
    );
}
fn drawing(width: f32, height: f32, shapes: Vec<Shape>) -> AnyElement {
    div()
        .w_full()
        .aspect_ratio(width / height)
        .max_h(px(height))
        .overflow_hidden()
        .child(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, cx| {
                    let scale = (bounds.size.width.as_f32() / width)
                        .min(bounds.size.height.as_f32() / height);
                    let origin = bounds.origin
                        + point(
                            px((bounds.size.width.as_f32() - width * scale) / 2.),
                            px(0.),
                        );
                    window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
                        for shape in &shapes {
                            match shape {
                                Shape::Rect {
                                    x,
                                    y,
                                    width,
                                    height,
                                    color: token,
                                } => window.paint_quad(fill(
                                    Bounds::new(
                                        origin + point(px(x * scale), px(y * scale)),
                                        size(px(width * scale), px(height * scale)),
                                    ),
                                    color(token),
                                )),
                                Shape::Circle {
                                    x,
                                    y,
                                    radius,
                                    color: token,
                                } => {
                                    let points = (0..48)
                                        .map(|i| {
                                            let a = i as f32 * std::f32::consts::TAU / 48.;
                                            [x + radius * a.cos(), y + radius * a.sin()]
                                        })
                                        .collect::<Vec<_>>();
                                    paint_path(
                                        &points,
                                        origin,
                                        scale,
                                        0.,
                                        true,
                                        color(token),
                                        window,
                                    );
                                }
                                Shape::Line {
                                    x1,
                                    y1,
                                    x2,
                                    y2,
                                    width,
                                    color: token,
                                } => paint_path(
                                    &[[*x1, *y1], [*x2, *y2]],
                                    origin,
                                    scale,
                                    width * scale,
                                    false,
                                    color(token),
                                    window,
                                ),
                                Shape::Polyline {
                                    points,
                                    width,
                                    color: token,
                                } => paint_path(
                                    points,
                                    origin,
                                    scale,
                                    width * scale,
                                    false,
                                    color(token),
                                    window,
                                ),
                                Shape::Label {
                                    x,
                                    y,
                                    text,
                                    color: token,
                                } => paint_label(
                                    text.clone(),
                                    origin + point(px(x * scale), px(y * scale)),
                                    token,
                                    12. * scale.max(0.7),
                                    window,
                                    cx,
                                ),
                            }
                        }
                    });
                },
            )
            .size_full(),
        )
        .into_any_element()
}
