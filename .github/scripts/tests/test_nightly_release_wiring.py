from __future__ import annotations

import json
import yaml
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
WORKFLOWS = ROOT / ".github/workflows"
TAG_PUSH = "github.event_name == 'push' && startsWith(github.ref, 'refs/tags/cua-driver-rs-v')"


def source(name: str) -> str:
    return (WORKFLOWS / name).read_text()


def test_driver_stable_publish_gate_and_workflow_name_are_frozen():
    driver = source("cd-rust-cua-driver.yml")
    assert driver.startswith('name: "CD: Cua Driver (cross-platform)"')
    release = driver.split("\n  release:\n", 1)[1].split("    steps:\n", 1)[0]
    # Only the stable tag push publishes. Nightly workflow_call runs inherit
    # the caller's schedule/workflow_dispatch event and can never match.
    assert (
        "    if: github.event_name == 'push' && "
        "startsWith(github.ref, 'refs/tags/cua-driver-rs-v')\n"
    ) in release
    assert "inputs.publish" not in driver
    sdk = source("cd-py-cua-driver.yml")
    assert 'workflows: ["CD: Cua Driver (cross-platform)"]' in sdk


def test_driver_release_jobs_are_disabled_outside_upstream():
    jobs = yaml.safe_load(source("cd-rust-cua-driver.yml"))["jobs"]
    roots = {name for name, job in jobs.items() if "needs" not in job}
    assert roots == {"release-attribution-preflight", "request-staging-smoke", "staging-gate"}
    for name in roots:
        assert "github.repository == 'trycua/cua'" in jobs[name]["if"]
    for name, job in jobs.items():
        if name not in roots:
            assert "always()" not in job.get("if", "")


def test_driver_sdk_release_is_disabled_outside_upstream():
    jobs = yaml.safe_load(source("cd-py-cua-driver.yml"))["jobs"]
    assert {name for name, job in jobs.items() if "needs" not in job} == {"get-version"}
    assert "github.repository == 'trycua/cua'" in jobs["get-version"]["if"]
    assert all("always()" not in job.get("if", "") for job in jobs.values())


def test_fork_driver_nightly_runs_desktop_lanes_without_publishing():
    nightly = yaml.safe_load(source("nightly-cua-driver.yml"))
    assert nightly["permissions"] == {"actions": "read", "contents": "read"}
    assert set(nightly["jobs"]) == {"linux", "windows", "browsers"}
    for job, called in (
        ("linux", "e2e-rust-linux.yml"),
        ("windows", "e2e-rust-windows.yml"),
        ("browsers", "e2e-rust-standalone-browsers.yml"),
    ):
        lane = nightly["jobs"][job]
        assert lane["uses"] == f"./.github/workflows/{called}"
        assert lane["with"]["ref"] == "${{ github.sha }}"


def test_lume_stable_publish_gate_remains_tag_only():
    lume = source("cd-swift-lume.yml")
    assert "if: startsWith(github.ref, 'refs/tags/lume-v')" in lume
    assert (
        "BUNDLE_VERSION: ${{ inputs.bundle_version || steps.set_version.outputs.version }}" in lume
    )


def test_lume_nightly_reuses_notarized_builder_and_never_becomes_latest():
    nightly = source("nightly-lume.yml")
    builder = source("cd-swift-lume.yml")
    assert "uses: ./.github/workflows/cd-swift-lume.yml" in nightly
    assert "bundle_version: ${{ needs.plan.outputs.bundle_version }}" in nightly
    assert "--create-if-missing" in nightly
    assert "--prerelease" in nightly
    assert "--make-latest" not in nightly
    assert "Collect PR-first attribution and render nightly body" in nightly
    assert "GH_TOKEN: ${{ github.token }}" in nightly
    assert "needs.plan.outputs.attribution_base_tag" in nightly
    assert "issues: write" in nightly
    assert "pull-requests: read" in nightly
    assert builder.index("- name: Set version") < builder.index(
        "- name: Stage nightly artifact version"
    )
    set_version = builder[
        builder.index("- name: Set version") : builder.index(
            "- name: Stage nightly artifact version"
        )
    ]
    assert set_version.index('inputs.channel }}" == "nightly"') < set_version.index(
        "SOURCE_VERSION=$(tr -d"
    )


def test_planner_requires_main_ancestry_and_preserves_immutable_evidence():
    planner = source("nightly-component-plan.yml")
    assert "SOURCE_SHA: ${{ inputs.source_sha }}" in planner
    assert 'git merge-base --is-ancestor "$SOURCE_SHA" origin/main' in planner
    assert "fetch-depth: 0" in planner
    assert "nightly-plan-${{ inputs.component }}" in planner
    assert "attribution_base_tag" in planner
    assert "attribution_issues" in planner
    assert "--attribution-config .github/release-attribution-config.json" in planner
    assert "held-attribution" not in planner
    assert "report-attribution-warning" in planner
    assert "needs.plan.outputs.attribution_issues != '[]'" in planner
    assert "GITHUB_STEP_SUMMARY" in planner
    # One rolling issue; a failed report must never fail the reusable planner.
    assert (
        "name: Refresh the rolling unresolved-attribution issue\n        continue-on-error: true"
        in planner
    )
    assert "release_attribution.py report-unresolved" in planner
    assert "cancel-in-progress: false" in source("nightly-cua-driver.yml")
    assert "cancel-in-progress: false" in source("nightly-lume.yml")


def test_registered_builder_workflows_match_the_orchestrators():
    components = json.loads((ROOT / ".github/releases/components.json").read_text())["components"]
    assert components["cua-driver-rs"]["builderWorkflow"] == (
        ".github/workflows/cd-rust-cua-driver.yml"
    )
    assert components["lume"]["builderWorkflow"] == ".github/workflows/cd-swift-lume.yml"
    for component in components.values():
        assert (ROOT / component["builderWorkflow"]).is_file()


def test_driver_nightly_version_is_staged_in_each_platform_builder():
    driver = source("cd-rust-cua-driver.yml")
    command = "release_channels.py apply-version --component cua-driver-rs --version"
    assert driver.count(command) == 3
    assert "inputs.source_ref || github.workflow_sha" in driver
    assert 'BUNDLE_VERSION="${VERSION%%-*}"' in driver
    assert 'CFBundleShortVersionString -string "$BUNDLE_VERSION"' in driver
    assert 'CFBundleVersion -string "$BUNDLE_VERSION"' in driver


def test_nightly_workflow_names_cannot_trigger_stable_driver_sdk_publish():
    for name in ("nightly-cua-driver.yml", "nightly-lume.yml"):
        first_line = source(name).splitlines()[0]
        assert "CD: Cua Driver (cross-platform)" not in first_line


def test_release_control_ci_runs_for_every_nightly_definition():
    test_workflow = source("ci-test-scripts.yml")
    for path in (
        ".github/releases/**",
        ".github/workflows/nightly-component-plan.yml",
        ".github/workflows/nightly-cua-driver.yml",
        ".github/workflows/nightly-lume.yml",
    ):
        assert f'- "{path}"' in test_workflow
