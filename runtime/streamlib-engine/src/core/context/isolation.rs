// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationTier {
    /// Separately built, untrusted code. Never mints an in-process FullAccess
    /// context — its privileged lifecycle is expected to run behind the
    /// subprocess sandbox (isolation enforcement). Nothing the engine can spawn
    /// today classifies here.
    Untrusted,
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
