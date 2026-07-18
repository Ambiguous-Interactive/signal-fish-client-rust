#!/usr/bin/env python3
"""Audit GitHub default-branch rulesets against the checked-in release policy."""

from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.request
from pathlib import Path
from typing import Any

API_VERSION = "2022-11-28"


def load_json(path: Path) -> Any:
    with path.open(encoding="utf-8") as stream:
        return json.load(stream)


def api_request(url: str, token: str) -> urllib.request.Request:
    if not token:
        raise ValueError(
            "an authenticated GitHub token is required for live ruleset audits"
        )
    return urllib.request.Request(
        url,
        headers={
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {token}",
            "User-Agent": "signal-fish-client-repository-policy-audit",
            "X-GitHub-Api-Version": API_VERSION,
        },
    )


def fetch_json(url: str, token: str) -> Any:
    request = api_request(url, token)
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response)


def fetch_rulesets(repository: str, token: str) -> list[dict[str, Any]]:
    base = f"https://api.github.com/repos/{repository}/rulesets"
    summaries = fetch_json(f"{base}?per_page=100", token)
    if not isinstance(summaries, list):
        raise ValueError("GitHub rulesets response was not a list")
    return [fetch_json(f"{base}/{summary['id']}", token) for summary in summaries]


def audit(policy: dict[str, Any], rulesets: list[dict[str, Any]]) -> list[str]:
    expected = policy.get("repository_rules")
    if not isinstance(expected, dict):
        raise ValueError("policy has no repository_rules object")
    expected_refs = set(expected.get("include", []))
    if not expected_refs:
        raise ValueError("repository_rules.include must not be empty")
    configured_checks = policy.get("required_checks")
    if not isinstance(configured_checks, list) or not configured_checks:
        raise ValueError("policy must define a non-empty required_checks list")
    if any(
        not isinstance(check, dict)
        or not isinstance(check.get("job"), str)
        or not check["job"]
        for check in configured_checks
    ):
        raise ValueError("every required check must define a non-empty job name")
    required_checks = {check["job"] for check in configured_checks}
    if len(required_checks) != len(configured_checks):
        raise ValueError("required check job names must be unique")
    applicable = [
        ruleset
        for ruleset in rulesets
        if ruleset.get("enforcement") == expected.get("enforcement")
        and expected_refs.issubset(
            ruleset.get("conditions", {}).get("ref_name", {}).get("include", [])
        )
    ]
    failures: list[str] = []
    if not applicable:
        return ["no active ruleset targets ~DEFAULT_BRANCH"]

    if expected.get("forbid_bypass_actors") and any(
        ruleset.get("bypass_actors") for ruleset in applicable
    ):
        failures.append("default-branch rulesets must not define bypass actors")

    rules = [rule for ruleset in applicable for rule in ruleset.get("rules", [])]
    rule_types = {rule.get("type") for rule in rules}
    for rule_type in ("deletion", "non_fast_forward"):
        if rule_type not in rule_types:
            failures.append(f"missing {rule_type} rule")

    pull_requests = [rule for rule in rules if rule.get("type") == "pull_request"]
    if not pull_requests:
        failures.append("missing pull_request rule")
    else:
        for key in (
            "dismiss_stale_reviews_on_push",
            "required_review_thread_resolution",
        ):
            actual = any(rule.get("parameters", {}).get(key) for rule in pull_requests)
            if actual is not expected.get(key):
                failures.append(f"pull_request.{key} must be {expected.get(key)}")
        approvals = max(
            rule.get("parameters", {}).get("required_approving_review_count", 0)
            for rule in pull_requests
        )
        if approvals < expected.get("required_approving_review_count", 0):
            failures.append(
                "pull_request.required_approving_review_count is below the checked-in policy"
            )

    status_rules = [
        rule for rule in rules if rule.get("type") == "required_status_checks"
    ]
    if not status_rules:
        failures.append("missing required_status_checks rule")
    else:
        actual_checks = {
            check.get("context")
            for rule in status_rules
            for check in rule.get("parameters", {}).get("required_status_checks", [])
        }
        missing = sorted(required_checks - actual_checks)
        if missing:
            failures.append("missing required status checks: " + ", ".join(missing))
        strict = any(
            rule.get("parameters", {}).get("strict_required_status_checks_policy")
            for rule in status_rules
        )
        if strict is not expected.get("strict_required_status_checks_policy"):
            failures.append("required status checks must require an up-to-date branch")
    return failures


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--policy", type=Path, default=Path(".github/required-checks.json")
    )
    parser.add_argument(
        "--repository", default="Ambiguous-Interactive/signal-fish-client-rust"
    )
    parser.add_argument("--rulesets", type=Path)
    args = parser.parse_args()
    try:
        policy = load_json(args.policy)
        token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN") or ""
        rulesets = (
            load_json(args.rulesets)
            if args.rulesets
            else fetch_rulesets(args.repository, token)
        )
        if not isinstance(policy, dict) or not isinstance(rulesets, list):
            raise ValueError("policy must be an object and rulesets must be a list")
        failures = audit(policy, rulesets)
    except (OSError, ValueError, KeyError, json.JSONDecodeError) as error:
        print(f"repository-policy error: {error}", file=sys.stderr)
        return 1
    if failures:
        print("Repository rules do not match the checked-in policy:", file=sys.stderr)
        for failure in failures:
            print(f"- {failure}", file=sys.stderr)
        return 1
    print("Repository rules match the checked-in policy.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
