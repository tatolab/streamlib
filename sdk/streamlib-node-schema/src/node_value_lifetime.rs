// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

/// How long a value the runtime keeps for one placed node lives, as a `GraphInput`'s config
/// declares it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeValueLifetime {
    /// Kept in the project's store: it survives Stop, Play, close and a restart.
    Kept,
    /// Forgotten at Stop: the next Play starts the node from its defaults.
    Reset,
}

impl NodeValueLifetime {
    /// Every lifetime, in the order a refusal names them.
    pub const EVERY_LIFETIME: [Self; 2] = [Self::Kept, Self::Reset];

    /// The lifetime's name where a config or a declaration writes it.
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Kept => "kept",
            Self::Reset => "reset",
        }
    }

    /// The lifetime `wire_name` names; `None` for a name no lifetime has.
    pub fn named(wire_name: &str) -> Option<Self> {
        Self::EVERY_LIFETIME
            .into_iter()
            .find(|lifetime| lifetime.wire_name() == wire_name)
    }
}

#[cfg(test)]
mod tests {
    use super::NodeValueLifetime;

    #[test]
    fn each_lifetime_is_named_by_its_wire_name_and_serializes_as_it() {
        for lifetime in NodeValueLifetime::EVERY_LIFETIME {
            assert_eq!(
                NodeValueLifetime::named(lifetime.wire_name()),
                Some(lifetime)
            );
            assert_eq!(
                serde_json::to_value(lifetime).expect("serializes"),
                serde_json::json!(lifetime.wire_name())
            );
        }
        assert_eq!(NodeValueLifetime::named("forever"), None);
    }
}
