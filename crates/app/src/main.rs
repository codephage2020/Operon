mod input;
mod tasks;

use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, FocusHandle, Focusable, KeyBinding, MouseButton,
    SharedString, Window, WindowBounds, WindowOptions, actions, div, prelude::*, px, rgb, rgba,
    size,
};
use input::{TextInput, TextInputEvent};
use operon_core::*;
use tasks::TaskController;

enum Retry {
    Translation(RunOutput),
    Task(String),
}

actions!(
    operon,
    [
        Quit,
        Cancel,
        CopyOriginal,
        CopyTranslation,
        OpenScreenshot,
        OpenTranslation,
        OpenSpeech
    ]
);

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Tasks,
    Capabilities,
    Flows,
    Runs,
}

struct OperonApp {
    page: Page,
    input: gpui::Entity<TextInput>,
    focus_handle: FocusHandle,
    controller: TaskController,
    status: String,
    original: String,
    translated: String,
    service: String,
    target_language: String,
    running: bool,
    cancellation: Option<CancellationToken>,
    run_steps: Vec<StepRecord>,
    last_failure: Option<String>,
    speech_updates: Vec<String>,
    speech_interim: String,
    retry: Option<Retry>,
    needs_screen_permission: bool,
}

impl OperonApp {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new(cx, "输入要翻译的文本…", true));
        cx.subscribe(&input, |this, _, event: &TextInputEvent, cx| {
            if let TextInputEvent::Changed(value) = event {
                this.original = value.clone();
                cx.notify();
            }
        })
        .detach();
        let focus_handle = cx.focus_handle();
        window.focus(&input.focus_handle(cx));
        Self {
            page: Page::Tasks,
            input,
            focus_handle,
            controller: TaskController::new(),
            status: "就绪 · OCR 在本地完成".into(),
            original: String::new(),
            translated: String::new(),
            service: "MyMemory · 在线".into(),
            target_language: "zh-CN".into(),
            running: false,
            cancellation: None,
            run_steps: Vec::new(),
            last_failure: None,
            speech_updates: Vec::new(),
            speech_interim: String::new(),
            retry: None,
            needs_screen_permission: false,
        }
    }

    fn start(&mut self, task: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.running {
            self.status = "已有任务正在运行，请先取消或等待完成".into();
            cx.notify();
            return;
        }
        if task == "text" && self.original.trim().is_empty() {
            self.status = "请先输入要翻译的文本".into();
            window.focus(&self.input.focus_handle(cx));
            cx.notify();
            return;
        }
        if matches!(task, "screenshot" | "ocr") && !operon_platform::screen_recording_granted() {
            self.status = "缺少屏幕录制权限，截图无法开始".into();
            self.needs_screen_permission = true;
            cx.notify();
            return;
        }
        self.running = true;
        self.status = match task {
            "ocr" => "请框选屏幕区域 · Esc 取消".into(),
            "speech" => "正在请求麦克风与语音识别权限…".into(),
            _ => "任务处理中…".into(),
        };
        self.last_failure = None;
        self.translated.clear();
        self.run_steps.clear();
        self.cancellation = Some(CancellationToken::default());
        self.controller.start(
            task.into(),
            self.original.clone(),
            self.target_language.clone(),
            self.cancellation.clone().unwrap(),
        );
        cx.notify();
    }

    fn receive(&mut self, event: tasks::TaskEvent, cx: &mut Context<Self>) {
        match event {
            tasks::TaskEvent::Steps(steps) => self.run_steps = steps,
            tasks::TaskEvent::Original(text) => {
                self.original = text.clone();
                self.input
                    .update(cx, |input, cx| input.set_content(text, cx));
            }
            tasks::TaskEvent::Finished {
                original,
                translated,
                error,
            } => {
                operon_platform::cleanup_captures();
                self.original = original;
                self.translated = translated;
                self.running = false;
                self.cancellation = None;
                self.status = if error.is_some() {
                    "任务未完成 · 已完成的结果已保留".into()
                } else {
                    "任务完成".into()
                };
                if error.is_none() {
                    self.retry = None;
                }
                self.last_failure = error;
            }
            tasks::TaskEvent::Failed {
                output,
                task,
                message,
            } => {
                operon_platform::cleanup_captures();
                self.original = output
                    .outputs
                    .iter()
                    .find(|item| item.step_id == "ocr")
                    .and_then(|item| item.payload.text())
                    .map(|item| item.text.clone())
                    .unwrap_or_else(|| self.original.clone());
                self.translated.clear();
                self.run_steps = output.record.steps.clone();
                let failed_at_translate = output
                    .record
                    .steps
                    .iter()
                    .rev()
                    .find(|step| step.status == StepStatus::Failed)
                    .is_some_and(|step| step.step_id == "translate");
                self.retry = if task == "screenshot" && failed_at_translate {
                    Some(Retry::Translation(output))
                } else {
                    Some(Retry::Task(task))
                };
                self.running = false;
                self.cancellation = None;
                self.status = "任务失败 · 可从失败步骤重试".into();
                self.last_failure = Some(message);
            }
            tasks::TaskEvent::Speech {
                interim,
                translated,
                error,
            } => {
                self.speech_interim = interim;
                if let Some(text) = translated {
                    self.speech_updates.push(text);
                }
                self.last_failure = error;
            }
        }
        cx.notify();
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(cancellation) = &self.cancellation {
            cancellation.cancel();
            self.status = "正在取消…".into();
        }
        cx.notify();
    }

    fn copy(&self, text: String, cx: &mut App) {
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn retry_failed(&mut self, cx: &mut Context<Self>) {
        let Some(retry) = self.retry.take() else {
            return;
        };
        self.running = true;
        self.status = "正在重试失败步骤…".into();
        self.last_failure = None;
        self.run_steps.clear();
        self.cancellation = Some(CancellationToken::default());
        match retry {
            Retry::Translation(output) => self.controller.retry(
                "screenshot".into(),
                output,
                self.target_language.clone(),
                self.cancellation.clone().unwrap(),
            ),
            Retry::Task(task) => self.controller.start(
                task,
                self.original.clone(),
                self.target_language.clone(),
                self.cancellation.clone().unwrap(),
            ),
        }
        cx.notify();
    }

    fn check_screen_permission(&mut self, cx: &mut Context<Self>) {
        if operon_platform::screen_recording_granted() {
            self.needs_screen_permission = false;
            self.status = "权限已确认，可以开始截图任务".into();
        } else {
            self.status = "仍未检测到屏幕录制权限，授予后需要重新检查".into();
        }
        cx.notify();
    }

    fn select_language(&mut self, language: &str, cx: &mut Context<Self>) {
        self.target_language = language.into();
        cx.notify();
    }

    fn action(&self, label: &str, id: &'static str, primary: bool) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .px_4()
            .py_2()
            .rounded_md()
            .cursor_pointer()
            .text_sm()
            .font_weight(gpui::FontWeight::MEDIUM)
            .bg(if primary {
                rgb(0x176b5b)
            } else {
                rgb(0xffffff)
            })
            .text_color(if primary {
                rgb(0xffffff)
            } else {
                rgb(0x33413d)
            })
            .border_1()
            .border_color(if primary {
                rgb(0x176b5b)
            } else {
                rgb(0xd6dfdb)
            })
            .hover(|style| {
                style.bg(if primary {
                    rgb(0x12584c)
                } else {
                    rgb(0xecf4f0)
                })
            })
            .child(label.to_string())
    }

    fn task_card(
        &self,
        id: &'static str,
        title: &str,
        description: &str,
        shortcut: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let task = if id == "ocr" { "ocr" } else { "screenshot" };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .p_5()
            .bg(rgb(0xffffff))
            .rounded_xl()
            .border_1()
            .border_color(rgb(0xe0e7e3))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child(title.to_string()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x7b8a83))
                            .child(shortcut.to_string()),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(0x63746c))
                    .child(description.to_string()),
            )
            .child(self.action("开始任务", id, true).on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| this.start(task, window, cx)),
            ))
    }
}

impl Focusable for OperonApp {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for OperonApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let navigation = [
            (Page::Tasks, "tasks", "任务"),
            (Page::Capabilities, "capabilities", "能力"),
            (Page::Flows, "flows", "流程"),
            (Page::Runs, "runs", "运行详情"),
        ];
        let content: AnyElement = match self.page {
            Page::Tasks => div()
                .flex()
                .flex_col()
                .gap_5()
                .child(div().text_2xl().font_weight(gpui::FontWeight::SEMIBOLD).child("今天想处理什么？"))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(0x63746c))
                        .child("独立能力，自由接力。截图与 OCR 留在本地，翻译服务清晰可见。"),
                )
                .child(
                    div()
                        .flex()
                        .gap_4()
                        .child(self.task_card(
                            "translate",
                            "截图翻译",
                            "框选屏幕文字，识别并翻译为目标语言。",
                            "⌃⌥ T",
                            cx,
                        ))
                        .child(self.task_card(
                            "ocr",
                            "截图 OCR",
                            "只提取文字，不发送给翻译服务。",
                            "⌃⌥ O",
                            cx,
                        )),
                )
                .when(self.needs_screen_permission, |this| {
                    this.child(
                        div()
                            .p_5()
                            .bg(rgb(0xfdf1ec))
                            .rounded_xl()
                            .border_1()
                            .border_color(rgb(0xe8b7a3))
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(0x8a4326))
                                    .child("需要屏幕录制权限"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(0x7a5240))
                                    .child("截图任务需要读取屏幕内容。授权仅用于框选区域识别文字，不会持续录屏。"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .child(
                                        self.action("打开系统设置", "screen-settings", true)
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(|_, _, _, cx| {
                                                    let _ = operon_platform::open_screen_recording_settings();
                                                    cx.notify();
                                                }),
                                            ),
                                    )
                                    .child(
                                        self.action("重新检查", "screen-recheck", false)
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(|this, _, _, cx| {
                                                    this.check_screen_permission(cx)
                                                }),
                                            ),
                                    ),
                            ),
                    )
                })
                .child(
                    div()
                        .p_5()
                        .bg(rgb(0xffffff))
                        .rounded_xl()
                        .border_1()
                        .border_color(rgb(0xe0e7e3))
                        .flex()
                        .flex_col()
                        .gap_4()
                        .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child("文本翻译"))
                        .child(self.input.clone())
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .items_center()
                                .child(div().flex().gap_2().children(
                                    [("zh-CN", "中文"), ("en", "English"), ("ja", "日本語")].map(
                                        |(code, label)| {
                                            let active = self.target_language == code;
                                            div()
                                                .id(SharedString::from(format!("language-{code}")))
                                                .px_3()
                                                .py_1()
                                                .rounded_md()
                                                .text_sm()
                                                .cursor_pointer()
                                                .bg(if active {
                                                    rgb(0xdcefe8)
                                                } else {
                                                    rgba(0x00000000)
                                                })
                                                .child(label)
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(move |this, _, _, cx| {
                                                        this.select_language(code, cx)
                                                    }),
                                                )
                                        },
                                    ),
                                ))
                                .child(self.action("翻译文本", "text", true).on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, window, cx| {
                                        this.start("text", window, cx)
                                    }),
                                )),
                        ),
                )
                .child(self.result_panel(cx))
                .into_any_element(),
            Page::Capabilities => div()
                .flex()
                .flex_col()
                .gap_4()
                .child(div().text_2xl().font_weight(gpui::FontWeight::SEMIBOLD).child("能力"))
                .children(
                    [
                        (
                            "macOS 区域截图",
                            "输入 · 图片",
                            "本地",
                            "系统选区 · 支持多显示器",
                        ),
                        (
                            "Vision OCR",
                            "图片 → 文本",
                            "本地",
                            "中英文识别 · 保留坐标与置信度",
                        ),
                        (
                            "MyMemory 翻译",
                            "文本 → 文本",
                            "在线",
                            "api.mymemory.translated.net · 无需密钥",
                        ),
                        (
                            "Apple Speech",
                            "音频流 → 文本流",
                            "系统服务",
                            "首次使用请求麦克风和语音识别权限",
                        ),
                    ]
                    .map(|(name, kind, locality, detail)| {
                        div()
                            .p_5()
                            .bg(rgb(0xffffff))
                            .rounded_lg()
                            .border_1()
                            .border_color(rgb(0xe0e7e3))
                            .flex()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(name))
                                    .child(div().text_sm().text_color(rgb(0x63746c)).child(kind)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .items_end()
                                    .gap_1()
                                    .child(
                                        div().text_sm().text_color(rgb(0x176b5b)).child(locality),
                                    )
                                    .child(div().text_xs().text_color(rgb(0x63746c)).child(detail)),
                            )
                    }),
                )
                .into_any_element(),
            Page::Flows => div()
                .flex()
                .flex_col()
                .gap_4()
                .child(div().text_2xl().font_weight(gpui::FontWeight::SEMIBOLD).child("预设流程"))
                .children(
                    [
                        (
                            "截图翻译",
                            "区域截图 → Vision OCR → MyMemory 翻译 → 结果浮窗",
                        ),
                        ("截图 OCR", "区域截图 → Vision OCR → 结果浮窗"),
                        ("文本翻译", "文本输入 → MyMemory 翻译 → 结果浮窗"),
                        ("语音转译", "麦克风 → Apple Speech → 稳定片段 → 翻译 → 字幕"),
                    ]
                    .map(|(name, flow)| {
                        div()
                            .p_5()
                            .bg(rgb(0xffffff))
                            .rounded_lg()
                            .border_1()
                            .border_color(rgb(0xe0e7e3))
                            .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(name))
                            .child(div().mt_2().text_sm().text_color(rgb(0x63746c)).child(flow))
                    }),
                )
                .into_any_element(),
            Page::Runs => div()
                .flex()
                .flex_col()
                .gap_4()
                .child(div().text_2xl().font_weight(gpui::FontWeight::SEMIBOLD).child("最近一次运行"))
                .children(self.run_steps.iter().map(|step| {
                    div()
                        .p_4()
                        .bg(rgb(0xffffff))
                        .rounded_lg()
                        .border_1()
                        .border_color(rgb(0xe0e7e3))
                        .flex()
                        .justify_between()
                        .child(
                            div().child(step.capability_id.clone()).child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(0x63746c))
                                    .child(step.instance_id.clone()),
                            ),
                        )
                        .child(div().text_sm().child(format!(
                            "{:?} · {}ms",
                            step.status,
                            step.duration_ms.unwrap_or(0)
                        )))
                }))
                .when(self.run_steps.is_empty(), |this| this.child("尚未运行任务"))
                .into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .bg(rgb(0xf3f6f4))
            .text_color(rgb(0x20302a))
            .font_family(".AppleSystemUIFont")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Cancel, _, cx| this.cancel(cx)))
            .on_action(
                cx.listener(|this, _: &CopyOriginal, _, cx| this.copy(this.original.clone(), cx)),
            )
            .on_action(cx.listener(|this, _: &CopyTranslation, _, cx| {
                this.copy(this.translated.clone(), cx)
            }))
            .on_action(cx.listener(|this, _: &OpenScreenshot, window, cx| {
                this.start("screenshot", window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &OpenTranslation, window, cx| this.start("text", window, cx)),
            )
            .child(
                div()
                    .w(px(208.))
                    .h_full()
                    .p_5()
                    .bg(rgb(0x112e28))
                    .text_color(rgb(0xffffff))
                    .flex()
                    .flex_col()
                    .gap_6()
                    .child(
                        div()
                            .child(div().text_xl().font_weight(gpui::FontWeight::SEMIBOLD).child("能力流"))
                            .child(
                                div()
                                    .mt_1()
                                    .text_xs()
                                    .text_color(rgb(0x91b5aa))
                                    .child("OPERON · 桌面能力工作台"),
                            ),
                    )
                    .child(div().flex().flex_col().gap_2().children(navigation.map(
                        |(page, id, label)| {
                            let active = self.page == page;
                            div()
                                .id(id)
                                .px_3()
                                .py_2()
                                .rounded_md()
                                .cursor_pointer()
                                .bg(if active {
                                    rgb(0x245347)
                                } else {
                                    rgba(0x00000000)
                                })
                                .text_color(if active { rgb(0xffffff) } else { rgb(0xabc5bc) })
                                .child(label)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        this.page = page;
                                        cx.notify();
                                    }),
                                )
                        },
                    )))
                    .child(
                        div()
                            .mt_auto()
                            .text_xs()
                            .text_color(rgb(0x91b5aa))
                            .child("macOS · Rust + GPUI"),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .p_8()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .items_center()
                            .child(div().text_sm().child(self.status.clone()))
                            .when(self.running, |this| {
                                this.child(self.action("取消", "cancel", false).on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.cancel(cx)),
                                ))
                            }),
                    )
                    .child(content)
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .items_center()
                            .child(self.action("语音转译", "speech", false).on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| this.start("speech", window, cx)),
                            ))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(0x63746c))
                                    .child("在线翻译会发送文本至所选服务"),
                            ),
                    ),
            )
    }
}

impl OperonApp {
    fn result_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .p_5()
            .bg(rgb(0xffffff))
            .rounded_xl()
            .border_1()
            .border_color(rgb(0xe0e7e3))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("结果"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x63746c))
                            .child(self.service.clone()),
                    ),
            )
            .child(div().text_sm().child(if self.original.is_empty() {
                "识别原文会显示在这里".to_string()
            } else {
                self.original.clone()
            }))
            .child(
                div()
                    .text_lg()
                    .text_color(rgb(0x176b5b))
                    .child(if self.translated.is_empty() {
                        "译文会显示在这里".to_string()
                    } else {
                        self.translated.clone()
                    }),
            )
            .when_some(self.last_failure.clone(), |this, error| {
                this.child(div().text_sm().text_color(rgb(0xb14232)).child(error))
            })
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        self.action("复制原文", "copy-original", false)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.copy(this.original.clone(), cx)),
                            ),
                    )
                    .child(
                        self.action("复制译文", "copy-translation", false)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    this.copy(this.translated.clone(), cx)
                                }),
                            ),
                    )
                    .when(self.retry.is_some() && !self.running, |this| {
                        let (label, id) = match &self.retry {
                            Some(Retry::Translation(_)) => ("重试翻译", "retry-translate"),
                            _ => ("重试任务", "retry-task"),
                        };
                        this.child(
                            self.action(label, id, true)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.retry_failed(cx)),
                                ),
                        )
                    }),
            )
    }
}

fn main() {
    gpui::Application::new().run(|cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("escape", Cancel, None),
            KeyBinding::new("ctrl-alt-t", OpenScreenshot, None),
            KeyBinding::new("cmd-enter", OpenTranslation, None),
        ]);
        cx.on_action(|_: &Quit, cx| {
            operon_platform::cleanup_captures();
            cx.quit();
        });
        let bounds = Bounds::centered(None, size(px(1080.), px(800.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|cx| OperonApp::new(window, cx));
                let mut receiver = view.read(cx).controller.subscribe();
                cx.spawn({
                    let view = view.clone();
                    async move |cx| {
                        while let Ok(event) = receiver.recv().await {
                            view.update(cx, |this, cx| this.receive(event, cx)).ok();
                        }
                    }
                })
                .detach();
                view
            },
        )
        .expect("open Operon window");
        cx.activate(true);
    });
}
