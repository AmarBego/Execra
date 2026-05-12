use serde::{Deserialize, Serialize};

pub use execra;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TauriEvent {
    pub event: String,
    pub payload: execra::Event,
}

impl TauriEvent {
    pub fn from_execra(event: execra::Event) -> Self {
        TauriEvent {
            event: "execra://event".into(),
            payload: event,
        }
    }
}
