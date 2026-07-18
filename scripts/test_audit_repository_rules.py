#!/usr/bin/env python3
"""Tests for audit-repository-rules.py."""

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "audit_repository_rules", Path(__file__).with_name("audit-repository-rules.py")
)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("could not load audit-repository-rules.py")
audit = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(audit)


class RepositoryRuleTests(unittest.TestCase):
    policy = {
        "required_checks": [{"workflow": "CI", "job": "CI Required"}],
        "repository_rules": {
            "enforcement": "active",
            "include": ["~DEFAULT_BRANCH"],
            "required_approving_review_count": 1,
            "dismiss_stale_reviews_on_push": True,
            "required_review_thread_resolution": True,
            "strict_required_status_checks_policy": True,
            "forbid_bypass_actors": True,
        },
    }

    @staticmethod
    def ruleset() -> dict[str, object]:
        return {
            "enforcement": "active",
            "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
            "rules": [
                {"type": "deletion"},
                {"type": "non_fast_forward"},
                {
                    "type": "pull_request",
                    "parameters": {
                        "required_approving_review_count": 1,
                        "dismiss_stale_reviews_on_push": True,
                        "required_review_thread_resolution": True,
                    },
                },
                {
                    "type": "required_status_checks",
                    "parameters": {
                        "strict_required_status_checks_policy": True,
                        "required_status_checks": [{"context": "CI Required"}],
                    },
                },
            ],
        }

    def test_accepts_matching_default_branch_rules(self) -> None:
        self.assertEqual(audit.audit(self.policy, [self.ruleset()]), [])

    def test_live_api_request_requires_and_sends_authentication(self) -> None:
        with self.assertRaisesRegex(ValueError, "authenticated GitHub token"):
            audit.api_request("https://api.github.test/rulesets", "")

        request = audit.api_request("https://api.github.test/rulesets", "test-token")
        self.assertEqual(request.get_header("Authorization"), "Bearer test-token")
        self.assertEqual(request.get_header("X-github-api-version"), audit.API_VERSION)

    def test_reports_missing_safety_rules_and_checks(self) -> None:
        ruleset = self.ruleset()
        ruleset["rules"] = []
        failures = audit.audit(self.policy, [ruleset])
        self.assertIn("missing deletion rule", failures)
        self.assertIn("missing pull_request rule", failures)
        self.assertIn("missing required_status_checks rule", failures)

    def test_rejects_empty_or_malformed_required_check_policy(self) -> None:
        for required_checks in (
            [],
            None,
            [{"workflow": "CI"}],
            [{"workflow": "CI", "job": "   "}],
        ):
            with self.subTest(required_checks=required_checks):
                policy = {**self.policy, "required_checks": required_checks}
                with self.assertRaisesRegex(ValueError, "required.check"):
                    audit.audit(policy, [self.ruleset()])

    def test_rejects_bypass_and_non_strict_checks(self) -> None:
        ruleset = self.ruleset()
        ruleset["bypass_actors"] = [{"actor_type": "OrganizationAdmin"}]
        status = next(
            rule
            for rule in ruleset["rules"]
            if rule["type"] == "required_status_checks"
        )
        status["parameters"]["strict_required_status_checks_policy"] = False
        failures = audit.audit(self.policy, [ruleset])
        self.assertIn("default-branch rulesets must not define bypass actors", failures)
        self.assertIn(
            "required status checks must require an up-to-date branch", failures
        )


if __name__ == "__main__":
    unittest.main()
