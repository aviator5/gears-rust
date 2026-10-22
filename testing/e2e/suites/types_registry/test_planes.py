"""One plane per route through the real api-gateway (D17, D20)."""

import uuid

import httpx
import pytest

from .helpers import PLATFORM_API, TRACEPARENT, get_entity


def _reason(response):
    """The machine-readable reason of a canonical `401` Problem."""
    assert response.status_code == 401, response.text
    assert response.headers["content-type"].startswith("application/problem+json")
    return response.json()["context"]["reason"]


async def _send(base_url, method, path, headers, **kwargs):
    """A request carrying exactly `headers` as credentials, none from a fixture client."""
    async with httpx.AsyncClient(base_url=base_url, timeout=3.0) as bare:
        return await bare.request(
            method, path, headers={"traceparent": TRACEPARENT, **headers}, **kwargs
        )


@pytest.mark.scenario("TR-PLANE-001")
async def test_a_platform_route_serves_the_internal_token_and_refuses_a_bearer(
    registry_http, auth_headers, platform_headers, registration_fixture
):
    document = registration_fixture("person_schema")
    body = {"items": [document], "dry_run": True}
    forged = {"X-ToolKit-Internal-Token": f"forged-{uuid.uuid4()}"}
    for credentials, expected in (
        ({}, "MISSING_INTERNAL_TOKEN"),
        # The bearer is dropped unvalidated, so the token is what is missing.
        (auth_headers, "MISSING_INTERNAL_TOKEN"),
        (forged, "INTERNAL_AUTH_FAILED"),
    ):
        response = await _send(
            registry_http.base_url,
            "POST",
            f"{PLATFORM_API}/entities",
            {"Idempotency-Key": str(uuid.uuid4()), **credentials},
            json=body,
        )
        assert _reason(response) == expected, sorted(credentials)

    accepted = await _send(
        registry_http.base_url,
        "POST",
        f"{PLATFORM_API}/entities",
        {"Idempotency-Key": str(uuid.uuid4()), **platform_headers},
        json=body,
    )
    assert accepted.status_code == 202, accepted.text


@pytest.mark.scenario("TR-PLANE-002")
async def test_both_credentials_are_refused_on_either_route_set(
    registry_http,
    registry_api_path,
    auth_headers,
    platform_headers,
    registration_fixture,
    given_registered,
):
    document = registration_fixture("person_schema")
    await given_registered(document)
    for path in (
        f"{PLATFORM_API}/entities/{document['gts_id']}",
        f"{registry_api_path}/entities/{document['gts_id']}",
    ):
        response = await _send(
            registry_http.base_url, "GET", path, {**auth_headers, **platform_headers}
        )
        assert _reason(response) == "MIXED_CREDENTIALS", path


@pytest.mark.scenario("TR-PLANE-003")
async def test_a_tenant_reads_with_its_bearer_and_cannot_mutate(
    registry_http,
    registry_api_path,
    auth_headers,
    platform_headers,
    registration_fixture,
    given_registered,
):
    document = registration_fixture("person_schema")
    await given_registered(document)

    tenant = await get_entity(registry_http, registry_api_path, document["gts_id"])
    platform = await get_entity(registry_http.platform, PLATFORM_API, document["gts_id"])
    assert tenant.status_code == 200, tenant.text
    assert (tenant.json(), tenant.headers["etag"]) == (
        platform.json(),
        platform.headers["etag"],
    )

    token_only = await _send(
        registry_http.base_url,
        "GET",
        f"{registry_api_path}/entities/{document['gts_id']}",
        platform_headers,
    )
    assert token_only.status_code == 401, token_only.text

    write = await _send(
        registry_http.base_url,
        "POST",
        f"{registry_api_path}/entities",
        {"Idempotency-Key": str(uuid.uuid4()), **auth_headers},
        json={"items": [document]},
    )
    assert write.status_code in (404, 405), write.text
    after = await get_entity(
        registry_http, registry_api_path, document["gts_id"], select="origin"
    )
    assert after.json()["origin"]["resource_version"] == 1, after.text
