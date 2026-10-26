//! Per-device exit-node state machine (shared vocabulary).
//!
//! Shared invariant: at any moment at most one side of the pair may be
//! `UsingOther`. Both devices `UsingOther` at once would create a routing
//! loop where outbound traffic has nowhere to go, so transitions into
//! `UsingOther` are guarded (see [`crate::coordinator`]).

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitState {
    /// no exit-node relationship with the peer
    None,
    /// this device routes traffic via the peer
    UsingOther,
    /// the peer routes traffic via this device
    BeingUsed,
}

impl ExitState {
    /// Human-readable label (tray menu / UI).
    pub fn label(self) -> &'static str {
        match self {
            ExitState::None => "Independent (no exit node)",
            ExitState::UsingOther => "Using peer as exit node",
            ExitState::BeingUsed => "Serving as peer's exit node",
        }
    }
}

impl fmt::Display for ExitState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_case_round_trip() {
        for s in [ExitState::None, ExitState::UsingOther, ExitState::BeingUsed] {
            let json = serde_json::to_value(s).unwrap();
            assert_eq!(serde_json::from_value::<ExitState>(json).unwrap(), s);
        }
        assert_eq!(
            serde_json::to_value(ExitState::UsingOther).unwrap(),
            serde_json::json!("using_other")
        );
    }
}
