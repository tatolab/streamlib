// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What the capability extensions installed beside the wheel registered.
//!
//! One registry per process taking an engine role: the app process reaches it
//! through the [`Runner`](super::Runner) that owns it, a helper process through
//! the wheel's own. The registration itself crosses no process boundary — a
//! helper's registrations are its own, and the app process renders only what
//! loaded in it.

use std::sync::Mutex;

use crate::core::error::{Error, Result};

/// One capability a loaded extension wheel registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedCapabilityExtension {
    /// The capability's name, unique across every loaded distribution.
    pub name: String,
    /// The version the registering distribution declared for it.
    pub version: String,
    /// The distribution whose entry point registered it.
    pub distribution: String,
}

/// The capabilities extension wheels have registered in this process.
#[derive(Debug, Default)]
pub struct LoadedCapabilityExtensionRegistry {
    registered: Mutex<Vec<LoadedCapabilityExtension>>,
}

impl LoadedCapabilityExtensionRegistry {
    /// Record `capability`, refusing a name another distribution already took.
    ///
    /// Refusal rather than last-one-wins: two wheels claiming one capability
    /// name is an installation the operator has to resolve, and a half-loaded
    /// extension is worse than one that refused.
    pub fn register(&self, capability: LoadedCapabilityExtension) -> Result<()> {
        if capability.name.is_empty() {
            return Err(Error::Runtime(format!(
                "the capability extension in `{}` registered a capability with an empty name",
                capability.distribution
            )));
        }
        let mut registered = self
            .registered
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(already) = registered
            .iter()
            .find(|already| already.name == capability.name)
        {
            return Err(Error::CapabilityExtensionNameAlreadyRegistered {
                capability: capability.name.clone(),
                already_registered_by: already.distribution.clone(),
                also_registered_by: capability.distribution,
            });
        }
        registered.push(capability);
        Ok(())
    }

    /// Take on everything `already_loaded` holds, under the same rule.
    ///
    /// How a runtime learns what loaded before it existed: the hooks run once
    /// per process, and every runtime in that process reports the same set.
    pub fn adopt(&self, already_loaded: Vec<LoadedCapabilityExtension>) -> Result<()> {
        already_loaded
            .into_iter()
            .try_for_each(|capability| self.register(capability))
    }

    /// Every capability registered so far, in registration order.
    pub fn registered(&self) -> Vec<LoadedCapabilityExtension> {
        self.registered
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capability(name: &str, distribution: &str) -> LoadedCapabilityExtension {
        LoadedCapabilityExtension {
            name: name.to_string(),
            version: "1.4.2".to_string(),
            distribution: distribution.to_string(),
        }
    }

    #[test]
    fn one_distribution_registering_two_capabilities_keeps_both_in_order() {
        let registry = LoadedCapabilityExtensionRegistry::default();

        registry
            .register(capability("webrtc", "streamlib-webrtc"))
            .expect("the first capability registers");
        registry
            .register(capability("whep", "streamlib-webrtc"))
            .expect("a second name from one distribution registers");

        let names: Vec<String> = registry
            .registered()
            .into_iter()
            .map(|registered| registered.name)
            .collect();
        assert_eq!(names, vec!["webrtc".to_string(), "whep".to_string()]);
    }

    #[test]
    fn two_distributions_registering_one_capability_name_are_refused_naming_both() {
        let registry = LoadedCapabilityExtensionRegistry::default();
        registry
            .register(capability("webrtc", "streamlib-webrtc"))
            .expect("the first capability registers");

        let refusal = registry
            .register(capability("webrtc", "someone-elses-webrtc"))
            .expect_err("the second registration of one name is refused");

        let refusal = refusal.to_string();
        assert!(
            refusal.contains("streamlib-webrtc") && refusal.contains("someone-elses-webrtc"),
            "the refusal must name both distributions, got: {refusal}"
        );
        assert_eq!(
            registry.registered(),
            vec![capability("webrtc", "streamlib-webrtc")],
            "a refused registration leaves the registry as it was"
        );
    }

    #[test]
    fn a_runtime_adopts_every_capability_the_process_had_already_registered() {
        let process_wide = LoadedCapabilityExtensionRegistry::default();
        process_wide
            .register(capability("webrtc", "streamlib-webrtc"))
            .expect("the capability registers");
        process_wide
            .register(capability("moq", "streamlib-moq"))
            .expect("the second capability registers");

        let a_later_runtime = LoadedCapabilityExtensionRegistry::default();
        a_later_runtime
            .adopt(process_wide.registered())
            .expect("a fresh registry adopts the process's set");

        assert_eq!(a_later_runtime.registered(), process_wide.registered());
    }

    #[test]
    fn a_capability_registered_with_an_empty_name_is_refused_naming_its_distribution() {
        let registry = LoadedCapabilityExtensionRegistry::default();

        let refusal = registry
            .register(capability("", "streamlib-webrtc"))
            .expect_err("an empty capability name is refused");

        assert!(
            refusal.to_string().contains("streamlib-webrtc"),
            "the refusal must name the distribution, got: {refusal}"
        );
        assert!(registry.registered().is_empty());
    }
}
