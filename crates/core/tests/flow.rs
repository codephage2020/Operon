use operon_core::*;
use std::collections::BTreeMap;
use std::sync::Arc;

struct Noop;
#[async_trait::async_trait]
impl CapabilityExecutor for Noop {
    async fn execute(
        &self,
        _: Payload,
        _: ExecutionContext,
    ) -> Result<Payload, CapabilityError> {
        Err(CapabilityError::Failed("测试不执行".into()))
    }
}

fn instance(id: &str, capability: &str, input: DataType, output: DataType, online: bool) -> PluginInstance {
    PluginInstance {
        id: id.into(),
        plugin_id: id.into(),
        name: id.into(),
        version: "1".into(),
        enabled: true,
        capabilities: vec![Capability {
            id: capability.into(),
            name: capability.into(),
            input,
            output,
            mode: ExecutionMode::Once,
            online,
            permissions: Vec::new(),
        }],
        config: BTreeMap::new(),
        endpoint: None,
    }
}

fn registry() -> CapabilityRegistry {
    let mut registry = CapabilityRegistry::new();
    for (id, input, output) in [
        ("capture", DataType::Image, DataType::Image),
        ("ocr", DataType::Image, DataType::Text),
        ("translate", DataType::Text, DataType::Text),
        ("translate-b", DataType::Text, DataType::Text),
        ("summarize", DataType::Text, DataType::Text),
    ] {
        registry.register(
            instance(id, id, input, output, id.starts_with("translate")),
            Arc::new(Noop),
        );
    }
    registry
}

fn flow() -> Flow {
    preset_flows("translate", "zh-CN")
        .into_iter()
        .find(|flow| flow.workflow.id == "screenshot-translate")
        .expect("preset flow")
}

#[test]
fn preset_flows_validate_without_external_plugins() {
    let registry = registry();
    for flow in preset_flows("translate", "zh-CN") {
        flow.validate(&registry).expect("preset must be runnable");
    }
}

#[test]
fn replacing_translation_keeps_capture_and_ocr_untouched() {
    let registry = registry();
    let mut flow = flow();
    let before: Vec<(String, String)> = flow
        .workflow
        .steps
        .iter()
        .filter(|step| step.id != "translate")
        .map(|step| (step.id.clone(), step.instance_id.clone()))
        .collect();
    flow.replace_step(&registry, "translate", "translate-b", "translate-b")
        .expect("compatible replacement");
    let after: Vec<(String, String)> = flow
        .workflow
        .steps
        .iter()
        .filter(|step| step.id != "translate")
        .map(|step| (step.id.clone(), step.instance_id.clone()))
        .collect();
    assert_eq!(before, after);
    assert_eq!(flow.workflow.steps[2].instance_id, "translate-b");
    flow.validate(&registry).unwrap();
}

#[test]
fn incompatible_replacement_is_rejected_before_saving() {
    let registry = registry();
    let mut flow = flow();
    // The OCR step needs an image input; a text translator cannot serve it.
    let error = flow
        .replace_step(&registry, "ocr", "translate-b", "translate-b")
        .unwrap_err();
    assert!(matches!(error, FlowError::Incompatible(_)));
    assert_eq!(flow.workflow.steps[1].instance_id, "ocr");
}

#[test]
fn missing_and_disabled_instances_report_clearly() {
    let mut registry = registry();
    let mut flow = flow();
    assert!(matches!(
        flow.replace_step(&registry, "translate", "ghost", "translate"),
        Err(FlowError::MissingPlugin(_))
    ));
    let mut disabled = instance("off", "translate", DataType::Text, DataType::Text, false);
    disabled.enabled = false;
    registry.register(disabled, Arc::new(Noop));
    assert!(matches!(
        flow.replace_step(&registry, "translate", "off", "translate"),
        Err(FlowError::DisabledPlugin(_))
    ));
}

#[test]
fn required_steps_cannot_be_disabled() {
    let mut flow = flow();
    assert!(flow.set_step_enabled("ocr", false).is_err());
    // The translation step is optional in a translate flow.
    flow.set_step_enabled("translate", false).unwrap();
    assert!(!flow.workflow.steps[2].enabled);
}

#[test]
fn target_language_reaches_every_step_and_survives_export() {
    let mut flow = flow();
    flow.set_target_language("ja").unwrap();
    assert_eq!(flow.target_language, "ja");
    assert!(flow
        .workflow
        .steps
        .iter()
        .all(|step| step.config.get("target_language").map(String::as_str) == Some("ja")));
    let json = serde_json::to_string(&flow).unwrap();
    assert!(!json.contains("api_key"));
    let restored: Flow = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.target_language, "ja");
    restored.validate(&registry()).unwrap();
}

#[test]
fn replacement_choices_exclude_incompatible_instances() {
    let registry = registry();
    let flow = flow();
    let choices = flow.replacement_choices(&registry, "translate").unwrap();
    let ids: Vec<&str> = choices.iter().map(|item| item.instance_id.as_str()).collect();
    // Every text→text instance is offered, including a summary plugin: type
    // compatibility is not a semantic guarantee.
    assert!(ids.contains(&"translate"));
    assert!(ids.contains(&"translate-b"));
    assert!(ids.contains(&"summarize"));
    // OCR consumes images, so it can never stand in for a text step.
    assert!(!ids.contains(&"ocr"));
    assert!(!ids.contains(&"capture"));
}

#[test]
fn replacement_choices_respect_the_downstream_type() {
    let registry = registry();
    let mut flow = flow();
    // Insert a step that turns text back into an image is not a P0 concept, so
    // instead confirm that a text step before a text step is accepted while the
    // OCR step (which must output text into translate) rejects image output.
    let choices = flow.replacement_choices(&registry, "ocr").unwrap();
    let ids: Vec<&str> = choices.iter().map(|item| item.instance_id.as_str()).collect();
    assert_eq!(ids, vec!["ocr"], "only image→text instances may replace OCR");
    assert!(flow.set_step_enabled("translate", false).is_ok());
}
