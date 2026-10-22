//! The platform REST transport (`rest-client`): the client and its private wire DTOs.
//!
//! The semantic models stay transport-free; only this module knows the wire.

mod dto;
mod platform;

pub use platform::{PLATFORM_BASE_PATH, PlatformTypesRegistryRestClient};
