//! Canonical `FailedPrecondition` violation types.
//!
//! Legacy parent refusals use [`PARENT_NOT_REGISTERED`] as the violation type,
//! the parent Type Schema id as its subject, and the dependent id as `resource_name`.
//! The violation description carries the message.
//!
//! Policy refusals use [`REGISTRATION_POLICY_PREFIX`] and [`PolicyParameter`];
//! item failures use [`crate::item_failure::AdmissionFailure`]. Each decoder
//! accepts only its own shape.

/// Violation type for [`crate::TypesRegistryError::ParentNotRegistered`].
pub const PARENT_NOT_REGISTERED: &str = "PARENT_NOT_REGISTERED";

/// Prefix of the `violations[].type` of a registration-policy refusal; the rest is the
/// refused policy parameter in upper case (`REGISTRATION_POLICY_ALLOWED_VENDORS`).
///
/// The refusal names the candidate in `resource_name` and the policy region (or
/// [`DEFAULT_REGION`]) in `violations[].subject`.
pub const REGISTRATION_POLICY_PREFIX: &str = "REGISTRATION_POLICY_";

/// The `violations[].subject` of a policy refusal made by the default policy, which
/// belongs to no named region.
pub const DEFAULT_REGION: &str = "<default>";

/// The candidate's vendor is not in the region's `allowed_vendors`.
pub const REGISTRATION_POLICY_ALLOWED_VENDORS: &str = "REGISTRATION_POLICY_ALLOWED_VENDORS";

/// The candidate is not allowed by the region's `tenant_ownable` setting.
pub const REGISTRATION_POLICY_TENANT_OWNABLE: &str = "REGISTRATION_POLICY_TENANT_OWNABLE";

/// Typed view of a registration-policy `violations[].type`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyParameter {
    /// See [`REGISTRATION_POLICY_ALLOWED_VENDORS`].
    AllowedVendors,
    /// See [`REGISTRATION_POLICY_TENANT_OWNABLE`].
    TenantOwnable,
    /// A policy-prefixed code this build does not know; the full code is preserved.
    Unknown(String),
}

impl PolicyParameter {
    /// Project a `violations[].type`; `None` unless it carries [`REGISTRATION_POLICY_PREFIX`].
    #[must_use]
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            REGISTRATION_POLICY_ALLOWED_VENDORS => Some(Self::AllowedVendors),
            REGISTRATION_POLICY_TENANT_OWNABLE => Some(Self::TenantOwnable),
            other if other.starts_with(REGISTRATION_POLICY_PREFIX) => {
                Some(Self::Unknown(other.to_owned()))
            }
            _ => None,
        }
    }

    /// Render back to the wire `violations[].type`. Inverse of [`Self::from_wire`].
    #[must_use]
    pub fn as_wire(&self) -> &str {
        match self {
            Self::AllowedVendors => REGISTRATION_POLICY_ALLOWED_VENDORS,
            Self::TenantOwnable => REGISTRATION_POLICY_TENANT_OWNABLE,
            Self::Unknown(s) => s.as_str(),
        }
    }
}

impl core::fmt::Display for PolicyParameter {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_wire())
    }
}
