// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Isolation trust tier — the by-construction capability moat deciding whether
//! a processor's privileged lifecycle may mint an in-process
//! [`RuntimeContextFullAccess`](super::RuntimeContextFullAccess).
//!
//! This is an **orthogonal trust axis** that composes with the phase-axis
//! capability typestate (`RuntimeContextFullAccess` for `setup`/`start`/`stop`/
//! `teardown`, `RuntimeContextLimitedAccess` for the hot path). The phase axis
//! answers *which* lifecycle method is running; the trust axis answers *whether
//! the code running it is trusted enough to hold FullAccess in-process at all*.
//!
//! Every processor the engine can spawn today is
//! [`IsolationTier::TrustedInstalled`], and the tier is not derived from
//! anything: it used to be read off a module's `@session/…` provenance, and
//! provenance died with the identity grammar that spelled it. The separately
//! built, dlopen'd code the [`Untrusted`](IsolationTier::Untrusted) tier
//! existed to sandbox died earlier still, with the plugin ABI.
//!
//! The moat is the [`FullAccessGrant`] token: minting a
//! [`RuntimeContextFullAccess`](super::RuntimeContextFullAccess) requires one,
//! and a grant is producible **only** from [`IsolationTier::TrustedInstalled`]
//! (see [`IsolationTier::grant_full_access`]). The untrusted lifecycle dispatch
//! has no grant to pass, so an in-process FullAccess context is unrepresentable
//! for it — a compile-time guarantee, not a runtime check. That guarantee is
//! what survives here, and it is why the tier is a two-variant enum rather than
//! a constant: it is the seam an untrusted-code path returns through, and the
//! dispatch that honours it stays compiled and tested meanwhile.
//!
//! Actual runtime *enforcement* of the untrusted tier — own-subprocess sandbox,
//! cgroup-v2 limits — is a separate concern (isolation *enforcement*); this
//! module owns only the policy model and the capability moat at the minting
//! seam.

/// Declarative trust tier a loaded processor runs under.
///
/// Composes with — does not replace — the phase-axis capability typestate. A
/// [`TrustedInstalled`](Self::TrustedInstalled) processor still only sees
/// FullAccess in its privileged lifecycle methods; an
/// [`Untrusted`](Self::Untrusted) processor never sees an in-process FullAccess
/// at all.
///
/// The capability moat is sealed by construction: `grant_full_access` (the only
/// producer of the token `RuntimeContextFullAccess::new` requires) is
/// crate-internal, so no external caller — trusted tier or not — can mint a
/// FullAccess grant:
///
/// ```compile_fail
/// use streamlib::sdk::context::IsolationTier;
/// let _ = IsolationTier::TrustedInstalled.grant_full_access();
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationTier {
    /// Separately built, untrusted code. Never mints an in-process FullAccess
    /// context — its privileged lifecycle is expected to run behind the
    /// subprocess sandbox (isolation enforcement). Nothing the engine can spawn
    /// today classifies here.
    Untrusted,
    /// Host-binary-compiled code: every processor the engine spawns. May mint
    /// an in-process FullAccess context via [`Self::grant_full_access`].
    TrustedInstalled,
}

impl IsolationTier {
    /// Produce a [`FullAccessGrant`] iff this tier is
    /// [`TrustedInstalled`](Self::TrustedInstalled). The
    /// [`Untrusted`](Self::Untrusted) tier returns `None`, so the untrusted
    /// dispatch path has no token to mint a
    /// [`RuntimeContextFullAccess`](super::RuntimeContextFullAccess).
    pub(crate) fn grant_full_access(self) -> Option<FullAccessGrant> {
        match self {
            Self::TrustedInstalled => Some(FullAccessGrant(())),
            Self::Untrusted => None,
        }
    }

    /// Whether this tier permits minting an in-process FullAccess context.
    ///
    /// Delegates to [`Self::grant_full_access`] so the moat predicate and the
    /// grant producer are a single source of truth — a future third tier can't
    /// desync them.
    pub fn permits_in_process_full_access(self) -> bool {
        self.grant_full_access().is_some()
    }

    /// Stable lowercase label for logs.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Untrusted => "untrusted",
            Self::TrustedInstalled => "trusted",
        }
    }
}

/// Zero-sized capability token proving an [`IsolationTier::TrustedInstalled`]
/// authorized minting a
/// [`RuntimeContextFullAccess`](super::RuntimeContextFullAccess).
///
/// Constructible **only** inside this module, and only via
/// [`IsolationTier::grant_full_access`] — the untrusted dispatch path can never
/// obtain one, so an in-process FullAccess context is unrepresentable for it by
/// construction. The field is private so no other module (in-crate or out) can
/// fabricate a grant.
pub(crate) struct FullAccessGrant(());

#[cfg(test)]
mod tests {
    use super::*;

    /// The moat: an untrusted tier can never produce a [`FullAccessGrant`];
    /// only the trusted tier can. Revert `grant_full_access` to return
    /// `Some(..)` unconditionally and the untrusted assertion below fails.
    #[test]
    fn only_the_trusted_tier_grants_full_access() {
        assert!(
            IsolationTier::TrustedInstalled
                .grant_full_access()
                .is_some(),
            "the trusted tier must mint a FullAccess grant"
        );
        assert!(
            IsolationTier::Untrusted.grant_full_access().is_none(),
            "the untrusted tier must never mint a FullAccess grant"
        );
        assert!(!IsolationTier::Untrusted.permits_in_process_full_access());
        assert!(IsolationTier::TrustedInstalled.permits_in_process_full_access());
    }
}
