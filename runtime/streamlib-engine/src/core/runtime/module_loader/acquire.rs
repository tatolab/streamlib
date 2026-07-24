// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Policy-gated **acquire-on-reference**: on an installed-set load miss,
//! optionally fetch the providing package by version from the package source and record
//! it in `streamlib.lock` — completing the load without an explicit
//! `streamlib add`. Off by default (a normal run never reaches the network);
//! the [`AcquireOnReferencePolicy`] knob opts a fleet in.

use std::sync::{Arc, RwLock};

use streamlib_idents::{PackageRef, SemVerRange};

/// Environment variable carrying the acquire-on-reference policy —
/// `off` (default) / `on` / `prompt`. A programmatic override
/// ([`set_acquire_on_reference_policy`]) takes precedence.
pub(crate) const ACQUIRE_ON_REFERENCE_ENV: &str = "STREAMLIB_ACQUIRE_ON_REFERENCE";

/// Whether — and how — the runtime may acquire a package by version from the package source on
/// a load miss. Off by default: the load gate is installed-set-only unless a
/// fleet explicitly opts in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AcquireOnReferencePolicy {
    /// Never acquire. A miss surfaces the `streamlib add` fix-it.
    #[default]
    Off,
    /// Acquire on every miss.
    On,
    /// Acquire only when a host-installed confirmation handler approves.
    Prompt,
}

impl AcquireOnReferencePolicy {
    /// Parse the `off` / `on` / `prompt` spelling (case-insensitive). An
    /// unrecognized value is `None` so the caller can fall back to the default
    /// rather than silently enabling acquisition.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" | "false" | "0" => Some(Self::Off),
            "on" | "true" | "1" => Some(Self::On),
            "prompt" | "ask" => Some(Self::Prompt),
            _ => None,
        }
    }
}

/// A host-installed acquisition confirmation gate for
/// [`AcquireOnReferencePolicy::Prompt`]: given the package + resolution range
/// about to be acquired, return `true` to proceed. A host (such as the CLI) can
/// install a confirmation handler here; with none installed, `Prompt` fails
/// closed. The engine performs no interactive I/O itself (engine purity).
pub type AcquireConfirmationHandler = Arc<dyn Fn(&PackageRef, &SemVerRange) -> bool + Send + Sync>;

/// Process-wide policy override. `None` falls back to [`ACQUIRE_ON_REFERENCE_ENV`],
/// then the [`AcquireOnReferencePolicy::Off`] default.
static POLICY_OVERRIDE: RwLock<Option<AcquireOnReferencePolicy>> = RwLock::new(None);

/// Process-wide confirmation handler for the `Prompt` policy.
static CONFIRMATION_HANDLER: RwLock<Option<AcquireConfirmationHandler>> = RwLock::new(None);

/// Set the process-wide acquire-on-reference policy override. `None` clears it
/// (back to env / default).
pub(crate) fn set_acquire_on_reference_policy(policy: Option<AcquireOnReferencePolicy>) {
    *POLICY_OVERRIDE
        .write()
        .expect("acquire policy override lock poisoned") = policy;
}

/// Install (or clear, with `None`) the confirmation handler consulted under the
/// `Prompt` policy.
pub(crate) fn set_acquire_confirmation_handler(handler: Option<AcquireConfirmationHandler>) {
    *CONFIRMATION_HANDLER
        .write()
        .expect("acquire confirmation handler lock poisoned") = handler;
}

/// The effective acquire-on-reference policy: the runtime override, else the
/// [`ACQUIRE_ON_REFERENCE_ENV`] env var, else [`AcquireOnReferencePolicy::Off`].
/// An unrecognized env value warns once per read and falls back to `Off`.
pub(crate) fn acquire_on_reference_policy() -> AcquireOnReferencePolicy {
    if let Some(policy) = *POLICY_OVERRIDE
        .read()
        .expect("acquire policy override lock poisoned")
    {
        return policy;
    }
    match std::env::var(ACQUIRE_ON_REFERENCE_ENV) {
        Ok(raw) if !raw.trim().is_empty() => match AcquireOnReferencePolicy::parse(&raw) {
            Some(policy) => policy,
            None => {
                tracing::warn!(
                    value = %raw,
                    env = ACQUIRE_ON_REFERENCE_ENV,
                    "unrecognized acquire-on-reference policy — expected off/on/prompt; \
                     defaulting to off"
                );
                AcquireOnReferencePolicy::Off
            }
        },
        _ => AcquireOnReferencePolicy::Off,
    }
}

/// Decide whether acquisition is permitted for `pkg_ref` at `range` under the
/// current policy. `Off` → never; `On` → always; `Prompt` → the confirmation
/// handler's verdict (or `false` — fail closed — when none is installed).
pub(crate) fn acquisition_permitted(pkg_ref: &PackageRef, range: &SemVerRange) -> bool {
    match acquire_on_reference_policy() {
        AcquireOnReferencePolicy::Off => false,
        AcquireOnReferencePolicy::On => true,
        AcquireOnReferencePolicy::Prompt => {
            let handler = CONFIRMATION_HANDLER
                .read()
                .expect("acquire confirmation handler lock poisoned")
                .clone();
            match handler {
                Some(confirm) => confirm(pkg_ref, range),
                None => {
                    tracing::warn!(
                        package = %pkg_ref,
                        "acquire-on-reference is set to `prompt` but no confirmation handler \
                         is installed — declining to acquire (set the policy to `on`, or \
                         install a confirmation handler)"
                    );
                    false
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib_idents::{Org, Package};

    fn pkg_ref(org: &str, name: &str) -> PackageRef {
        PackageRef::new(Org::new(org).unwrap(), Package::new(name).unwrap())
    }

    #[test]
    fn parse_accepts_the_three_spellings() {
        assert_eq!(
            AcquireOnReferencePolicy::parse("off"),
            Some(AcquireOnReferencePolicy::Off)
        );
        assert_eq!(
            AcquireOnReferencePolicy::parse("ON"),
            Some(AcquireOnReferencePolicy::On)
        );
        assert_eq!(
            AcquireOnReferencePolicy::parse(" Prompt "),
            Some(AcquireOnReferencePolicy::Prompt)
        );
        assert_eq!(AcquireOnReferencePolicy::parse("garbage"), None);
    }

    #[test]
    fn default_policy_is_off() {
        assert_eq!(
            AcquireOnReferencePolicy::default(),
            AcquireOnReferencePolicy::Off
        );
    }

    /// The override drives the permission decision without touching env, and
    /// `Prompt` fails closed with no handler installed. Serialized against the
    /// process-wide statics via a mutex so the parallel test runner can't
    /// interleave two policy writers.
    #[test]
    fn permission_honors_override_and_prompt_fails_closed() {
        use std::sync::Mutex;
        static SERIALIZE: Mutex<()> = Mutex::new(());
        let _guard = SERIALIZE.lock().unwrap();

        let pr = pkg_ref("tatolab", "camera");
        let range = SemVerRange::Any;

        set_acquire_on_reference_policy(Some(AcquireOnReferencePolicy::Off));
        assert!(!acquisition_permitted(&pr, &range), "off never acquires");

        set_acquire_on_reference_policy(Some(AcquireOnReferencePolicy::On));
        assert!(acquisition_permitted(&pr, &range), "on always acquires");

        // Prompt with no handler fails closed.
        set_acquire_confirmation_handler(None);
        set_acquire_on_reference_policy(Some(AcquireOnReferencePolicy::Prompt));
        assert!(
            !acquisition_permitted(&pr, &range),
            "prompt without a handler must fail closed"
        );

        // Prompt with an approving handler acquires.
        set_acquire_confirmation_handler(Some(Arc::new(|_pkg, _range| true)));
        assert!(
            acquisition_permitted(&pr, &range),
            "prompt with an approving handler acquires"
        );

        // A declining handler blocks.
        set_acquire_confirmation_handler(Some(Arc::new(|_pkg, _range| false)));
        assert!(
            !acquisition_permitted(&pr, &range),
            "prompt with a declining handler blocks"
        );

        // Reset the process-wide statics for other tests.
        set_acquire_confirmation_handler(None);
        set_acquire_on_reference_policy(None);
    }
}
