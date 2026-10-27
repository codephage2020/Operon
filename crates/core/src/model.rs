use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::mpsc::UnboundedSender;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataType {
    Text,
    Image,
    Audio,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextBlock {
    pub text: String,
    pub bounds: Option<Rect>,
    pub confidence: Option<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextData {
    pub text: String,
    pub language: Option<String>,
    pub blocks: Vec<TextBlock>,
}

impl TextData {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            language: None,
            blocks: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum Payload {
    Text(TextData),
    Image {
        path: PathBuf,
        format: String,
    },
    Audio {
        path: PathBuf,
        format: String,
        start_ms: u64,
        end_ms: u64,
    },
}

impl Payload {
    pub fn data_type(&self) -> DataType {
        match self {
            Self::Text(_) => DataType::Text,
            Self::Image { .. } => DataType::Image,
            Self::Audio { .. } => DataType::Audio,
        }
    }

    pub fn text(&self) -> Option<&TextData> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    Once,
    Stream,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Capability {
    pub id: String,
    pub name: String,
    pub input: DataType,
    pub output: DataType,
    pub mode: ExecutionMode,
    pub online: bool,
    pub permissions: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginInstance {
    pub id: String,
    pub plugin_id: String,
    pub name: String,
    pub version: String,
    pub enabled: bool,
    pub capabilities: Vec<Capability>,
    pub config: BTreeMap<String, String>,
    pub endpoint: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowStep {
    pub id: String,
    pub capability_id: String,
    pub instance_id: String,
    pub config: BTreeMap<String, String>,
    pub optional: bool,
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workflow {
    pub id: String,
    pub name: String,
    pub input: DataType,
    pub steps: Vec<WorkflowStep>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Skipped,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StepRecord {
    pub step_id: String,
    pub capability_id: String,
    pub instance_id: String,
    pub status: StepStatus,
    pub started_at_ms: u128,
    pub duration_ms: Option<u128>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: String,
    pub workflow_id: String,
    pub steps: Vec<StepRecord>,
}

#[derive(Clone, Debug)]
pub struct StepOutput {
    pub step_id: String,
    pub payload: Payload,
}

#[derive(Clone, Debug)]
pub struct RunOutput {
    pub record: RunRecord,
    pub outputs: Vec<StepOutput>,
}

#[derive(Clone, Debug)]
pub enum RunEvent {
    Step(StepRecord),
}

#[derive(Clone)]
pub struct ExecutionContext {
    pub config: BTreeMap<String, String>,
    pub cancellation: CancellationToken,
    pub events: Option<UnboundedSender<RunEvent>>,
}

#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<std::sync::atomic::AtomicBool>);

impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CapabilityError {
    #[error("操作已取消")]
    Cancelled,
    #[error("{0}")]
    InvalidInput(String),
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    Failed(String),
}

#[async_trait]
pub trait CapabilityExecutor: Send + Sync {
    async fn execute(
        &self,
        input: Payload,
        context: ExecutionContext,
    ) -> Result<Payload, CapabilityError>;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginRequest {
    pub protocol: u32,
    pub id: String,
    pub capability_id: String,
    pub input: Payload,
    pub config: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginResponse {
    pub protocol: u32,
    pub id: String,
    pub output: Option<Payload>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub protocol: u32,
    pub executable: String,
    pub capabilities: Vec<Capability>,
    pub permissions: Vec<String>,
}

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(45);
