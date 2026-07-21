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
//! Isolation is an **opt-in feature**: by default every loaded processor —
//! including `@session/…` (in-app authored / agent-submitted) separately-built
//! (cdylib-resident) code — runs [`IsolationTier::TrustedInstalled`], with the
//! same in-process FullAccess permissions as an installed package (no behavior
//! change vs before the trust axis existed). An operator opts into sandboxing
//! submitted code by setting the session tier to `untrusted` (via
//! [`SESSION_ISOLATION_TIER_ENV`] or [`set_session_isolation_tier`]); only then
//! is a `@session/…` cdylib-resident module classified
//! [`IsolationTier::Untrusted`]. Installed, content-hash-locked packages and any
//! host-binary-compiled processor (`register::<P>()` / `add_local::<P>()`, the
//! host's own code under a `@session` namespace key) are always
//! [`IsolationTier::TrustedInstalled`] and are unaffected by the knob.
//!
//! The moat is the [`FullAccessGrant`] token: minting a
//! [`RuntimeContextFullAccess`](super::RuntimeContextFullAccess) requires one,
//! and a grant is producible **only** from [`IsolationTier::TrustedInstalled`]
//! (see [`IsolationTier::grant_full_access`]). The untrusted lifecycle dispatch
//! has no grant to pass, so an in-process FullAccess context is unrepresentable
//! for it — a compile-time guarantee, not a runtime check.
//!
//! Actual runtime *enforcement* of the untrusted tier — own-subprocess sandbox,
//! cgroup-v2 limits, narrow Deno permissions — is a separate concern
//! (isolation *enforcement*); this module owns only the policy model, the
//! process-wide config default, and the capability moat at the minting seam.

use parking_lot::RwLock;

use streamlib_idents::Org;

/// Environment variable selecting the tier assigned to `@session/…`
/// cdylib-resident (submitted-source) modules — `trusted` (default) or
/// `untrusted`. Setting it to `untrusted` is the operator's opt-in to sandbox
/// submitted code; unset leaves submitted code trusted like installed code. A
/// programmatic override ([`set_session_isolation_tier`]) takes precedence.
pub(crate) const SESSION_ISOLATION_TIER_ENV: &str = "STREAMLIB_SESSION_ISOLATION_TIER";

/// Declarative trust tier a loaded processor runs under, derived by
/// construction from module provenance.
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
    /// Session / agent-submitted, separately-built (cdylib-resident) code.
    /// Never mints an in-process FullAccess context — its privileged lifecycle
    /// is expected to run behind the subprocess sandbox (isolation enforcement).
    Untrusted,
    /// Installed, content-hash-locked package, or host-binary-compiled code.
    /// May mint an in-process FullAccess context via [`Self::grant_full_access`].
    TrustedInstalled,
}

impl IsolationTier {
    /// Derive the tier from a processor's module provenance.
    ///
    /// [`TrustedInstalled`](Self::TrustedInstalled) by default for everything —
    /// isolation is opt-in. A `@session/…` module that was separately built and
    /// dlopened (`cdylib_resident == true`) resolves through the process-wide
    /// session tier ([`session_isolation_tier`]), which is
    /// [`TrustedInstalled`](Self::TrustedInstalled) unless the operator opts into
    /// `untrusted` via [`SESSION_ISOLATION_TIER_ENV`] /
    /// [`set_session_isolation_tier`]. Everything else — installed packages (any
    /// non-session org) and host-binary-compiled processors (`register::<P>()` /
    /// `add_local::<P>()`, which are not cdylib-resident even under a `@session`
    /// key) — is always [`TrustedInstalled`](Self::TrustedInstalled).
    pub fn for_processor(org: &Org, cdylib_resident: bool) -> Self {
        if cdylib_resident && org.is_reserved_for_session() {
            session_isolation_tier()
        } else {
            Self::TrustedInstalled
        }
    }

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

    /// Stable lowercase label for logs / config round-tripping.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Untrusted => "untrusted",
            Self::TrustedInstalled => "trusted",
        }
    }

    /// Parse the `untrusted` / `trusted` spelling (case-insensitive) — the two
    /// canonical labels [`Self::as_str`] round-trips. An unrecognized value is
    /// `None` so the caller falls back to the default rather than silently
    /// widening trust.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "untrusted" => Some(Self::Untrusted),
            "trusted" => Some(Self::TrustedInstalled),
            _ => None,
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

/// Process-wide override for the tier assigned to `@session/…` cdylib-resident
/// modules. `None` falls back to [`SESSION_ISOLATION_TIER_ENV`], then the
/// [`IsolationTier::TrustedInstalled`] default (isolation is opt-in).
static SESSION_TIER_OVERRIDE: RwLock<Option<IsolationTier>> = RwLock::new(None);

/// Set (or clear, with `None`) the process-wide session isolation tier
/// override. `None` restores the env / [`IsolationTier::TrustedInstalled`]
/// default.
pub(crate) fn set_session_isolation_tier(tier: Option<IsolationTier>) {
    *SESSION_TIER_OVERRIDE.write() = tier;
}

/// The effective tier for a `@session/…` cdylib-resident module: the runtime
/// override, else the [`SESSION_ISOLATION_TIER_ENV`] env var, else
/// [`IsolationTier::TrustedInstalled`] — isolation is opt-in, so submitted code
/// runs trusted (like installed code) unless the operator opts into `untrusted`.
/// An unrecognized env value warns once per read and falls back to the trusted
/// default (a garbage value is not an opt-in to sandboxing).
pub(crate) fn session_isolation_tier() -> IsolationTier {
    if let Some(tier) = *SESSION_TIER_OVERRIDE.read() {
        return tier;
    }
    match std::env::var(SESSION_ISOLATION_TIER_ENV) {
        Ok(raw) if !raw.trim().is_empty() => match IsolationTier::parse(&raw) {
            Some(tier) => tier,
            None => {
                tracing::warn!(
                    value = %raw,
                    env = SESSION_ISOLATION_TIER_ENV,
                    "unrecognized session isolation tier — expected untrusted/trusted; \
                     defaulting to trusted"
                );
                IsolationTier::TrustedInstalled
            }
        },
        _ => IsolationTier::TrustedInstalled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib_idents::{Org, SESSION_ORG};

    fn session_org() -> Org {
        Org::new(SESSION_ORG).expect("session org is constructible")
    }

    /// Serialized (via `#[serial_test::serial]`) reset of the process-wide
    /// session-tier state shared by the three tests below. On construction and
    /// on drop it clears the override AND [`SESSION_ISOLATION_TIER_ENV`], so a
    /// panicking test can't leak `Untrusted` into the next, and a developer
    /// machine that exports the env var can't perturb the trusted-by-default
    /// assertions (which fall through to the env when the override is `None`).
    struct SessionIsolationTierTestStateGuard;

    impl SessionIsolationTierTestStateGuard {
        fn new() -> Self {
            Self::reset_session_tier_state();
            Self
        }

        fn reset_session_tier_state() {
            set_session_isolation_tier(None);
            // SAFETY: the three callers are serialized by
            // `#[serial_test::serial]`, and this env var is read only by
            // `session_isolation_tier`, which no other test touches — no
            // concurrent env access races this write.
            unsafe { std::env::remove_var(SESSION_ISOLATION_TIER_ENV) };
        }
    }

    impl Drop for SessionIsolationTierTestStateGuard {
        fn drop(&mut self) {
            Self::reset_session_tier_state();
        }
    }

    /// The moat: an untrusted tier can never produce a [`FullAccessGrant`];
    /// only the trusted tier can. Revert `grant_full_access` to return
    /// `Some(..)` unconditionally and the untrusted assertion below fails.
    #[test]
    fn only_the_trusted_tier_grants_full_access() {
        assert!(
            IsolationTier::TrustedInstalled.grant_full_access().is_some(),
            "the trusted tier must mint a FullAccess grant"
        );
        assert!(
            IsolationTier::Untrusted.grant_full_access().is_none(),
            "the untrusted tier must never mint a FullAccess grant"
        );
        assert!(!IsolationTier::Untrusted.permits_in_process_full_access());
        assert!(IsolationTier::TrustedInstalled.permits_in_process_full_access());
    }

    /// Isolation is opt-in: with no override every provenance — `@session`
    /// cdylib-resident submitted code included — derives
    /// [`IsolationTier::TrustedInstalled`], the same permissions as an installed
    /// package. Revert the `session_isolation_tier` default back to `Untrusted`
    /// and the first assertion flips.
    #[test]
    #[serial_test::serial]
    fn tier_is_derived_from_provenance() {
        let _guard = SessionIsolationTierTestStateGuard::new();

        let session = session_org();
        let installed = Org::new("tatolab").expect("valid org");

        // @session + separately-built (cdylib) → trusted by default (opt-in).
        assert_eq!(
            IsolationTier::for_processor(&session, true),
            IsolationTier::TrustedInstalled
        );
        // @session but host-compiled (not cdylib-resident) → trusted: it's the
        // host binary's own code under a @session namespace key.
        assert_eq!(
            IsolationTier::for_processor(&session, false),
            IsolationTier::TrustedInstalled
        );
        // Installed (non-session) package, however loaded → trusted.
        assert_eq!(
            IsolationTier::for_processor(&installed, true),
            IsolationTier::TrustedInstalled
        );
        assert_eq!(
            IsolationTier::for_processor(&installed, false),
            IsolationTier::TrustedInstalled
        );
    }

    /// The operator opt-in: setting the process-wide session tier to `untrusted`
    /// sandboxes `@session` cdylib submitted code, and clearing it restores the
    /// trusted (opt-out) default.
    #[test]
    #[serial_test::serial]
    fn session_override_opts_into_untrusted_and_clears() {
        let _guard = SessionIsolationTierTestStateGuard::new();

        let session = session_org();

        assert_eq!(
            IsolationTier::for_processor(&session, true),
            IsolationTier::TrustedInstalled,
            "default @session cdylib tier is trusted (isolation is opt-in)"
        );

        set_session_isolation_tier(Some(IsolationTier::Untrusted));
        assert_eq!(
            IsolationTier::for_processor(&session, true),
            IsolationTier::Untrusted,
            "the override must opt @session into untrusted (sandboxed)"
        );

        set_session_isolation_tier(None);
        assert_eq!(
            IsolationTier::for_processor(&session, true),
            IsolationTier::TrustedInstalled,
            "clearing the override restores the trusted default"
        );
    }

    /// Opt-in default in full: an `@session` cdylib-resident module with no
    /// override mints a [`FullAccessGrant`] — it runs in-process with the same
    /// FullAccess as installed code, so the interim fail-closed dead-end is gone
    /// by default. Revert the `session_isolation_tier` default to `Untrusted`
    /// and the grant vanishes.
    #[test]
    #[serial_test::serial]
    fn default_session_cdylib_mints_full_access_grant() {
        let _guard = SessionIsolationTierTestStateGuard::new();

        let session = session_org();
        let tier = IsolationTier::for_processor(&session, true);
        assert_eq!(tier, IsolationTier::TrustedInstalled);
        assert!(
            tier.grant_full_access().is_some(),
            "default @session cdylib must mint a FullAccess grant (runs in-process like installed)"
        );
        assert!(tier.permits_in_process_full_access());
    }

    #[test]
    fn parse_accepts_the_two_spellings_and_labels_round_trip() {
        assert_eq!(
            IsolationTier::parse("untrusted"),
            Some(IsolationTier::Untrusted)
        );
        assert_eq!(
            IsolationTier::parse("TRUSTED"),
            Some(IsolationTier::TrustedInstalled)
        );
        assert_eq!(IsolationTier::parse("garbage"), None);
        assert_eq!(IsolationTier::Untrusted.as_str(), "untrusted");
        assert_eq!(IsolationTier::TrustedInstalled.as_str(), "trusted");
    }
}
