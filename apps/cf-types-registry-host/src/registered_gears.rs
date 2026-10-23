// Link the gears whose `#[toolkit::gear]` inventory registrations this binary runs.
//
// Only the registry and the tenant-plane AuthN stack that turns a bearer into a
// `SecurityContext` in process: authn-resolver registers `AuthNResolverBearerAuthenticator`
// over its local `AuthNResolverClient`, which `oop_serve` installs as the tenant plane
// (SPEC §8.4, P30). No api-gateway, account-management or tenant-resolver: the HTTP surface
// is the OoP listener, and the directory is dialled remotely.
#![allow(unused_imports)]

use authn_resolver as _;
use oidc_authn_plugin as _;
use types_registry as _;

#[cfg(feature = "dev-authn")]
use static_authn_plugin as _;
