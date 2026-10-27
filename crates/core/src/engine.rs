use crate::*;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::mpsc;

#[derive(Default)]
pub struct CapabilityRegistry {
    executors: HashMap<String, (PluginInstance, Arc<dyn CapabilityExecutor>)>,
}

impl CapabilityRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, instance: PluginInstance, executor: Arc<dyn CapabilityExecutor>) {
        self.executors
            .insert(instance.id.clone(), (instance, executor));
    }

    pub fn instance(&self, id: &str) -> Option<&PluginInstance> {
        self.executors.get(id).map(|(instance, _)| instance)
    }

    /// Enabled instances with any capability accepting `input`, paired with the
    /// capability id. Callers filter the output side against what comes next.
    pub fn compatible_input(&self, input: &DataType) -> Vec<(String, String)> {
        let mut matches = Vec::new();
        for (instance, _) in self.executors.values() {
            if !instance.enabled {
                continue;
            }
            for capability in &instance.capabilities {
                if capability.input == *input && capability.mode == ExecutionMode::Once {
                    matches.push((instance.id.clone(), capability.id.clone()));
                }
            }
        }
        matches
    }

    /// Enabled instances offering a capability with the given input and output
    /// types. A match only means the steps can be connected; it never starts a
    /// task or selects a service.
    pub fn compatible_with(&self, input: &DataType, output: &DataType) -> Vec<(String, String)> {
        let mut matches = Vec::new();
        for (instance, _) in self.executors.values() {
            if !instance.enabled {
                continue;
            }
            for capability in &instance.capabilities {
                if capability.input == *input
                    && capability.output == *output
                    && capability.mode == ExecutionMode::Once
                {
                    matches.push((instance.id.clone(), capability.id.clone()));
                }
            }
        }
        matches
    }

    pub fn validate(&self, workflow: &Workflow) -> Result<(), EngineError> {
        let mut expected = workflow.input.clone();
        for step in workflow.steps.iter().filter(|step| step.enabled) {
            let (instance, _) = self.executors.get(&step.instance_id).ok_or_else(|| {
                EngineError::Validation(format!(
                    "步骤 {} 缺少插件实例 {}",
                    step.id, step.instance_id
                ))
            })?;
            if !instance.enabled {
                return Err(EngineError::Validation(format!(
                    "插件 {} 已停用",
                    instance.name
                )));
            }
            let capability = instance
                .capabilities
                .iter()
                .find(|item| item.id == step.capability_id)
                .ok_or_else(|| {
                    EngineError::Validation(format!(
                        "插件 {} 不提供能力 {}",
                        instance.name, step.capability_id
                    ))
                })?;
            if capability.input != expected {
                return Err(EngineError::Validation(format!(
                    "步骤 {} 需要 {:?}，上游输出 {:?}",
                    step.id, capability.input, expected
                )));
            }
            expected = capability.output.clone();
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("{0}")]
    Validation(String),
    #[error("任务已取消")]
    Cancelled,
    #[error("{step_id}: {message}")]
    Step {
        step_id: String,
        message: String,
        output: RunOutput,
    },
}

pub struct WorkflowEngine {
    pub registry: CapabilityRegistry,
    history: Mutex<Vec<RunRecord>>,
    /// Optional live step updates, so the UI can show progress instead of
    /// waiting for the whole run to finish.
    events: Option<mpsc::UnboundedSender<RunEvent>>,
}

impl WorkflowEngine {
    pub fn new(registry: CapabilityRegistry) -> Self {
        Self {
            registry,
            history: Mutex::new(Vec::new()),
            events: None,
        }
    }

    pub fn with_events(mut self, events: mpsc::UnboundedSender<RunEvent>) -> Self {
        self.events = Some(events);
        self
    }

    pub fn history(&self) -> Vec<RunRecord> {
        self.history.lock().expect("history lock").clone()
    }

    pub async fn run(
        &self,
        workflow: &Workflow,
        input: Payload,
        cancellation: CancellationToken,
    ) -> Result<RunOutput, EngineError> {
        self.run_from(workflow, input, 0, Vec::new(), cancellation)
            .await
    }

    pub async fn retry(
        &self,
        workflow: &Workflow,
        previous: RunOutput,
        step_id: &str,
        cancellation: CancellationToken,
    ) -> Result<RunOutput, EngineError> {
        let index = workflow
            .steps
            .iter()
            .position(|step| step.id == step_id)
            .ok_or_else(|| EngineError::Validation("找不到重试步骤".into()))?;
        let input = if index == 0 {
            previous
                .outputs
                .first()
                .map(|item| item.payload.clone())
                .ok_or_else(|| EngineError::Validation("缺少原始输入".into()))?
        } else {
            previous
                .outputs
                .iter()
                .find(|item| item.step_id == workflow.steps[index - 1].id)
                .map(|item| item.payload.clone())
                .ok_or_else(|| EngineError::Validation("缺少重试步骤的上游结果".into()))?
        };
        self.run_from(
            workflow,
            input,
            index,
            previous.outputs.into_iter().take(index).collect(),
            cancellation,
        )
        .await
    }

    async fn run_from(
        &self,
        workflow: &Workflow,
        input: Payload,
        start: usize,
        mut outputs: Vec<StepOutput>,
        cancellation: CancellationToken,
    ) -> Result<RunOutput, EngineError> {
        self.registry.validate(workflow)?;
        if input.data_type()
            != if start == 0 {
                workflow.input.clone()
            } else {
                self.registry
                    .instance(&workflow.steps[start - 1].instance_id)
                    .and_then(|instance| {
                        instance
                            .capabilities
                            .iter()
                            .find(|item| item.id == workflow.steps[start - 1].capability_id)
                    })
                    .map(|item| item.output.clone())
                    .expect("validated previous step")
            }
        {
            return Err(EngineError::Validation("重试输入类型不匹配".into()));
        }
        let (events, _) = mpsc::unbounded_channel();
        let observer = self.events.clone();
        let mut record = RunRecord {
            id: uuid::Uuid::new_v4().to_string(),
            workflow_id: workflow.id.clone(),
            steps: Vec::new(),
        };
        let mut current = input;
        for step in workflow.steps.iter().skip(start) {
            if cancellation.is_cancelled() {
                return self.cancelled(record);
            }
            if !step.enabled {
                continue;
            }
            if current
                .text()
                .is_some_and(|text| text.text.trim().is_empty())
            {
                continue;
            }
            let mut running = StepRecord {
                step_id: step.id.clone(),
                capability_id: step.capability_id.clone(),
                instance_id: step.instance_id.clone(),
                status: StepStatus::Running,
                started_at_ms: now(),
                duration_ms: None,
                error: None,
            };
            record.steps.push(running.clone());
            if let Some(observer) = &observer {
                let _ = observer.send(RunEvent::Step(running.clone()));
            }
            let started = Instant::now();
            let executor = self
                .registry
                .executors
                .get(&step.instance_id)
                .expect("validated executor")
                .1
                .clone();
            let mut config = step.config.clone();
            // Plugin processes select which declared capability handles the
            // request; the step already names it, so bind it here.
            config
                .entry("capability_id".into())
                .or_insert_with(|| step.capability_id.clone());
            let context = ExecutionContext {
                config,
                cancellation: cancellation.clone(),
                events: Some(events.clone()),
            };
            let executing = executor.execute(current.clone(), context);
            let result = match step.timeout_or_default() {
                Some(timeout) => tokio::time::timeout(timeout, executing).await.map_err(|_| ()),
                // Interactive steps run unbounded; the executor watches the
                // cancellation token, so a stuck call can still be cancelled.
                None => Ok(executing.await),
            };
            running.duration_ms = Some(started.elapsed().as_millis());
            match result {
                Ok(Ok(payload)) => {
                    running.status = StepStatus::Succeeded;
                    outputs.push(StepOutput {
                        step_id: step.id.clone(),
                        payload: payload.clone(),
                    });
                    current = payload;
                }
                Ok(Err(CapabilityError::Cancelled)) | Err(_) if cancellation.is_cancelled() => {
                    return self.cancelled(record);
                }
                Ok(Err(error)) => {
                    running.status = StepStatus::Failed;
                    running.error = Some(error.to_string());
                    if let Some(item) = record.steps.last_mut() {
                        *item = running.clone();
                    }
                    if let Some(observer) = &observer {
                        let _ = observer.send(RunEvent::Step(running.clone()));
                    }
                    self.remember(record.clone());
                    return Err(EngineError::Step {
                        step_id: step.id.clone(),
                        message: error.to_string(),
                        output: RunOutput { record, outputs },
                    });
                }
                Err(_) => {
                    cancellation.cancel();
                    running.status = StepStatus::Failed;
                    running.error = Some("插件执行超时，已终止本次调用".into());
                    if let Some(item) = record.steps.last_mut() {
                        *item = running.clone();
                    }
                    if let Some(observer) = &observer {
                        let _ = observer.send(RunEvent::Step(running.clone()));
                    }
                    self.remember(record.clone());
                    return Err(EngineError::Step {
                        step_id: step.id.clone(),
                        message: "插件执行超时".into(),
                        output: RunOutput { record, outputs },
                    });
                }
            }
            if let Some(item) = record.steps.last_mut() {
                        *item = running.clone();
                    }
            if let Some(observer) = &observer {
                let _ = observer.send(RunEvent::Step(running.clone()));
            }
        }
        self.remember(record.clone());
        Ok(RunOutput { record, outputs })
    }

    fn remember(&self, record: RunRecord) {
        let mut history = self.history.lock().expect("history lock");
        history.push(record);
        if history.len() > 100 {
            let excess = history.len() - 100;
            history.drain(..excess);
        }
    }

    fn cancelled(&self, record: RunRecord) -> Result<RunOutput, EngineError> {
        self.remember(record);
        Err(EngineError::Cancelled)
    }
}

fn now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
