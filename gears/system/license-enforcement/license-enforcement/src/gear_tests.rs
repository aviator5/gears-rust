#![allow(clippy::expect_used)]

use license_resolver_sdk::LicenseResolverPluginSpecV1;
use serde_json::{Value, json};
use toolkit::config::ConfigProvider;
use toolkit::gts::PluginV1;
use toolkit::gts_publication::GtsPublicationCapability;

use super::{INSTANCE_SUFFIX, LicenseEnforcement};

struct Config(Value);

impl ConfigProvider for Config {
    fn get_gear_config(&self, gear: &str) -> Option<&Value> {
        self.0.get(gear)
    }
}

/// The runtime publishes the SDK's recipient projection and this gear's
/// license-resolver backend instance, with the configured vendor and
/// priority, before `init`.
#[test]
fn the_manifest_is_the_recipient_projection_and_the_configured_backend_instance() {
    let config = Config(json!({
        "license-enforcement": { "config": { "vendor": "acme", "priority": 7 } }
    }));
    let manifest = LicenseEnforcement::default()
        .descriptor()
        .build_gts_manifest(&config)
        .expect("manifest");

    let (expected, document) =
        PluginV1::<LicenseResolverPluginSpecV1>::build_registration(INSTANCE_SUFFIX, "acme", 7)
            .expect("backend instance");
    let documents = manifest.documents();
    let ids: Vec<&str> = documents.iter().map(|(id, _)| id.as_ref()).collect();
    let mut want = vec![
        expected.as_ref(),
        "gts.cf.core.license_enforcement.recipient.v1~",
    ];
    want.sort_unstable();
    assert_eq!(ids, want);
    let backend = documents
        .iter()
        .find(|(id, _)| id.as_ref() == expected.as_ref())
        .expect("backend instance");
    assert_eq!(backend.1, document);
    assert_eq!(manifest.publisher_name(), "license-enforcement");
}

/// An invalid configuration is never advertised as a backend.
#[test]
fn an_invalid_configuration_publishes_nothing() {
    let config = Config(json!({
        "license-enforcement": { "config": { "idempotency_window_secs": 0 } }
    }));
    assert!(
        LicenseEnforcement::default()
            .descriptor()
            .build_gts_manifest(&config)
            .is_err()
    );
}
