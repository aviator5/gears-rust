//! Shared REST path constants.

/// In-memory contract path, retired with its repository at T38.
pub const V1: &str = "/types-registry/v1";

/// Interim tenant read path; T38 promotes it to [`V1`] while retiring the in-memory API.
pub const V2: &str = "/types-registry/v2";

/// The platform API: every operation, for callers holding a validated internal token (D17).
pub const PLATFORM_V1: &str = "/types-registry/platform/v1";
