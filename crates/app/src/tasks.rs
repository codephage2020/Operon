use async_trait::async_trait;
use operon_core::*;
use operon_platform::{
    HttpTranslator, MacSpeechRecognizer, SpeechRecognitionEvent, VisionOcr, capture_region,
};
use operon_plugins::PluginManager;
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, mpsc};

pub fn default_plugin_root() -> std::path::PathBuf {
    std::env::var_os("OPERON_PLUGIN_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                .join("Library/Application Support/Operon/plugins")
        })
}

#[derive(Clone, Debug)]
pub enum TaskEvent {
    Steps(Vec<StepRecord>),
    /// A single step changed state mid-run, so the run detail updates live.
    StepChanged(StepRecord),
    Original(String),
    Finished {
        original: String,
        translated: String,
        error: Option<String>,
    },
    Speech {
        interim: String,
        translated: Option<String>,
        error: Option<String>,
    },
    Failed {
        output: RunOutput,
        task: String,
        message: String,
    },
}

struct Screenshot;
#[async_trait]
impl CapabilityExecutor for Screenshot {
    async fn execute(
        &self,
        _: Payload,
        context: ExecutionContext,
    ) -> Result<Payload, CapabilityError> {
        capture_region(context.cancellation).await
    }
}

async fn wait_cancel(token: CancellationToken) {
    while !token.is_cancelled() {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[derive(Clone, Debug)]
pub struct TranslationBinding {
    pub instance_id: String,
    pub capability_id: String,
    pub label: String,
    pub online: bool,
}

pub struct TaskController {
    sender: broadcast::Sender<TaskEvent>,
    manager: PluginManager,
    translation: Mutex<TranslationBinding>,
}

impl TaskController {
    pub fn new() -> Self {
        Self::with_plugin_root(default_plugin_root())
    }

    pub fn with_plugin_root(root: std::path::PathBuf) -> Self {
        let (sender, _) = broadcast::channel(256);
        let manager = PluginManager::new(root);
        let _ = manager.scan();
        Self {
            sender,
            manager,
            translation: Mutex::new(TranslationBinding {
                instance_id: "translate".into(),
                capability_id: "translate".into(),
                label: "MyMemory · 在线".into(),
                online: true,
            }),
        }
    }
    pub fn subscribe(&self) -> broadcast::Receiver<TaskEvent> {
        self.sender.subscribe()
    }

    pub fn plugins(&self) -> Vec<operon_plugins::InstalledPlugin> {
        self.manager.list()
    }

    pub fn install_plugin(&self, package: &std::path::Path) -> anyhow::Result<String> {
        let instance = self.manager.install(package)?;
        Ok(instance.name)
    }

    pub fn set_plugin_enabled(&self, id: &str, enabled: bool) -> anyhow::Result<TranslationBinding> {
        self.manager.set_enabled(id, enabled)?;
        Ok(self.maintain_binding())
    }

    pub fn uninstall_plugin(&self, id: &str) -> anyhow::Result<TranslationBinding> {
        self.manager.uninstall(id)?;
        Ok(self.maintain_binding())
    }

    /// A translation step bound to a removed or disabled instance falls back to
    /// the built-in service so existing tasks keep working. Enabling a plugin
    /// never switches the service by itself.
    fn maintain_binding(&self) -> TranslationBinding {
        let binding = self.translation.lock().expect("translation binding").clone();
        if binding.instance_id == "translate" {
            return binding;
        }
        let usable = self
            .manager
            .list()
            .into_iter()
            .any(|plugin| plugin.enabled
                && format!("{}:default", plugin.manifest.id) == binding.instance_id);
        if usable {
            binding
        } else {
            let fallback = built_in_translation();
            *self.translation.lock().expect("translation binding") = fallback.clone();
            fallback
        }
    }

    pub fn translation_binding(&self) -> TranslationBinding {
        self.translation.lock().expect("translation binding").clone()
    }

    /// Enabled text→text instances the translate step could use. Switching
    /// requires an explicit user action; nothing selects a service by itself.
    pub fn translation_choices(&self) -> Vec<(String, TranslationBinding)> {
        let registry = self.registry();
        registry
            .compatible_with(&DataType::Text, &DataType::Text)
            .into_iter()
            .map(|(instance_id, capability_id)| {
                let instance = registry.instance(&instance_id).expect("listed instance");
                let online = instance
                    .capabilities
                    .iter()
                    .any(|item| item.id == capability_id && item.online);
                (
                    instance_id.clone(),
                    TranslationBinding {
                        instance_id,
                        capability_id,
                        label: format!("{} · {}", instance.name, if online { "在线" } else { "本地" }),
                        online,
                    },
                )
            })
            .collect()
    }

    pub fn use_translation(&self, instance_id: &str) -> anyhow::Result<TranslationBinding> {
        let binding = self
            .translation_choices()
            .into_iter()
            .find(|(id, _)| id == instance_id)
            .map(|(_, binding)| binding)
            .ok_or_else(|| anyhow::anyhow!("该服务无法处理当前文本翻译步骤"))?;
        *self.translation.lock().expect("translation binding") = binding.clone();
        Ok(binding)
    }

    fn registry(&self) -> CapabilityRegistry {
        let mut registry = builtin_registry();
        for (instance, executor) in self.manager.executors() {
            registry.register(instance, executor);
        }
        registry
    }

    /// Snapshot of every registered instance, for the flow editor.
    pub fn registry_snapshot(&self) -> CapabilityRegistry {
        self.registry()
    }

    pub fn start(
        &self,
        task: String,
        text: String,
        language: String,
        cancellation: CancellationToken,
    ) {
        let sender = self.sender.clone();
        let registry = self.registry();
        let binding = self.translation.lock().expect("translation binding").clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("task runtime");
            runtime.block_on(async move {
                if task == "speech" {
                    speech(language, cancellation, sender).await;
                    return;
                }
                let engine = {
                    let (step_events, mut step_receiver) = mpsc::unbounded_channel();
                    let live = sender.clone();
                    // Forward step transitions to the UI as they happen.
                    tokio::spawn(async move {
                        while let Some(RunEvent::Step(step)) = step_receiver.recv().await {
                            let _ = live.send(TaskEvent::StepChanged(step));
                        }
                    });
                    WorkflowEngine::new(registry).with_events(step_events)
                };
                let (mut workflow, input) = if task == "text" {
                    (
                        Workflow {
                            id: task.clone(),
                            name: "文本翻译".into(),
                            input: DataType::Text,
                            steps: vec![translate_step(&binding)],
                        },
                        Payload::Text(TextData::plain(text)),
                    )
                } else {
                    let mut steps = vec![step("capture"), step("ocr")];
                    if task == "screenshot" {
                        steps.push(translate_step(&binding));
                    }
                    (
                        Workflow {
                            id: task.clone(),
                            name: "截图任务".into(),
                            input: DataType::Image,
                            steps,
                        },
                        Payload::Image {
                            path: "".into(),
                            format: "png".into(),
                        },
                    )
                };
                for step in &mut workflow.steps {
                    step.config
                        .insert("target_language".into(), language.clone());
                }
                match engine.run(&workflow, input, cancellation).await {
                    Ok(output) => {
                        let original = original(&output);
                        let translated = output
                            .outputs
                            .last()
                            .and_then(|item| item.payload.text())
                            .map(|item| item.text.clone())
                            .unwrap_or_default();
                        let _ = sender.send(TaskEvent::Steps(output.record.steps));
                        let _ = sender.send(TaskEvent::Original(original.clone()));
                        let _ = sender.send(TaskEvent::Finished {
                            original,
                            translated,
                            error: None,
                        });
                    }
                    Err(EngineError::Step {
                        output, message, ..
                    }) => {
                        let _ = sender.send(TaskEvent::Failed {
                            output,
                            task,
                            message,
                        });
                    }
                    Err(error) => {
                        let _ = sender.send(TaskEvent::Finished {
                            original: String::new(),
                            translated: String::new(),
                            error: Some(error.to_string()),
                        });
                    }
                }
            });
        });
    }

    pub fn retry(
        &self,
        task: String,
        output: RunOutput,
        language: String,
        cancellation: CancellationToken,
    ) {
        let sender = self.sender.clone();
        let registry = self.registry();
        let binding = self.translation.lock().expect("translation binding").clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("retry runtime");
            runtime.block_on(async move {
                let mut workflow = Workflow {
                    id: task,
                    name: "截图翻译".into(),
                    input: DataType::Image,
                    steps: vec![step("capture"), step("ocr"), translate_step(&binding)],
                };
                for step in &mut workflow.steps {
                    step.config
                        .insert("target_language".into(), language.clone());
                }
                match {
                    let (step_events, mut step_receiver) = mpsc::unbounded_channel();
                    let live = sender.clone();
                    tokio::spawn(async move {
                        while let Some(RunEvent::Step(step)) = step_receiver.recv().await {
                            let _ = live.send(TaskEvent::StepChanged(step));
                        }
                    });
                    WorkflowEngine::new(registry).with_events(step_events)
                }
                .retry(&workflow, output, "translate", cancellation)
                .await
                {
                    Ok(output) => {
                        let _ = sender.send(TaskEvent::Steps(output.record.steps.clone()));
                        let _ = sender.send(TaskEvent::Finished {
                            original: original(&output),
                            translated: output
                                .outputs
                                .last()
                                .and_then(|item| item.payload.text())
                                .map(|item| item.text.clone())
                                .unwrap_or_default(),
                            error: None,
                        });
                    }
                    Err(error) => {
                        let _ = sender.send(TaskEvent::Finished {
                            original: String::new(),
                            translated: String::new(),
                            error: Some(error.to_string()),
                        });
                    }
                }
            });
        });
    }
}

fn original(output: &RunOutput) -> String {
    output
        .outputs
        .iter()
        .find(|item| item.step_id == "ocr")
        .or_else(|| output.outputs.first())
        .and_then(|item| item.payload.text())
        .map(|item| item.text.clone())
        .unwrap_or_default()
}

fn builtin_display_name(id: &str) -> &str {
    match id {
        "capture" => "macOS 截图",
        "ocr" => "Vision OCR",
        "translate" => "MyMemory 翻译",
        other => other,
    }
}

fn builtin_registry() -> CapabilityRegistry {
    let mut registry = CapabilityRegistry::new();
    for (id, executor) in [
        (
            "capture",
            Arc::new(Screenshot) as Arc<dyn CapabilityExecutor>,
        ),
        ("ocr", Arc::new(VisionOcr) as Arc<dyn CapabilityExecutor>),
        (
            "translate",
            Arc::new(HttpTranslator::default()) as Arc<dyn CapabilityExecutor>,
        ),
    ] {
        let (input, output) = match id {
            "capture" => (DataType::Image, DataType::Image),
            "ocr" => (DataType::Image, DataType::Text),
            _ => (DataType::Text, DataType::Text),
        };
        registry.register(
            PluginInstance {
                id: id.into(),
                plugin_id: id.into(),
                name: builtin_display_name(id).into(),
                version: env!("CARGO_PKG_VERSION").into(),
                enabled: true,
                capabilities: vec![Capability {
                    id: id.into(),
                    name: id.into(),
                    input,
                    output,
                    mode: ExecutionMode::Once,
                    online: id == "translate",
                    permissions: Vec::new(),
                }],
                config: Default::default(),
                endpoint: None,
            },
            executor,
        );
    }
    registry
}

async fn speech(
    language: String,
    cancellation: CancellationToken,
    sender: broadcast::Sender<TaskEvent>,
) {
    let (events, mut receiver) = mpsc::unbounded_channel();
    let recognizer = match MacSpeechRecognizer::start("zh-CN", events).await {
        Ok(recognizer) => recognizer,
        Err(error) => {
            let _ = sender.send(TaskEvent::Finished {
                original: String::new(),
                translated: String::new(),
                error: Some(error.to_string()),
            });
            return;
        }
    };
    let translator = HttpTranslator::default();
    let mut coordinator = SpeechCoordinator::new(8);
    let mut translations = Vec::new();
    let mut stopping = false;
    loop {
        let event = tokio::select! {
            event = receiver.recv() => event,
            _ = wait_cancel(cancellation.clone()), if !stopping => { stopping = true; recognizer.stop().await; continue; }
        };
        match event {
            Some(SpeechRecognitionEvent::Update(update)) => {
                for event in coordinator.push(update) {
                    match event {
                        SpeechEvent::Interim { text, .. } => {
                            let _ = sender.send(TaskEvent::Speech {
                                interim: text,
                                translated: None,
                                error: None,
                            });
                        }
                        SpeechEvent::Translate { id, text } => {
                            let mut data = TextData::plain(text);
                            data.language = Some("zh-CN".into());
                            let context = ExecutionContext {
                                config: [("target_language".into(), language.clone())].into(),
                                cancellation: cancellation.clone(),
                                events: None,
                            };
                            match translator.execute(Payload::Text(data), context).await {
                                Ok(Payload::Text(result)) => {
                                    coordinator.acknowledge(&id);
                                    translations.push(result.text.clone());
                                    let _ = sender.send(TaskEvent::Speech {
                                        interim: String::new(),
                                        translated: Some(result.text),
                                        error: None,
                                    });
                                }
                                Ok(_) => {}
                                Err(error) => {
                                    let _ = sender.send(TaskEvent::Speech {
                                        interim: String::new(),
                                        translated: None,
                                        error: Some(error.to_string()),
                                    });
                                }
                            }
                        }
                        SpeechEvent::Revise { text, .. } => {
                            let _ = sender.send(TaskEvent::Speech {
                                interim: text,
                                translated: None,
                                error: None,
                            });
                        }
                        SpeechEvent::Overflow => {
                            let _ = sender.send(TaskEvent::Speech {
                                interim: String::new(),
                                translated: None,
                                error: Some("翻译处理积压，请放慢语速或稍后重试".into()),
                            });
                        }
                    }
                }
            }
            Some(SpeechRecognitionEvent::Error(error)) => {
                let _ = sender.send(TaskEvent::Speech {
                    interim: String::new(),
                    translated: None,
                    error: Some(error),
                });
                stopping = true;
                recognizer.stop().await;
            }
            Some(SpeechRecognitionEvent::Finished) | None => break,
        }
    }
    recognizer.stop().await;
    let _ = sender.send(TaskEvent::Finished {
        original: String::new(),
        translated: translations.join("\n"),
        error: None,
    });
}

fn step(id: &str) -> WorkflowStep {
    WorkflowStep {
        id: id.into(),
        capability_id: id.into(),
        instance_id: id.into(),
        config: Default::default(),
        optional: false,
        enabled: true,
        // The capture step waits for the user to frame a region, so it is paced
        // by cancellation rather than a fixed timeout.
        timeout: (id == "capture").then_some(None),
    }
}

/// The translate step keeps its stable id so retries and result lookups work
/// regardless of which instance currently serves it.
fn translate_step(binding: &TranslationBinding) -> WorkflowStep {
    WorkflowStep {
        id: "translate".into(),
        capability_id: binding.capability_id.clone(),
        instance_id: binding.instance_id.clone(),
        config: Default::default(),
        optional: false,
        enabled: true,
        timeout: None,
    }
}

fn built_in_translation() -> TranslationBinding {
    TranslationBinding {
        instance_id: "translate".into(),
        capability_id: "translate".into(),
        label: "MyMemory · 在线".into(),
        online: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn package_dir(root: &std::path::Path, id: &str) -> std::path::PathBuf {
        let package = root.join(format!("pkg-{id}"));
        fs::create_dir_all(&package).unwrap();
        fs::write(package.join("plugin"), "#!/bin/sh\n").unwrap();
        let manifest = PluginManifest {
            id: id.into(),
            name: "测试翻译器".into(),
            version: "0.1.0".into(),
            protocol: PROTOCOL_VERSION,
            executable: "plugin".into(),
            capabilities: vec![Capability {
                id: "translate".into(),
                name: "测试文本转换".into(),
                input: DataType::Text,
                output: DataType::Text,
                mode: ExecutionMode::Once,
                online: false,
                permissions: Vec::new(),
            }],
            permissions: Vec::new(),
        };
        fs::write(
            package.join("plugin.json"),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();
        package
    }

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "operon-controller-{tag}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn replacement_service_survives_until_disabled_then_falls_back() {
        let root = temp_root("swap");
        let controller = TaskController::with_plugin_root(root.join("plugins"));
        assert_eq!(controller.translation_binding().instance_id, "translate");

        let package = package_dir(&root, "test.translator");
        controller.install_plugin(&package).unwrap();
        let choices = controller.translation_choices();
        assert!(choices.iter().any(|(id, _)| id == "test.translator:default"));

        let binding = controller.use_translation("test.translator:default").unwrap();
        assert_eq!(binding.instance_id, "test.translator:default");
        assert!(!binding.online, "demo plugin is local; label must not claim online");

        let binding = controller.set_plugin_enabled("test.translator", false).unwrap();
        assert_eq!(binding.instance_id, "translate");

        controller.set_plugin_enabled("test.translator", true).unwrap();
        assert_eq!(
            controller.translation_binding().instance_id,
            "translate",
            "enabling a plugin must not switch the service by itself"
        );

        let binding = controller.uninstall_plugin("test.translator").unwrap();
        assert_eq!(binding.instance_id, "translate");
        assert!(controller.plugins().is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unknown_service_is_rejected_without_changing_binding() {
        let root = temp_root("reject");
        let controller = TaskController::with_plugin_root(root.join("plugins"));
        assert!(controller.use_translation("nope:default").is_err());
        assert_eq!(controller.translation_binding().instance_id, "translate");
        let _ = fs::remove_dir_all(&root);
    }
}
