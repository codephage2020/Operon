mod engine;
mod model;
mod speech;

pub use engine::{CapabilityRegistry, EngineError, WorkflowEngine};
pub use model::*;
pub use speech::{SegmentUpdate, SpeechCoordinator, SpeechEvent, SpeechStatus};

pub const PROTOCOL_VERSION: u32 = 1;
