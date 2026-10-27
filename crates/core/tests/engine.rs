use operon_core::*;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

struct TestCapability {
    input: DataType,
    output: DataType,
    fail: bool,
    cancel: bool,
    text: &'static str,
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl CapabilityExecutor for TestCapability {
    async fn execute(
        &self,
        input: Payload,
        context: ExecutionContext,
    ) -> Result<Payload, CapabilityError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if self.cancel {
            context.cancellation.cancel();
            return Err(CapabilityError::Cancelled);
        }
        if self.fail && call == 0 {
            return Err(CapabilityError::Failed("测试失败".into()));
        }
        if self.input != input.data_type() {
            return Err(CapabilityError::InvalidInput("类型错误".into()));
        }
        match self.output {
            DataType::Text => Ok(Payload::Text(TextData::plain(self.text))),
            DataType::Image => Ok(Payload::Image {
                path: "image.png".into(),
                format: "png".into(),
            }),
            DataType::Audio => unreachable!(),
        }
    }
}

fn setup(fail_translation: bool) -> (WorkflowEngine, Workflow, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    setup_with_ocr_text("识别原文", fail_translation)
}

fn setup_with_ocr_text(
    ocr_text: &'static str,
    fail_translation: bool,
) -> (WorkflowEngine, Workflow, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let ocr_calls = Arc::new(AtomicUsize::new(0));
    let translate_calls = Arc::new(AtomicUsize::new(0));
    let mut registry = CapabilityRegistry::default();
    for (id, input, output, fail, text, calls) in [
        (
            "ocr",
            DataType::Image,
            DataType::Text,
            false,
            ocr_text,
            ocr_calls.clone(),
        ),
        (
            "translate",
            DataType::Text,
            DataType::Text,
            fail_translation,
            "译文",
            translate_calls.clone(),
        ),
    ] {
        registry.register(
            PluginInstance {
                id: id.into(),
                plugin_id: id.into(),
                name: id.into(),
                version: "1".into(),
                enabled: true,
                capabilities: vec![Capability {
                    id: id.into(),
                    name: id.into(),
                    input: input.clone(),
                    output: output.clone(),
                    mode: ExecutionMode::Once,
                    online: false,
                    permissions: vec![],
                }],
                config: BTreeMap::new(),
                endpoint: None,
            },
            Arc::new(TestCapability {
                input,
                output,
                fail,
                cancel: false,
                text,
                calls,
            }),
        );
    }
    let workflow = Workflow {
        id: "screenshot-translate".into(),
        name: "截图翻译".into(),
        input: DataType::Image,
        steps: ["ocr", "translate"]
            .into_iter()
            .map(|id| WorkflowStep {
                id: id.into(),
                capability_id: id.into(),
                instance_id: id.into(),
                config: BTreeMap::new(),
                optional: false,
                enabled: true,
            })
            .collect(),
    };
    (
        WorkflowEngine::new(registry),
        workflow,
        ocr_calls,
        translate_calls,
    )
}

fn image() -> Payload {
    Payload::Image {
        path: "test.png".into(),
        format: "png".into(),
    }
}

#[tokio::test]
async fn translation_failure_preserves_ocr_and_retry_does_not_repeat_it() {
    let (engine, workflow, ocr, translation) = setup(true);
    let error = engine
        .run(&workflow, image(), CancellationToken::default())
        .await
        .unwrap_err();
    let EngineError::Step {
        step_id,
        output: previous,
        ..
    } = error
    else {
        panic!("expected step error");
    };
    assert_eq!(step_id, "translate");
    assert_eq!(previous.outputs.len(), 1);
    assert_eq!(previous.outputs[0].step_id, "ocr");
    assert_eq!(previous.outputs[0].payload.text().unwrap().text, "识别原文");

    let retried = engine
        .retry(
            &workflow,
            previous,
            "translate",
            CancellationToken::default(),
        )
        .await
        .unwrap();
    assert_eq!(retried.outputs.len(), 2);
    assert_eq!(retried.outputs[1].payload.text().unwrap().text, "译文");
    assert_eq!(ocr.load(Ordering::SeqCst), 1);
    assert_eq!(translation.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn rejects_incompatible_workflows_and_input_without_execution() {
    let (mut engine, mut workflow, ocr, translation) = setup(false);
    workflow.steps[1].capability_id = "ocr".into();
    assert!(matches!(
        engine
            .run(&workflow, image(), CancellationToken::default())
            .await,
        Err(EngineError::Validation(_))
    ));
    assert_eq!(translation.load(Ordering::SeqCst), 0);

    workflow.steps[1].capability_id = "translate".into();
    assert!(matches!(
        engine
            .run(
                &workflow,
                Payload::Text(TextData::plain("错误输入")),
                CancellationToken::default(),
            )
            .await,
        Err(EngineError::Validation(_))
    ));
    assert_eq!(ocr.load(Ordering::SeqCst), 0);

    let previous = RunOutput {
        record: RunRecord {
            id: "previous".into(),
            workflow_id: workflow.id.clone(),
            steps: vec![],
        },
        outputs: vec![StepOutput {
            step_id: "ocr".into(),
            payload: image(),
        }],
    };
    assert!(matches!(
        engine
            .retry(
                &workflow,
                previous,
                "translate",
                CancellationToken::default(),
            )
            .await,
        Err(EngineError::Validation(_))
    ));
    assert_eq!(translation.load(Ordering::SeqCst), 0);

    engine.registry = CapabilityRegistry::default();
    assert!(
        engine
            .run(&workflow, image(), CancellationToken::default())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cancelled_run_does_not_execute() {
    let (engine, workflow, ocr, _) = setup(false);
    let cancellation = CancellationToken::default();
    cancellation.cancel();
    assert!(matches!(
        engine.run(&workflow, image(), cancellation).await,
        Err(EngineError::Cancelled)
    ));
    assert_eq!(ocr.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cancellation_during_execution_preserves_cancelled_error() {
    let (mut engine, mut workflow, ocr, translation) = setup(false);
    let calls = Arc::new(AtomicUsize::new(0));
    engine.registry.register(
        PluginInstance {
            id: "cancel".into(),
            plugin_id: "cancel".into(),
            name: "cancel".into(),
            version: "1".into(),
            enabled: true,
            capabilities: vec![Capability {
                id: "cancel".into(),
                name: "cancel".into(),
                input: DataType::Image,
                output: DataType::Text,
                mode: ExecutionMode::Once,
                online: false,
                permissions: vec![],
            }],
            config: BTreeMap::new(),
            endpoint: None,
        },
        Arc::new(TestCapability {
            input: DataType::Image,
            output: DataType::Text,
            fail: false,
            cancel: true,
            text: "",
            calls,
        }),
    );
    workflow.steps[0].instance_id = "cancel".into();
    workflow.steps[0].capability_id = "cancel".into();
    assert!(matches!(
        engine
            .run(&workflow, image(), CancellationToken::default())
            .await,
        Err(EngineError::Cancelled)
    ));
    assert_eq!(ocr.load(Ordering::SeqCst), 0);
    assert_eq!(translation.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn empty_ocr_text_does_not_call_translation() {
    for text in ["", " \n\t"] {
        let (engine, workflow, ocr, translation) = setup_with_ocr_text(text, false);
        let output = engine
            .run(&workflow, image(), CancellationToken::default())
            .await
            .unwrap();
        assert_eq!(output.outputs.len(), 1);
        assert_eq!(output.outputs[0].step_id, "ocr");
        assert_eq!(ocr.load(Ordering::SeqCst), 1);
        assert_eq!(translation.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn history_keeps_only_the_latest_hundred_runs() {
    let (engine, workflow, _, _) = setup(false);
    let mut last_run_id = String::new();
    for _ in 0..105 {
        last_run_id = engine
            .run(&workflow, image(), CancellationToken::default())
            .await
            .unwrap()
            .record
            .id;
    }
    let history = engine.history();
    assert_eq!(history.len(), 100);
    assert_eq!(history.last().unwrap().id, last_run_id);
    assert!(history.windows(2).all(|pair| pair[0].id != pair[1].id));
}

#[test]
fn workflow_export_contains_no_secrets_or_results() {
    let (_, workflow, _, _) = setup(false);
    let json = serde_json::to_string(&workflow).unwrap();
    assert!(!json.contains("api_key"));
    assert!(!json.contains("识别"));
}
