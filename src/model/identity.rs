//! Instance identity is process-local; hardware evidence is optional and portable.

use std::{
    collections::BTreeSet,
    fmt,
    sync::atomic::{AtomicU64, Ordering},
};

use super::{ModelError, Result};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn allocate(counter: &AtomicU64) -> Result<u64> {
    // The counter publishes no model state; relaxed ordering is enough for
    // uniqueness. Checked advancement leaves exhaustion permanent, never wrapping.
    // Use the compare/exchange primitive available on the project's MSRV rather
    // than its newer convenience API (the old spelling is now deprecated).
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        let next = current.checked_add(1).ok_or(ModelError::IdExhausted)?;
        match counter.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(id) => return Ok(id),
            Err(actual) => current = actual,
        }
    }
}

macro_rules! runtime_id {
    ($($name:ident),+ $(,)?) => {$ (
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub(crate) struct $name(u64);

        impl $name {
            pub(super) fn allocate() -> Result<Self> {
                allocate(&NEXT_ID).map(Self)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    )+};
}

runtime_id!(
    KeyboardId,
    PhysicalDeviceId,
    InterfaceId,
    EndpointId,
    ComponentId,
    AttachmentGeneration,
    MembershipGeneration
);

/// Group identifiers describe association, never individual hardware identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum IdentityScope {
    Unit,
    Group,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verification {
    Advertised,
    Verified,
}

/// The issuer namespace must include any model/transport scoping needed to make
/// its unit identifiers comparable; a bare USB serial string is not enough.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IdentityClaim {
    namespace: String,
    value: String,
    scope: IdentityScope,
    verification: Verification,
}

impl IdentityClaim {
    pub(crate) fn new(
        namespace: impl Into<String>,
        value: impl Into<String>,
        scope: IdentityScope,
        verification: Verification,
    ) -> Result<Self> {
        Ok(Self {
            namespace: nonblank(namespace.into(), "identity namespace")?,
            value: nonblank(value.into(), "identity value")?,
            scope,
            verification,
        })
    }

    pub(crate) fn namespace(&self) -> &str {
        &self.namespace
    }
    pub(crate) fn value(&self) -> &str {
        &self.value
    }
    pub(crate) fn scope(&self) -> IdentityScope {
        self.scope
    }
    pub(crate) fn verification(&self) -> Verification {
        self.verification
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IdentityAbsence {
    NotReported,
    NotYetEstablished,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Evidence {
    Missing(IdentityAbsence),
    Claims(Vec<IdentityClaim>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HardwareIdentity(Evidence);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IdentityComparison {
    Matching,
    Conflicting,
    Insufficient,
}

impl HardwareIdentity {
    pub(crate) fn missing(reason: IdentityAbsence) -> Self {
        Self(Evidence::Missing(reason))
    }

    pub(crate) fn from_claims(claims: Vec<IdentityClaim>) -> Result<Self> {
        if claims.is_empty() {
            return Err(ModelError::EmptyValue {
                field: "identity claims",
            });
        }
        let mut keys = BTreeSet::new();
        for claim in &claims {
            if !keys.insert((claim.scope, &claim.namespace)) {
                return Err(ModelError::DuplicateClaim {
                    namespace: claim.namespace.clone(),
                });
            }
        }
        Ok(Self(Evidence::Claims(claims)))
    }

    pub(crate) fn claims(&self) -> &[IdentityClaim] {
        match &self.0 {
            Evidence::Claims(claims) => claims,
            Evidence::Missing(_) => &[],
        }
    }

    pub(crate) fn absence(&self) -> Option<IdentityAbsence> {
        match self.0 {
            Evidence::Missing(reason) => Some(reason),
            Evidence::Claims(_) => None,
        }
    }

    pub(crate) fn has_verified_unit_claim(&self) -> bool {
        self.claims().iter().any(|claim| {
            claim.scope == IdentityScope::Unit && claim.verification == Verification::Verified
        })
    }

    pub(crate) fn compare(&self, other: &Self) -> IdentityComparison {
        let mut matching = false;
        for left in self
            .claims()
            .iter()
            .filter(|c| c.scope == IdentityScope::Unit && c.verification == Verification::Verified)
        {
            for right in other.claims().iter().filter(|c| {
                c.scope == IdentityScope::Unit
                    && c.verification == Verification::Verified
                    && c.namespace == left.namespace
            }) {
                // A shared claim must not hide another incompatible unit claim.
                if left.value != right.value {
                    return IdentityComparison::Conflicting;
                }
                matching = true;
            }
        }
        if matching {
            IdentityComparison::Matching
        } else {
            IdentityComparison::Insufficient
        }
    }
}

/// Names and locators must be useful to people, but identity values are preserved
/// exactly: trimming an identifier could make two different hardware claims equal.
pub(super) fn nonblank(value: String, field: &'static str) -> Result<String> {
    if value.trim().is_empty() {
        Err(ModelError::EmptyValue { field })
    } else {
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhausted_identifiers_never_wrap_or_become_available_again() {
        let counter = AtomicU64::new(u64::MAX - 1);
        assert_eq!(allocate(&counter), Ok(u64::MAX - 1));
        assert_eq!(allocate(&counter), Err(ModelError::IdExhausted));
        assert_eq!(allocate(&counter), Err(ModelError::IdExhausted));
    }

    #[test]
    fn concurrent_allocations_do_not_repeat_identifiers() {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(|| {
                    (0..100)
                        .map(|_| PhysicalDeviceId::allocate().unwrap())
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let ids: BTreeSet<_> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        assert_eq!(ids.len(), 800);
    }
}
