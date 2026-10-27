//! Editable flow definitions.
//!
//! P0 only allows a narrow edit surface: view a preset flow, replace a step with
//! another compatible instance, change the target language, and enable or
//! disable optional steps. Adding arbitrary steps stays out of scope, so this
//! module validates and applies edits instead of exposing a graph editor.

use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Flow {
    pub workflow: Workflow,
    /// Target language applied to steps that accept it.
    pub target_language: String,
}

/// A step instance offered as a replacement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replacement {
    pub instance_id: String,
    pub capability_id: String,
    pub label: String,
    pub online: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlowError {
    UnknownStep(String),
    Incompatible(String),
    MissingPlugin(String),
    DisabledPlugin(String),
    InvalidLanguage(String),
}

impl std::fmt::Display for FlowError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownStep(step) => write!(formatter, "流程中没有步骤 {step}"),
            Self::Incompatible(message) => write!(formatter, "{message}"),
            Self::MissingPlugin(name) => write!(formatter, "插件实例 {name} 未安装"),
            Self::DisabledPlugin(name) => write!(formatter, "插件实例 {name} 已停用"),
            Self::InvalidLanguage(language) => {
                write!(formatter, "不支持的目标语言 {language}")
            }
        }
    }
}

impl std::error::Error for FlowError {}

impl Flow {
    /// Re-point a step at another instance. The replacement must declare a
    /// capability whose input matches the upstream output and whose output
    /// matches the downstream input; a type match only means the steps can be
    /// connected, so nothing here starts a task or switches online services.
    pub fn replace_step(
        &mut self,
        registry: &CapabilityRegistry,
        step_id: &str,
        instance_id: &str,
        capability_id: &str,
    ) -> Result<(), FlowError> {
        let (index, upstream, downstream) = self.surroundings(registry, step_id)?;
        let instance = registry
            .instance(instance_id)
            .ok_or_else(|| FlowError::MissingPlugin(instance_id.to_string()))?;
        if !instance.enabled {
            return Err(FlowError::DisabledPlugin(instance.name.clone()));
        }
        let capability = instance
            .capabilities
            .iter()
            .find(|item| item.id == capability_id)
            .ok_or_else(|| {
                FlowError::Incompatible(format!(
                    "{} 不提供能力 {capability_id}",
                    instance.name
                ))
            })?;
        if capability.input != upstream {
            return Err(FlowError::Incompatible(format!(
                "{} 需要 {:?}，上游输出 {:?}",
                instance.name, capability.input, upstream
            )));
        }
        if let Some(downstream) = downstream
            && capability.output != downstream
        {
            return Err(FlowError::Incompatible(format!(
                "{} 输出 {:?}，下游需要 {:?}",
                instance.name, capability.output, downstream
            )));
        }
        let step = &mut self.workflow.steps[index];
        step.instance_id = instance_id.to_string();
        step.capability_id = capability_id.to_string();
        Ok(())
    }

    pub fn set_target_language(&mut self, language: &str) -> Result<(), FlowError> {
        if language.trim().is_empty() {
            return Err(FlowError::InvalidLanguage(language.to_string()));
        }
        self.target_language = language.to_string();
        for step in &mut self.workflow.steps {
            step.config
                .insert("target_language".into(), language.to_string());
        }
        Ok(())
    }

    /// Only steps marked optional may be toggled; the required capture and OCR
    /// steps stay enabled so a flow cannot be saved into an unrunnable shape.
    pub fn set_step_enabled(&mut self, step_id: &str, enabled: bool) -> Result<(), FlowError> {
        let index = self
            .workflow
            .steps
            .iter()
            .position(|step| step.id == step_id)
            .ok_or_else(|| FlowError::UnknownStep(step_id.to_string()))?;
        let step = &mut self.workflow.steps[index];
        if !step.optional && !enabled {
            return Err(FlowError::Incompatible(format!(
                "步骤 {step_id} 是必需步骤，不能停用"
            )));
        }
        step.enabled = enabled;
        Ok(())
    }

    /// Validate the whole flow against the registry before saving.
    pub fn validate(&self, registry: &CapabilityRegistry) -> Result<(), FlowError> {
        let mut expected = self.workflow.input.clone();
        for step in self.workflow.steps.iter().filter(|step| step.enabled) {
            let instance = registry
                .instance(&step.instance_id)
                .ok_or_else(|| FlowError::MissingPlugin(step.instance_id.clone()))?;
            if !instance.enabled {
                return Err(FlowError::DisabledPlugin(instance.name.clone()));
            }
            let capability = instance
                .capabilities
                .iter()
                .find(|item| item.id == step.capability_id)
                .ok_or_else(|| {
                    FlowError::Incompatible(format!(
                        "{} 不提供能力 {}",
                        instance.name, step.capability_id
                    ))
                })?;
            if capability.input != expected {
                return Err(FlowError::Incompatible(format!(
                    "步骤 {} 需要 {:?}，上游输出 {:?}",
                    step.id, capability.input, expected
                )));
            }
            expected = capability.output.clone();
        }
        Ok(())
    }

    /// Enabled instances that could take a step's place, for the replacement
    /// picker. Only type-compatible candidates are offered; choosing one is
    /// still an explicit user action.
    pub fn replacement_choices(
        &self,
        registry: &CapabilityRegistry,
        step_id: &str,
    ) -> Result<Vec<Replacement>, FlowError> {
        let (_, upstream, downstream) = self.surroundings(registry, step_id)?;
        let mut choices = Vec::new();
        for (instance_id, capability_id) in registry.compatible_input(&upstream) {
            let Some(instance) = registry.instance(&instance_id) else {
                continue;
            };
            let Some(capability) = instance
                .capabilities
                .iter()
                .find(|item| item.id == capability_id)
            else {
                continue;
            };
            if let Some(downstream) = &downstream
                && capability.output != *downstream
            {
                continue;
            }
            choices.push(Replacement {
                instance_id,
                capability_id,
                label: instance.name.clone(),
                online: capability.online,
            });
        }
        Ok(choices)
    }

    /// The type a step receives and the type its successor expects.
    fn surroundings(
        &self,
        registry: &CapabilityRegistry,
        step_id: &str,
    ) -> Result<(usize, DataType, Option<DataType>), FlowError> {
        let index = self
            .workflow
            .steps
            .iter()
            .position(|step| step.id == step_id)
            .ok_or_else(|| FlowError::UnknownStep(step_id.to_string()))?;
        let input = self.step_input(registry, index)?;
        let output = if index + 1 < self.workflow.steps.len() {
            Some(self.step_input(registry, index + 1)?)
        } else {
            None
        };
        Ok((index, input, output))
    }

    fn step_input(
        &self,
        registry: &CapabilityRegistry,
        index: usize,
    ) -> Result<DataType, FlowError> {
        if index == 0 {
            return Ok(self.workflow.input.clone());
        }
        let previous = &self.workflow.steps[index - 1];
        let instance = registry
            .instance(&previous.instance_id)
            .ok_or_else(|| FlowError::MissingPlugin(previous.instance_id.clone()))?;
        instance
            .capabilities
            .iter()
            .find(|item| item.id == previous.capability_id)
            .map(|item| item.output.clone())
            .ok_or_else(|| {
                FlowError::Incompatible(format!(
                    "{} 不提供能力 {}",
                    instance.name, previous.capability_id
                ))
            })
    }
}

/// Build the three preset flows. These are the only flows P0 ships, and all of
/// them must work with no external plugin installed.
pub fn preset_flows(translate_instance: &str, target_language: &str) -> Vec<Flow> {
    let mut flows = Vec::new();
    for (id, name, with_translate) in [
        ("screenshot-translate", "截图翻译", true),
        ("screenshot-ocr", "截图 OCR", false),
        ("text-translate", "文本翻译", false),
    ] {
        let (input, steps) = match id {
            "text-translate" => (
                DataType::Text,
                vec![step("translate", translate_instance, true, target_language)],
            ),
            _ => {
                let mut steps = vec![
                    step("capture", "capture", false, target_language),
                    step("ocr", "ocr", false, target_language),
                ];
                if with_translate {
                    steps.push(step("translate", translate_instance, true, target_language));
                }
                (DataType::Image, steps)
            }
        };
        flows.push(Flow {
            workflow: Workflow {
                id: id.into(),
                name: name.into(),
                input,
                steps,
            },
            target_language: target_language.into(),
        });
    }
    flows
}

fn step(id: &str, instance: &str, optional: bool, language: &str) -> WorkflowStep {
    WorkflowStep {
        id: id.into(),
        capability_id: id.into(),
        instance_id: instance.into(),
        config: BTreeMap::from([("target_language".into(), language.into())]),
        optional,
        enabled: true,
        timeout: None,
    }
}
