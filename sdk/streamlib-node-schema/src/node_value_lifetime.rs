// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

/// How long a value set live on an input lives, as the input declares it, or as a `GraphInput`
/// states it for the value it publishes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(rename_all = "lowercase")]
pub enum NodeValueLifetime {
    /// Kept in the project's store: it survives Stop, Play, close and a restart.
    #[default]
    Kept,
    /// Forgotten at Stop: its live value is dropped, and the next Play reads its saved value, or
    /// its declared default where it has none.
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

    /// Whether this is the lifetime an input declares when it names none.
    pub fn is_kept(&self) -> bool {
        *self == Self::Kept
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
