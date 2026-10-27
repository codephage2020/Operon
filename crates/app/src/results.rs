//! The floating result window.
//!
//! Kept separate from the main window so the reading flow never depends on the
//! workbench being visible: a screenshot task can finish while the user is in
//! another app, and the translation still appears above whatever they are doing.

use gpui::{
    App, ClipboardItem, Context, FocusHandle, Focusable, MouseButton, SharedString, Window, actions,
    div, prelude::*, px, rgb,
};

actions!(
    operon_results,
    [CloseResults, CopyResultOriginal, CopyResultTranslation]
);

/// Shared with the main app; both views read the same text so copying and
/// retrying behave identically wherever the user is looking.
#[derive(Clone, Debug, Default)]
pub struct ResultState {
    pub original: String,
    pub translated: String,
    pub service: String,
    pub target_language: String,
    pub failure: Option<String>,
    pub can_retry: bool,
    pub running: bool,
}

pub struct ResultView {
    state: ResultState,
    focus_handle: FocusHandle,
}

impl ResultView {
    pub fn new(state: ResultState, cx: &mut Context<Self>) -> Self {
        Self {
            state,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn set_state(&mut self, state: ResultState, cx: &mut Context<Self>) {
        self.state = state;
        cx.notify();
    }

    fn copy(&self, text: String, cx: &mut App) {
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn button(&self, label: &str, id: &'static str) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .px_3()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .text_sm()
            .bg(rgb(0xecf4f0))
            .text_color(rgb(0x12584c))
            .hover(|style| style.bg(rgb(0xdcefe8)))
            .child(label.to_string())
    }

    fn text_section(&self, title: &str, body: &str, empty: &str, color: u32) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(0x7b8a83))
                    .child(title.to_string()),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(color))
                    .child(if body.is_empty() {
                        empty.to_string()
                    } else {
                        body.to_string()
                    }),
            )
    }
}

impl Focusable for ResultView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ResultView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Long text scrolls rather than growing past the screen edge.
        div()
            .id("result-window")
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .bg(rgb(0xffffff))
            .text_color(rgb(0x20302a))
            .font_family(".AppleSystemUIFont")
            .rounded_lg()
            .border_1()
            .border_color(rgb(0xd6dfdb))
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &CloseResults, window, _| window.remove_window()))
            .on_action(cx.listener(|this, _: &CopyResultOriginal, _, cx| {
                this.copy(this.state.original.clone(), cx)
            }))
            .on_action(cx.listener(|this, _: &CopyResultTranslation, _, cx| {
                this.copy(this.state.translated.clone(), cx)
            }))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(if self.state.running {
                                "处理中…".to_string()
                            } else {
                                "结果".to_string()
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(0x63746c))
                                    .child(format!(
                                        "{} → {}",
                                        self.state.service, self.state.target_language
                                    )),
                            )
                            .child(
                                self.button("关闭", "result-close").on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|_, _, window, _| window.remove_window()),
                                ),
                            ),
                    ),
            )
            .child(
                div()
                    .id("result-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(self.text_section(
                        "原文",
                        &self.state.original,
                        "识别原文会显示在这里",
                        0x33413d,
                    ))
                    .child(self.text_section(
                        "译文",
                        &self.state.translated,
                        "译文会显示在这里",
                        0x176b5b,
                    ))
                    .when_some(self.state.failure.clone(), |this, error| {
                        this.child(div().text_sm().text_color(rgb(0xb14232)).child(error))
                    }),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(self.button("复制原文", "result-copy-original").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.copy(this.state.original.clone(), cx)
                        }),
                    ))
                    .child(
                        self.button("复制译文", "result-copy-translation")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    this.copy(this.state.translated.clone(), cx)
                                }),
                            ),
                    ),
            )
    }
}

/// Open (or refresh) the floating result window. Reusing one window keeps a
/// burst of retries from stacking overlapping panels.
pub fn show(
    state: ResultState,
    cx: &mut App,
    existing: Option<gpui::WindowHandle<ResultView>>,
) -> gpui::WindowHandle<ResultView> {
    if let Some(handle) = existing {
        let updated = handle.update(cx, |view, _window, cx| view.set_state(state.clone(), cx));
        if updated.is_ok() {
            return handle;
        }
    }
    let bounds = gpui::Bounds::centered(
        None,
        gpui::size(px(420.), px(320.)),
        cx,
    );
    cx.open_window(
        gpui::WindowOptions {
            window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some(SharedString::from("Operon 结果")),
                appears_transparent: true,
                traffic_light_position: None,
            }),
            kind: gpui::WindowKind::PopUp,
            focus: false,
            show: true,
            is_movable: true,
            is_resizable: true,
            ..Default::default()
        },
        move |_window, cx| cx.new(|cx| ResultView::new(state.clone(), cx)),
    )
    .expect("open result window")
}
