//! The REST transport (`rest-client`): both clients and their private wire DTOs.
//!
//! The semantic models stay transport-free; only this module knows the wire.

mod dto;
mod platform;
mod tenant;
mod transport;

pub use platform::{PLATFORM_BASE_PATH, PlatformTypesRegistryRestClient};
pub use tenant::TypesRegistryRestClient;
