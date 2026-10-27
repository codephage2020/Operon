mod input;
mod results;
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

const BUILT_IN_CAPABILITIES: [(&str, &str, &str, &str); 4] = [
    (
        "macOS 区域截图",
        "图片 → 图片",
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
];

fn step_status_label(status: &StepStatus) -> &'static str {
    match status {
        StepStatus::Pending => "等待",
        StepStatus::Running => "运行中",
        StepStatus::Succeeded => "成功",
        StepStatus::Failed => "失败",
        StepStatus::Skipped => "跳过",
        StepStatus::Cancelled => "已取消",
    }
}

fn capability_row(    name: String,
    kind: String,
    locality: String,
    detail: String,
    actions: Vec<gpui::Stateful<gpui::Div>>,
) -> impl IntoElement {
    div()
        .p_5()
        .bg(rgb(0xffffff))
        .rounded_lg()
        .border_1()
        .border_color(rgb(0xe0e7e3))
        .flex()
        .justify_between()
        .items_center()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(name))
                .child(div().text_sm().text_color(rgb(0x63746c)).child(kind))
                .child(div().text_xs().text_color(rgb(0x63746c)).child(detail)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().text_sm().text_color(rgb(0x176b5b)).child(locality))
                .children(actions),
        )
}

actions!(
    operon,
    [
        Quit,
        Cancel,
        CopyOriginal,
        CopyTranslation,
        OpenScreenshot,
        OpenScreenshotOcr,
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
    result_window: Option<gpui::WindowHandle<results::ResultView>>,
    flows: Vec<Flow>,
    selected_flow: usize,
    /// Instance whose credential is being edited, plus the value in progress.
    /// The typed value lives only in memory and is cleared after saving.
    secret_target: Option<String>,
    secret_input: gpui::Entity<TextInput>,
}

impl OperonApp {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new(cx, "输入要翻译的文本…", true));
        cx.subscribe(&input, |this, _, event: &TextInputEvent, cx| {
            let TextInputEvent::Changed(value) = event;
            this.original = value.clone();
            cx.notify();
        })
        .detach();
        let focus_handle = cx.focus_handle();
        window.focus(&input.focus_handle(cx));
        let controller = TaskController::new();
        let service = controller.translation_binding().label;
        let flows = preset_flows(&controller.translation_binding().instance_id, "zh-CN");
        Self {
            page: Page::Tasks,
            input,
            focus_handle,
            controller,
            status: "就绪 · OCR 在本地完成".into(),
            original: String::new(),
            translated: String::new(),
            service,
            target_language: "zh-CN".into(),
            running: false,
            cancellation: None,
            run_steps: Vec::new(),
            last_failure: None,
            speech_updates: Vec::new(),
            speech_interim: String::new(),
            retry: None,
            needs_screen_permission: false,
            result_window: None,
            flows,
            selected_flow: 0,
            secret_target: None,
            secret_input: cx.new(|cx| TextInput::new(cx, "粘贴 API 密钥…", false)),
        }
    }

    /// Mirror the current result into the floating window. Opening happens on
    /// completion so a screenshot task started from another app surfaces its
    /// translation without the user returning to the workbench.
    fn show_result(&mut self, open: bool, cx: &mut Context<Self>) {
        let state = results::ResultState {
            original: self.original.clone(),
            translated: self.translated.clone(),
            service: self.service.clone(),
            target_language: self.target_language.clone(),
            failure: self.last_failure.clone(),
            online: self
                .controller
                .translation_binding()
                .online,
            running: self.running,
        };
        if !open {
            if let Some(handle) = self.result_window {
                let _ = handle.update(cx, |view, _window, cx| view.set_state(state, cx));
            }
            return;
        }
        let handle = results::show(state, cx, self.result_window);
        self.result_window = Some(handle);
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
            tasks::TaskEvent::StepChanged(step) => {
                match self
                    .run_steps
                    .iter_mut()
                    .find(|item| item.step_id == step.step_id)
                {
                    Some(existing) => *existing = step,
                    None => self.run_steps.push(step),
                }
            }
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
                let produced = !self.original.is_empty() || !self.translated.is_empty();
                self.show_result(produced, cx);
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
                // Keep the OCR text visible even when translation failed.
                self.show_result(!self.original.is_empty(), cx);
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

    /// A system-wide hotkey fired, possibly while another app was frontmost.
    /// Bring the window forward so the result is visible, then start or cancel
    /// the same task the in-window shortcut would.
    fn hotkey(&mut self, hotkey: operon_platform::Hotkey, window: &mut Window, cx: &mut Context<Self>) {
        use operon_platform::Hotkey;
        window.activate_window();
        match hotkey {
            Hotkey::ScreenshotTranslate => self.start("screenshot", window, cx),
            Hotkey::ScreenshotOcr => self.start("ocr", window, cx),
            Hotkey::TranslateText => {
                self.page = Page::Tasks;
                window.focus(&self.input.focus_handle(cx));
                self.status = "输入文本后按 ⌘↩ 翻译".into();
                cx.notify();
            }
            Hotkey::StartSpeech => self.start("speech", window, cx),
            Hotkey::Cancel => self.cancel(cx),
        }
    }

    fn select_language(&mut self, language: &str, cx: &mut Context<Self>) {
        self.target_language = language.into();
        // Keep the saved flows in step with the language the tasks will use.
        for flow in &mut self.flows {
            let _ = flow.set_target_language(language);
        }
        cx.notify();
    }

    fn install_plugin(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: true,
            multiple: false,
            prompt: Some("选择插件包目录或 plugin.json".into()),
        });
        let weak = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            let picked = match receiver.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                _ => None,
            };
            let Some(path) = picked else { return };
            let result = weak.update(cx, |this, cx| {
                let outcome = this.controller.install_plugin(&path);
                match outcome {
                    Ok(name) => {
                        this.status = format!("已安装插件 {name}");
                        this.service = this.controller.translation_binding().label;
                    }
                    Err(error) => this.status = format!("安装失败：{error}"),
                }
                cx.notify();
            });
            let _ = result;
        })
        .detach();
    }

    fn toggle_plugin(&mut self, id: String, enabled: bool, cx: &mut Context<Self>) {
        match self.controller.set_plugin_enabled(&id, enabled) {
            Ok(binding) => {
                self.service = binding.label;
                self.status = format!(
                    "插件 {} · {}",
                    id,
                    if enabled { "已启用" } else { "已停用" }
                );
            }
            Err(error) => self.status = format!("操作失败：{error}"),
        }
        cx.notify();
    }

    fn remove_plugin(&mut self, id: String, cx: &mut Context<Self>) {
        match self.controller.uninstall_plugin(&id) {
            Ok(binding) => {
                self.service = binding.label;
                self.status = format!("已卸载插件 {id}");
            }
            Err(error) => self.status = format!("卸载失败：{error}"),
        }
        cx.notify();
    }

    fn use_translation(&mut self, instance_id: String, cx: &mut Context<Self>) {
        match self.controller.use_translation(&instance_id) {
            Ok(binding) => {
                self.service = binding.label.clone();
                self.status = format!("翻译已切换为 {service}", service = binding.label);
            }
            Err(error) => self.status = format!("切换失败：{error}"),
        }
        cx.notify();
    }

    /// Replace a step with another compatible instance. Validation happens on
    /// the edited copy, so a rejected choice leaves the saved flow untouched.
    fn replace_step(
        &mut self,
        step_id: String,
        instance_id: String,
        capability_id: String,
        cx: &mut Context<Self>,
    ) {
        let registry = self.controller.registry_snapshot();
        let Some(flow) = self.flows.get_mut(self.selected_flow) else {
            return;
        };
        let mut edited = flow.clone();
        match edited.replace_step(&registry, &step_id, &instance_id, &capability_id) {
            Ok(()) => match edited.validate(&registry) {
                Ok(()) => {
                    *flow = edited;
                    self.status = format!("步骤 {step_id} 已替换，保存前校验通过");
                }
                Err(error) => self.status = format!("未保存：{error}"),
            },
            Err(error) => self.status = format!("无法替换：{error}"),
        }
        cx.notify();
    }

    fn toggle_step(&mut self, step_id: String, enabled: bool, cx: &mut Context<Self>) {
        let Some(flow) = self.flows.get_mut(self.selected_flow) else {
            return;
        };
        match flow.set_step_enabled(&step_id, enabled) {
            Ok(()) => {
                self.status = format!(
                    "步骤 {step_id} · {}",
                    if enabled { "已启用" } else { "已停用" }
                );
            }
            Err(error) => self.status = format!("{error}"),
        }
        cx.notify();
    }

    fn edit_secret(&mut self, instance_id: String, cx: &mut Context<Self>) {
        self.secret_target = Some(instance_id);
        self.secret_input
            .update(cx, |input, cx| input.set_content(String::new(), cx));
        cx.notify();
    }

    fn save_secret(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(instance_id) = self.secret_target.clone() else {
            return;
        };
        let value = self.secret_input.read(cx).content().trim().to_string();
        if value.is_empty() {
            self.status = "密钥为空，未保存".into();
            cx.notify();
            return;
        }
        match self.controller.save_secret(&instance_id, "api_key", &value) {
            Ok(()) => {
                // Drop the typed value immediately; the Keychain holds it now.
                self.secret_input
                    .update(cx, |input, cx| input.set_content(String::new(), cx));
                self.secret_target = None;
                self.status = format!("已保存 {instance_id} 的密钥到钥匙串");
                window.focus(&self.focus_handle);
            }
            Err(error) => self.status = error,
        }
        cx.notify();
    }

    fn forget_secret(&mut self, instance_id: String, cx: &mut Context<Self>) {
        match self.controller.forget_secret(&instance_id) {
            Ok(()) => self.status = format!("已删除 {instance_id} 的密钥"),
            Err(error) => self.status = error,
        }
        cx.notify();
    }

    fn set_flow_language(&mut self, language: &str, cx: &mut Context<Self>) {        let Some(flow) = self.flows.get_mut(self.selected_flow) else {
            return;
        };
        match flow.set_target_language(language) {
            Ok(()) => {
                self.target_language = language.into();
                self.status = format!("流程目标语言已改为 {language}");
            }
            Err(error) => self.status = format!("{error}"),
        }
        cx.notify();
    }

    fn action(
        &self,
        label: &str,
        id: impl Into<SharedString>,
        primary: bool,
    ) -> gpui::Stateful<gpui::Div> {        div()
            .id(gpui::ElementId::from(id.into()))
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

impl OperonApp {
    /// Credential storage for plugins that need an API key. Values go to the
    /// Keychain and never into flow files, logs or exports.
    fn credentials_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // The built-in translator is always present and can take an optional
        // key, so it is listed alongside installed plugins.
        let mut services: Vec<(String, String)> = vec![(
            "translate".into(),
            "MyMemory 翻译（内置）".into(),
        )];
        services.extend(self.controller.plugins().into_iter().map(|plugin| {
            (
                format!("{}:default", plugin.manifest.id),
                plugin.manifest.name.clone(),
            )
        }));
        div()
            .p_5()
            .bg(rgb(0xffffff))
            .rounded_lg()
            .border_1()
            .border_color(rgb(0xe0e7e3))
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("密钥"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(0x63746c))
                    .child("密钥保存在系统钥匙串，不写入流程、导出文件或日志。"),
            )
            .children(services.into_iter().map(|(instance_id, name)| {
                let configured = self.controller.secret_configured(&instance_id);
                let editing = self.secret_target.as_deref() == Some(instance_id.as_str());
                let edit_target = instance_id.clone();
                let forget_target = instance_id.clone();
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .items_center()
                            .child(
                                div()
                                    .text_sm()
                                    .child(format!("{name} · API 密钥")),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(if configured {
                                                rgb(0x176b5b)
                                            } else {
                                                rgb(0x8a6d1f)
                                            })
                                            .child(if configured {
                                                "已配置"
                                            } else {
                                                "未配置"
                                            }),
                                    )
                                    .child(
                                        self.action(
                                            if configured { "替换" } else { "配置" },
                                            format!("secret-edit-{instance_id}"),
                                            !configured,
                                        )
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |this, _, _, cx| {
                                                this.edit_secret(edit_target.clone(), cx)
                                            }),
                                        ),
                                    )
                                    .when(configured, |this| {
                                        this.child(
                                            self.action(
                                                "删除",
                                                format!("secret-forget-{instance_id}"),
                                                false,
                                            )
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(move |this, _, _, cx| {
                                                    this.forget_secret(
                                                        forget_target.clone(),
                                                        cx,
                                                    )
                                                }),
                                            ),
                                        )
                                    }),
                            ),
                    )
                    .when(editing, |this| {
                        this.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(
                                    div()
                                        .id(SharedString::from(format!(
                                            "secret-field-{instance_id}"
                                        )))
                                        .child(self.secret_input.clone()),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .gap_2()
                                        .child(
                                            self.action(
                                                "保存",
                                                format!("secret-save-{instance_id}"),
                                                true,
                                            )
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(|this, _, window, cx| {
                                                    this.save_secret(window, cx)
                                                }),
                                            ),
                                        )
                                        .child(
                                            self.action(
                                                "取消",
                                                format!("secret-cancel-{instance_id}"),
                                                false,
                                            )
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(|this, _, _, cx| {
                                                    this.secret_target = None;
                                                    this.secret_input.update(cx, |input, cx| {
                                                        input.set_content(String::new(), cx)
                                                    });
                                                    cx.notify();
                                                }),
                                            ),
                                        ),
                                ),
                        )
                    })
            }))
    }

    /// Step cards for the selected flow. Required steps expose no disable
    /// control, matching the rule that a flow cannot be saved unrunnable.
    fn flow_steps(&self, flow: &Flow, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let registry = self.controller.registry_snapshot();
        let mut cards = Vec::new();
        cards.push(
            div()
                .p_4()
                .bg(rgb(0xf7faf8))
                .rounded_lg()
                .border_1()
                .border_color(rgb(0xe0e7e3))
                .flex()
                .justify_between()
                .items_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(0x63746c))
                        .child("目标语言"),
                )
                .child(div().flex().gap_2().children(
                    [("zh-CN", "中文"), ("en", "English"), ("ja", "日本語")].map(
                        |(code, label)| {
                            let active = flow.target_language == code;
                            let id = format!("flow-language-{code}");
                            div()
                                .id(SharedString::from(id))
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .cursor_pointer()
                                .text_sm()
                                .bg(if active {
                                    rgb(0xdcefe8)
                                } else {
                                    rgba(0x00000000)
                                })
                                .child(label)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        this.set_flow_language(code, cx)
                                    }),
                                )
                        },
                    ),
                ))
                .into_any_element(),
        );
        for (index, step) in flow.workflow.steps.iter().enumerate() {
            let step_id = step.id.clone();
            let label = registry
                .instance(&step.instance_id)
                .map(|instance| instance.name.clone())
                .unwrap_or_else(|| format!("{}（缺失）", step.instance_id));
            let disabled = !step.enabled;
            let toggle_on = !step.enabled;
            let toggle_id = format!("step-toggle-{index}");
            let mut row = div()
                .p_4()
                .bg(rgb(0xffffff))
                .rounded_lg()
                .border_1()
                .border_color(if disabled {
                    rgb(0xe8b7a3)
                } else {
                    rgb(0xe0e7e3)
                })
                .flex()
                .justify_between()
                .items_center()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(format!("{}. {}", index + 1, label)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(0x63746c))
                                .child(if disabled {
                                    format!("{} · 已停用", step.id)
                                } else {
                                    step.id.clone()
                                }),
                        ),
                )
                .child(
                    div().flex().gap_2().items_center().when(step.optional, |this| {
                        this.child(
                            self.action(
                                if toggle_on { "启用" } else { "停用" },
                                toggle_id,
                                false,
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.toggle_step(step_id.clone(), toggle_on, cx)
                                }),
                            ),
                        )
                    }),
                );
            // Offer compatible replacements for the step, if any besides its
            // current instance.
            let choices = flow
                .replacement_choices(&registry, &step.id)
                .unwrap_or_default()
                .into_iter()
                .filter(|choice| choice.instance_id != step.instance_id)
                .collect::<Vec<_>>();
            if !choices.is_empty() {
                let step_id = step.id.clone();
                row = row.child(
                    div()
                        .flex()
                        .gap_2()
                        .flex_wrap()
                        .children(choices.into_iter().map(|choice| {
                            let step_id = step_id.clone();
                            let id = format!("replace-{}-{}", step_id, choice.instance_id);
                            let label = if choice.online {
                                format!("换用 {} · 在线", choice.label)
                            } else {
                                format!("换用 {} · 本地", choice.label)
                            };
                            self.action(&label, id, false).on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.replace_step(
                                        step_id.clone(),
                                        choice.instance_id.clone(),
                                        choice.capability_id.clone(),
                                        cx,
                                    )
                                }),
                            )
                        })),
                );
            }
            cards.push(row.into_any_element());
        }
        cards
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
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(0x63746c))
                        .child("内置能力开箱即用；外部插件可安装、启停和卸载。翻译服务可在兼容实例间替换。"),
                )
                .children(BUILT_IN_CAPABILITIES.map(|(name, kind, locality, detail)| {
                    capability_row(name.into(), kind.into(), locality.into(), detail.into(), Vec::new())
                }))
                .children(self.controller.plugins().into_iter().map(|plugin| {
                    let id = plugin.manifest.id.clone();
                    let kind = plugin
                        .manifest
                        .capabilities
                        .iter()
                        .map(|item| format!("{:?} → {:?}", item.input, item.output))
                        .collect::<Vec<_>>()
                        .join(" · ");
                    let locality = if plugin
                        .manifest
                        .capabilities
                        .iter()
                        .any(|item| item.online)
                    {
                        "在线"
                    } else {
                        "本地"
                    };
                    let enabled = plugin.enabled;
                    let toggle_label = if enabled { "停用" } else { "启用" };
                    let toggle_id = format!("toggle-{id}");
                    let remove_id = format!("uninstall-{id}");
                    let remove_target = id.clone();
                    let actions = vec![
                        self.action(toggle_label, toggle_id, !enabled).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.toggle_plugin(id.clone(), !enabled, cx)
                            }),
                        ),
                        self.action("卸载", remove_id, false).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.remove_plugin(remove_target.clone(), cx)
                            }),
                        ),
                    ];
                    capability_row(
                        plugin.manifest.name.clone(),
                        kind,
                        locality.into(),
                        format!("v{} · 独立进程", plugin.manifest.version),
                        actions,
                    )
                }))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            self.action("安装外部插件…", "install-plugin", true)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.install_plugin(cx)),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(0x8a4326))
                                .child("第三方插件以本地代码运行，请确认来源可信"),
                        ),
                )
                .child(self.credentials_card(cx))
                .child(
                    div()
                        .p_5()
                        .bg(rgb(0xffffff))
                        .rounded_lg()
                        .border_1()
                        .border_color(rgb(0xe0e7e3))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("翻译服务"),
                        )
                        .children(self.controller.translation_choices().into_iter().map(
                            |(instance_id, binding)| {
                                let active = binding.instance_id
                                    == self.controller.translation_binding().instance_id;
                                let label = binding.label.clone();
                                let use_id = format!("use-{instance_id}");
                                div()
                                    .flex()
                                    .justify_between()
                                    .items_center()
                                    .child(div().text_sm().child(label))
                                    .child(if active {
                                        div()
                                            .text_xs()
                                            .text_color(rgb(0x176b5b))
                                            .child("使用中")
                                            .into_any_element()
                                    } else {
                                        self.action("使用此服务", use_id, false)
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(move |this, _, _, cx| {
                                                    this.use_translation(instance_id.clone(), cx)
                                                }),
                                            )
                                            .into_any_element()
                                    })
                            },
                        )),
                )
                .into_any_element(),
            Page::Flows => div()
                .flex()
                .flex_col()
                .gap_4()
                .child(div().text_2xl().font_weight(gpui::FontWeight::SEMIBOLD).child("流程"))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(0x63746c))
                        .child("可以替换步骤使用的插件实例、修改目标语言、启停可选步骤。保存前会校验类型与配置。"),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .children(self.flows.iter().enumerate().map(|(index, flow)| {
                            let active = index == self.selected_flow;
                            let id = format!("flow-{index}");
                            div()
                                .id(SharedString::from(id))
                                .px_3()
                                .py_2()
                                .rounded_md()
                                .cursor_pointer()
                                .text_sm()
                                .bg(if active { rgb(0xdcefe8) } else { rgb(0xffffff) })
                                .border_1()
                                .border_color(rgb(0xe0e7e3))
                                .child(flow.workflow.name.clone())
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        this.selected_flow = index;
                                        cx.notify();
                                    }),
                                )
                        })),
                )
                .children(
                    self.flows
                        .get(self.selected_flow)
                        .map(|flow| self.flow_steps(flow, cx))
                        .into_iter()
                        .flatten(),
                )
                .into_any_element(),
            Page::Runs => div()
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    div()
                        .text_2xl()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("最近一次运行"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(0x63746c))
                        .child("默认日志不记录截图、识别文本、译文与密钥。"),
                )
                .children(self.run_steps.iter().enumerate().map(|(index, step)| {
                    let instance = self
                        .controller
                        .registry_snapshot()
                        .instance(&step.instance_id)
                        .map(|item| (item.name.clone(), item.capabilities.clone()));
                    let online = instance
                        .as_ref()
                        .is_some_and(|(_, capabilities)| {
                            capabilities
                                .iter()
                                .any(|item| item.id == step.capability_id && item.online)
                        });
                    let name = instance
                        .map(|(name, _)| name)
                        .unwrap_or_else(|| step.instance_id.clone());
                    let status_color = match step.status {
                        StepStatus::Succeeded => rgb(0x176b5b),
                        StepStatus::Failed => rgb(0xb14232),
                        StepStatus::Running => rgb(0x8a6d1f),
                        _ => rgb(0x7b8a83),
                    };
                    div()
                        .id(SharedString::from(format!("run-step-{index}")))
                        .p_4()
                        .bg(rgb(0xffffff))
                        .rounded_lg()
                        .border_1()
                        .border_color(if step.status == StepStatus::Failed {
                            rgb(0xe8b7a3)
                        } else {
                            rgb(0xe0e7e3)
                        })
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .child(
                                    div()
                                        .child(format!(
                                            "{}. {}",
                                            index + 1,
                                            if name.is_empty() {
                                                step.capability_id.as_str()
                                            } else {
                                                name.as_str()
                                            }
                                        ))
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(rgb(0x63746c))
                                                .child(format!(
                                                    "{}{}",
                                                    step.instance_id,
                                                    if online { " · 在线" } else { "" }
                                                )),
                                        ),
                                )
                                .child(div().text_sm().text_color(status_color).child(format!(
                                    "{} · {}ms",
                                    step_status_label(&step.status),
                                    step.duration_ms.unwrap_or(0)
                                ))),
                        )
                        .when_some(step.error.clone(), |this, error| {
                            this.child(div().text_sm().text_color(rgb(0xb14232)).child(error))
                        })
                }))
                .when(self.run_steps.is_empty(), |this| {
                    this.child(
                        div()
                            .text_sm()
                            .text_color(rgb(0x7b8a83))
                            .child("尚未运行任务"),
                    )
                })
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
            .on_action(cx.listener(|this, _: &OpenScreenshotOcr, window, cx| {
                this.start("ocr", window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenSpeech, window, cx| {
                this.start("speech", window, cx)
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
                                .id(SharedString::from(id))
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
                    })
                    .when(!self.original.is_empty() || !self.translated.is_empty(), |this| {
                        this.child(
                            self.action("浮窗显示", "show-result", false).on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.show_result(true, cx)),
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
            KeyBinding::new("ctrl-alt-o", OpenScreenshotOcr, None),
            KeyBinding::new("ctrl-alt-s", OpenTranslation, None),
            KeyBinding::new("ctrl-alt-r", OpenSpeech, None),
            KeyBinding::new("cmd-enter", OpenTranslation, None),
        ]);
        cx.on_action(|_: &Quit, cx| {
            operon_platform::cleanup_captures();
            cx.quit();
        });
        let bounds = Bounds::centered(None, size(px(1080.), px(800.)), cx);
        let (hotkeys, failed) = operon_platform::register_all();
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|cx| {
                    let mut app = OperonApp::new(window, cx);
                    if !failed.is_empty() {
                        let lost = failed
                            .iter()
                            .map(|hotkey| hotkey.label())
                            .collect::<Vec<_>>()
                            .join("、");
                        app.status = format!("以下全局快捷键已被其他应用占用：{lost}");
                    }
                    app
                });
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
                // Drain system-wide hotkey presses on the UI thread. Carbon
                // delivers on the main loop, so a short timer polls the channel
                // and dispatches with the window in hand.
                window.spawn(cx, {
                    let view = view.clone();
                    async move |cx| {
                        loop {
                            match hotkeys.try_recv() {
                                Ok(hotkey) => {
                                    let alive = cx.update_window_entity(
                                        &view,
                                        |this, window, cx| this.hotkey(hotkey, window, cx),
                                    );
                                    if alive.is_err() {
                                        break;
                                    }
                                }
                                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                            }
                            cx.background_executor()
                                .timer(std::time::Duration::from_millis(30))
                                .await;
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
