use serde::{Deserialize, Serialize};

use crate::{MutationRequestId, RunId, SessionId};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case", deny_unknown_fields)]
pub enum SteeringChange {
    Enqueue {
        run_id: RunId,
        text: String,
    },
    Edit {
        item_id: [u8; 16],
        revision: u64,
        text: String,
    },
    Remove {
        item_id: [u8; 16],
        revision: u64,
    },
    Pause,
    Resume {
        run_id: RunId,
    },
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteeringMutation {
    pub request_id: MutationRequestId,
    pub session_id: SessionId,
    pub expected_revision: u64,
    pub change: SteeringChange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteeringReceipt {
    pub sequence: u64,
    pub queue_revision: u64,
    pub item_id: Option<[u8; 16]>,
    pub item_revision: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteeringCursor {
    pub session_id: SessionId,
    pub sequence: u64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteeringItem {
    pub id: [u8; 16],
    pub revision: u64,
    pub enqueue_sequence: u64,
    pub text: String,
}

impl std::fmt::Debug for SteeringItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SteeringItem")
            .field("id", &self.id)
            .field("revision", &self.revision)
            .field("enqueue_sequence", &self.enqueue_sequence)
            .field("text_bytes", &self.text.len())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteeringSnapshot {
    pub cursor: SteeringCursor,
    pub revision: u64,
    pub target_run_id: Option<RunId>,
    pub paused: bool,
    pub items: Vec<SteeringItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteeringNotice {
    pub cursor: SteeringCursor,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteeringPage {
    pub notices: Vec<SteeringNotice>,
    pub high_water: SteeringCursor,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steering_wire_is_closed_and_debug_redacts_text() {
        let request = crate::ApplicationRequest::MutateSteering {
            mutation: SteeringMutation {
                request_id: MutationRequestId::from_bytes([1; 16]),
                session_id: SessionId::from_bytes([2; 16]),
                expected_revision: 0,
                change: SteeringChange::Enqueue {
                    run_id: RunId::from_bytes([3; 16]),
                    text: "private steering text".into(),
                },
            },
        };
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(
            serde_json::from_value::<crate::ApplicationRequest>(value.clone()).unwrap(),
            request
        );
        assert!(!format!("{request:?}").contains("private steering text"));
        for field in ["actor", "attachments", "delivery", "outcome"] {
            let mut invalid = value.clone();
            invalid["mutation"][field] = serde_json::json!(true);
            assert!(serde_json::from_value::<crate::ApplicationRequest>(invalid).is_err());
        }
        let mut invalid = value;
        invalid["mutation"]["change"]["actor"] = serde_json::json!("local_owner");
        assert!(serde_json::from_value::<crate::ApplicationRequest>(invalid).is_err());
        assert!(
            serde_json::from_value::<SteeringChange>(serde_json::json!({"change":"resume"}))
                .is_err()
        );
        let item = SteeringItem {
            id: [4; 16],
            revision: 1,
            enqueue_sequence: 1,
            text: "private steering text".into(),
        };
        assert!(!format!("{item:?}").contains("private steering text"));
    }
}
