use async_trait::async_trait;
use operon_core::*;
use operon_platform::{
    HttpTranslator, MacSpeechRecognizer, SpeechRecognitionEvent, VisionOcr, capture_region,
};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};

#[derive(Clone, Debug)]
pub enum TaskEvent {
    Steps(Vec<StepRecord>),
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
        if context.cancellation.is_cancelled() {
            return Err(CapabilityError::Cancelled);
        }
        tokio::select! { result = capture_region() => result, _ = wait_cancel(context.cancellation) => Err(CapabilityError::Cancelled) }
    }
}

async fn wait_cancel(token: CancellationToken) {
    while !token.is_cancelled() {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

pub struct TaskController {
    sender: broadcast::Sender<TaskEvent>,
}

impl TaskController {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(256);
        Self { sender }
    }
    pub fn subscribe(&self) -> broadcast::Receiver<TaskEvent> {
        self.sender.subscribe()
    }

    pub fn start(
        &self,
        task: String,
        text: String,
        language: String,
        cancellation: CancellationToken,
    ) {
        let sender = self.sender.clone();
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
                let engine = WorkflowEngine::new(builtin_registry());
                let (mut workflow, input) = if task == "text" {
                    (
                        Workflow {
                            id: task.clone(),
                            name: "文本翻译".into(),
                            input: DataType::Text,
                            steps: vec![step("translate")],
                        },
                        Payload::Text(TextData::plain(text)),
                    )
                } else {
                    let mut steps = vec![step("capture"), step("ocr")];
                    if task == "screenshot" {
                        steps.push(step("translate"));
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
                    steps: vec![step("capture"), step("ocr"), step("translate")],
                };
                for step in &mut workflow.steps {
                    step.config
                        .insert("target_language".into(), language.clone());
                }
                match WorkflowEngine::new(builtin_registry())
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
                name: id.into(),
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
    }
}
