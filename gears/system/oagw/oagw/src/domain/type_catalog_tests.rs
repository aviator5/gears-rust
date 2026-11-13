use serde_json::Value;
use toolkit_gts::GTS_ID_URI_PREFIX;

use crate::domain::gts_helpers::*;

/// `(GTS id, document)` of this crate's collector.
fn declarations() -> Vec<(String, Value)> {
    crate::gts_declarations()
        .into_iter()
        .map(|(id, text)| {
            let document = serde_json::from_str(&text).expect("declared JSON");
            (id, document)
        })
        .collect()
}

fn is_type(id: &str) -> bool {
    id.ends_with('~')
}

/// The collector declares exactly the catalog the rest of OAGW names.
#[test]
fn the_collector_declares_the_oagw_catalog() {
    let mut expected = vec![
        UPSTREAM_SCHEMA,
        ROUTE_SCHEMA,
        PROTOCOL_SCHEMA,
        AUTH_PLUGIN_SCHEMA,
        GUARD_PLUGIN_SCHEMA,
        TRANSFORM_PLUGIN_SCHEMA,
        PROXY_SCHEMA,
        HTTP_PROTOCOL_ID,
        GRPC_PROTOCOL_ID,
        NOOP_AUTH_PLUGIN_ID,
        APIKEY_AUTH_PLUGIN_ID,
        BASIC_AUTH_PLUGIN_ID,
        BEARER_AUTH_PLUGIN_ID,
        OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID,
        OAUTH2_CLIENT_CRED_BASIC_AUTH_PLUGIN_ID,
        TIMEOUT_GUARD_PLUGIN_ID,
        CORS_GUARD_PLUGIN_ID,
        REQUIRED_HEADERS_GUARD_PLUGIN_ID,
        LOGGING_TRANSFORM_PLUGIN_ID,
        METRICS_TRANSFORM_PLUGIN_ID,
        REQUEST_ID_TRANSFORM_PLUGIN_ID,
    ];
    expected.sort_unstable();

    let declared = declarations();
    let ids: Vec<&str> = declared.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, expected);
}

/// Managed admission (step 5) requires a Draft-07 schema whose `$id` is the
/// `gts://` URI of the registered identifier. The schemas stay open so
/// upstream and route payloads keep their own fields; an Instance body names
/// its identifier in `id` (the GTS base-type contract).
#[test]
fn every_schema_is_open_draft_07_and_names_its_identifier() {
    for (id, document) in declarations().iter().filter(|(id, _)| is_type(id)) {
        assert_eq!(document["$id"], format!("{GTS_ID_URI_PREFIX}{id}"), "{id}");
        assert_eq!(
            document["$schema"], "http://json-schema.org/draft-07/schema#",
            "{id}"
        );
        assert_eq!(document["additionalProperties"], true, "{id}: {document}");
        assert_eq!(
            document["required"],
            serde_json::json!(["id"]),
            "{id}: {document}"
        );
    }
}

/// The managed identifier profile refuses an explicit UUID tail.
#[test]
fn no_identifier_ends_in_a_uuid() {
    for (id, _) in declarations() {
        let tail = id.rsplit('~').next().unwrap_or_default();
        assert!(uuid::Uuid::parse_str(tail).is_err(), "{id}");
    }
}
