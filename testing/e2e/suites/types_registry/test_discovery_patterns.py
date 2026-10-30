"""HTTP identifier selectors from gts-spec §10 / shared_id_pattern_cases.py.

Expected sets are authored here, independently of the registry matcher. Managed
families cannot mix major-only and minor-bearing schemas for the same major, so
v1 uses explicit minors and v2 uses the major-only spelling. Managed instances
require a major-only final segment and reject explicit UUID tails.
"""

import pytest

from .helpers import assert_pages, mandatory, managed, paged, schema_with_id, walk

UUID = "7a1d2f34-5678-49ab-9012-abcdef123456"


@pytest.fixture
async def pattern_entities(discovery_fixture, given_registered):
    template = discovery_fixture("person_schema")
    prefix = template["gts_id"].removesuffix("person.v1~")
    segment = prefix.removeprefix("gts.")
    documents = {}

    def schema(name, identifier, parent=None):
        document = schema_with_id(template, identifier)
        if parent is not None:
            document["content"]["allOf"] = [{"$ref": f"gts://{parent}"}]
        documents[name] = document
        return identifier

    def instance(name, parent, tail):
        documents[name] = {
            "gts_id": parent + tail,
            "content": {"name": "Alice", "payload": {}},
        }

    for version in ("0.0", "0.1", "1.0", "1.1", "1.2", "2", "10"):
        root = schema(f"root_{version}", f"{prefix}person.v{version}~")
        if not version.startswith("0."):
            instance(f"direct_{version}", root, f"{segment}alice.v1")
    schema("name_boundary", f"{prefix}person_extra.v1~")
    for minor in (0, 1):
        root = documents[f"root_1.{minor}"]["gts_id"]
        for derived_minor in (0, 1):
            branch = schema(
                f"branch_{minor}_{derived_minor}",
                root + f"{segment}employee.v1.{derived_minor}~",
                root,
            )
            instance(f"leaf_{minor}_{derived_minor}", branch, f"{segment}item.v2")
        instance(f"named_{minor}_10", root, f"{segment}alice.v10")
        instance(f"name_boundary_{minor}", root, f"{segment}alice_extra.v1")
    root = documents["root_1.2"]["gts_id"]
    branch = schema("branch_omitted", root + f"{segment}employee.v1~", root)
    instance("leaf_omitted", branch, f"{segment}item.v2")
    root = documents["root_2"]["gts_id"]
    schema("branch_other_major", root + f"{segment}employee.v1.0~", root)
    await given_registered(*documents.values())
    return prefix, segment, documents


async def check_selectors(client, api_path, documents, selectors):
    """Compare complete pages, including exclusions and continuation at limit 2."""
    for pattern, names in selectors:
        expected = sorted((documents[name] for name in names), key=lambda d: d["gts_id"])
        pages = await walk(client, api_path, {"pattern": pattern, "limit": 2})
        try:
            assert_pages(
                pages,
                paged([{**mandatory(d), "origin": managed(1)} for d in expected], 2),
            )
        except AssertionError as error:
            raise AssertionError(f"pattern={pattern}: {error}") from error


@pytest.mark.scenario("TR-DISC-104")
async def test_type_selectors_respect_versions_and_token_boundaries(
    registry_http, registry_api_path, pattern_entities
):
    """Concrete types and chain wildcards agree; family/version wildcards span majors."""
    prefix, segment, documents = pattern_entities
    v1 = [
        "root_1.0", "root_1.1", "root_1.2",
        "direct_1.0", "direct_1.1", "direct_1.2",
        "branch_0_0", "branch_0_1", "branch_1_0", "branch_1_1",
        "leaf_0_0", "leaf_0_1", "leaf_1_0", "leaf_1_1",
        "named_0_10", "named_1_10", "name_boundary_0", "name_boundary_1",
        "branch_omitted", "leaf_omitted",
    ]
    family = v1 + [
        "root_0.0", "root_0.1", "root_2", "direct_2", "root_10", "direct_10",
        "branch_other_major",
    ]
    await check_selectors(registry_http, registry_api_path, documents, [
        (prefix + "person.v1~", v1),
        (prefix + "person.v1~*", v1),
        (prefix + "person.v1.*", v1),
        (prefix + "person.*", family),
        (prefix + "person.v*", family),
        (prefix + "person.v2.*", []),
        (prefix + "person.v2~*", ["root_2", "direct_2", "branch_other_major"]),
        (prefix + "person.v2~" + segment + "*", ["direct_2", "branch_other_major"]),
    ])


@pytest.mark.scenario("TR-DISC-105")
async def test_pinned_type_minor_and_major_zero_select_their_own_scope(
    registry_http, registry_api_path, pattern_entities
):
    """Pinned minor excludes other minors; zero obeys the same selection rules."""
    prefix, _, documents = pattern_entities
    pinned = ["root_1.0", "direct_1.0",
              "branch_0_0", "leaf_0_0", "branch_0_1", "leaf_0_1",
              "named_0_10", "name_boundary_0"]
    zero = ["root_0.0", "root_0.1"]
    await check_selectors(registry_http, registry_api_path, documents, [
        (prefix + "person.v1.0~", pinned),
        (prefix + "person.v1.0~*", pinned),
        (prefix + "person.v0~*", zero),
        (prefix + "person.v0.*", zero),
        (prefix + "person.v0.1~*", ["root_0.1"]),
    ])


@pytest.mark.scenario("TR-DISC-106")
async def test_instance_selectors_keep_kind_names_and_chain_length(
    registry_http, registry_api_path, pattern_entities
):
    """Flexible base minors do not admit other names, type branches or major 10."""
    prefix, segment, documents = pattern_entities
    selector = prefix + "person.v1~" + segment + "alice.v1"
    await check_selectors(registry_http, registry_api_path, documents, [
        (selector, ["direct_1.0", "direct_1.1", "direct_1.2"]),
        (prefix + "person.v1~" + segment + "employee.v1", []),
        (prefix + "person.v1~" + segment + "employee.v1.0", []),
        (prefix + "person.v1.0~" + segment + "alice.v1", ["direct_1.0"]),
        (selector + ".0", []),
        (prefix + "person.v1.0~" + segment + "alice.v1.0", []),
    ])


@pytest.mark.scenario("TR-DISC-107")
async def test_later_minor_and_chain_wildcards_require_different_version_shapes(
    registry_http, registry_api_path, pattern_entities
):
    """A later v1.* excludes the omitted-minor branch selected by v1~*."""
    prefix, segment, documents = pattern_entities
    branch = prefix + "person.v1~" + segment + "employee.v1"
    explicit_subtree = [
        f"{kind}_{base}_{minor}"
        for base in (0, 1)
        for minor in (0, 1)
        for kind in ("branch", "leaf")
    ]
    await check_selectors(registry_http, registry_api_path, documents, [
        (branch + ".*", explicit_subtree),
        (branch + "~*", explicit_subtree + ["branch_omitted", "leaf_omitted"]),
        (branch + "~", explicit_subtree + ["branch_omitted", "leaf_omitted"]),
        (branch + ".0~", ["branch_0_0", "leaf_0_0", "branch_1_0", "leaf_1_0"]),
        (prefix + "person.v1~" + segment + "*", explicit_subtree + [
            "direct_1.0", "direct_1.1", "direct_1.2", "named_0_10", "named_1_10",
            "name_boundary_0", "name_boundary_1", "branch_omitted", "leaf_omitted"]),
        ("gts." + segment + "employee.v1~", []),
    ])


@pytest.mark.scenario("TR-DISC-108")
async def test_three_segment_selectors_pin_each_segment_independently(
    registry_http, registry_api_path, pattern_entities
):
    """Earlier omitted minors stay flexible when a later minor is pinned."""
    prefix, segment, documents = pattern_entities
    root = prefix + "person.v1~"
    branch = segment + "employee.v1~"
    tail = segment + "item.v2"
    await check_selectors(registry_http, registry_api_path, documents, [
        (root + branch + tail, ["leaf_0_0", "leaf_0_1", "leaf_1_0", "leaf_1_1", "leaf_omitted"]),
        (root + segment + "employee.v1.0~" + tail, ["leaf_0_0", "leaf_1_0"]),
        (prefix + "person.v1.0~" + branch + tail, ["leaf_0_0", "leaf_0_1"]),
        (root + branch + tail + ".0", []),
        (prefix + "person.v1.0~" + segment + "employee.v1.0~" + tail, ["leaf_0_0"]),
    ])


@pytest.mark.scenario("TR-DISC-109")
async def test_unregistered_uuid_selectors_are_valid_and_exclude_named_instances(
    registry_http, registry_api_path, pattern_entities
):
    """UUID selectors are accepted, although managed UUID tails cannot be registered."""
    prefix, _, documents = pattern_entities
    await check_selectors(registry_http, registry_api_path, documents, [
        (prefix + "person.v1~" + UUID, []),
        (prefix + "person.v1.0~" + UUID, []),
        (prefix + "person.v2~" + UUID, []),
    ])
