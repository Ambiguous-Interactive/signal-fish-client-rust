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
            "target": "branch",
            "enforcement": "active",
            "include": ["~DEFAULT_BRANCH"],
            "exclude": [],
            "required_approving_review_count": 1,
            "dismiss_stale_reviews_on_push": True,
            "required_review_thread_resolution": True,
            "strict_required_status_checks_policy": True,
        },
    }

    @staticmethod
    def ruleset() -> dict[str, object]:
        return {
            "target": "branch",
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

    def test_rejects_ruleset_that_excludes_the_default_branch(self) -> None:
        # include + exclude of the same ref protects nothing: exclusions win.
        inert = self.ruleset()
        inert["conditions"]["ref_name"]["exclude"] = ["~DEFAULT_BRANCH"]
        self.assertEqual(
            audit.audit(self.policy, [inert]),
            ["no active ruleset targets ~DEFAULT_BRANCH"],
        )

    def test_rejects_every_exclusion_not_in_the_checked_in_policy(self) -> None:
        ruleset = self.ruleset()
        ruleset["conditions"]["ref_name"]["exclude"] = ["refs/heads/dev"]
        self.assertEqual(
            audit.audit(self.policy, [ruleset]),
            ["no active ruleset targets ~DEFAULT_BRANCH"],
        )

    def test_rejects_explicit_default_branch_exclusion(self) -> None:
        ruleset = self.ruleset()
        ruleset["conditions"]["ref_name"]["exclude"] = ["refs/heads/main"]
        self.assertEqual(
            audit.audit(self.policy, [ruleset]),
            ["no active ruleset targets ~DEFAULT_BRANCH"],
        )

    def test_rejects_tag_ruleset_for_default_branch_policy(self) -> None:
        ruleset = self.ruleset()
        ruleset["target"] = "tag"
        self.assertEqual(
            audit.audit(self.policy, [ruleset]),
            ["no active ruleset targets ~DEFAULT_BRANCH"],
        )

    def test_wildcard_exclude_patterns_fail_closed(self) -> None:
        # A pattern like refs/heads/ma* can exclude the default branch while
        # not literally listing it; the audit does not model globs, so the
        # ruleset must not count as applicable.
        for pattern in ("refs/heads/ma*", "refs/heads/???", "refs/heads/[m]ain"):
            with self.subTest(pattern=pattern):
                inert = self.ruleset()
                inert["conditions"]["ref_name"]["exclude"] = [pattern]
                self.assertEqual(
                    audit.audit(self.policy, [inert]),
                    ["no active ruleset targets ~DEFAULT_BRANCH"],
                )

    def test_exclude_all_alias_fails_closed(self) -> None:
        # Excluding ~ALL excludes every ref, including the default branch the
        # include lists; the ruleset protects nothing.
        inert = self.ruleset()
        inert["conditions"]["ref_name"]["exclude"] = ["~ALL"]
        self.assertEqual(
            audit.audit(self.policy, [inert]),
            ["no active ruleset targets ~DEFAULT_BRANCH"],
        )

    def test_missing_conditions_do_not_crash_the_audit(self) -> None:
        ruleset = self.ruleset()
        ruleset["conditions"] = None
        self.assertEqual(
            audit.audit(self.policy, [ruleset]),
            ["no active ruleset targets ~DEFAULT_BRANCH"],
        )

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

    def test_rejects_repository_rule_fields_the_token_cannot_audit(self) -> None:
        policy = {
            **self.policy,
            "repository_rules": {
                **self.policy["repository_rules"],
                "forbid_bypass_actors": True,
            },
        }
        with self.assertRaisesRegex(ValueError, "unsupported repository_rules keys"):
            audit.audit(policy, [self.ruleset()])

    def test_rejects_non_strict_checks(self) -> None:
        ruleset = self.ruleset()
        status = next(
            rule
            for rule in ruleset["rules"]
            if rule["type"] == "required_status_checks"
        )
        status["parameters"]["strict_required_status_checks_policy"] = False
        failures = audit.audit(self.policy, [ruleset])
        self.assertIn(
            "required status checks must require an up-to-date branch", failures
        )

    def test_rejects_policy_split_across_multiple_rulesets(self) -> None:
        review_ruleset = self.ruleset()
        review_ruleset["name"] = "reviews-only"
        review_ruleset["rules"] = review_ruleset["rules"][:-1]
        status_ruleset = self.ruleset()
        status_ruleset["name"] = "checks-only"
        status_ruleset["rules"] = status_ruleset["rules"][-1:]
        self.assertIn(
            "no single default-branch ruleset satisfies the complete checked-in policy",
            audit.audit(self.policy, [review_ruleset, status_ruleset]),
        )


class ReleaseEnvironmentPolicyTests(unittest.TestCase):
    policy = {
        "required_checks": [{"workflow": "CI", "job": "CI Required"}],
        "repository_rules": {
            "target": "branch",
            "enforcement": "active",
            "include": ["~DEFAULT_BRANCH"],
            "exclude": [],
            "required_approving_review_count": 0,
        },
        "release_environment": {
            "name": "crates-io",
            "required_reviewers": 1,
            "protected_branches": True,
        },
    }

    def test_accepts_the_documented_policy(self) -> None:
        self.assertEqual(
            audit.release_environment_policy(self.policy),
            self.policy["release_environment"],
        )

    def test_rejects_a_policy_without_an_environment_block(self) -> None:
        for release_environment in (None, "crates-io", [], {}):
            with self.subTest(release_environment=release_environment):
                policy = {**self.policy, "release_environment": release_environment}
                with self.assertRaisesRegex(
                    ValueError, "release_environment"
                ):
                    audit.release_environment_policy(policy)

    def test_rejects_unsupported_environment_keys(self) -> None:
        policy = {
            **self.policy,
            "release_environment": {
                **self.policy["release_environment"],
                "can_admins_bypass": False,
            },
        }
        with self.assertRaisesRegex(ValueError, "unsupported release_environment"):
            audit.release_environment_policy(policy)

    def test_rejects_empty_or_missing_environment_name(self) -> None:
        for name in (None, "", "   "):
            with self.subTest(name=name):
                policy = {
                    **self.policy,
                    "release_environment": {
                        **self.policy["release_environment"],
                        "name": name,
                    },
                }
                with self.assertRaisesRegex(ValueError, "non-empty string"):
                    audit.release_environment_policy(policy)

    def test_rejects_non_positive_reviewer_counts(self) -> None:
        for reviewers in (None, 0, -1, 1.5, True, "1"):
            with self.subTest(reviewers=reviewers):
                policy = {
                    **self.policy,
                    "release_environment": {
                        **self.policy["release_environment"],
                        "required_reviewers": reviewers,
                    },
                }
                with self.assertRaisesRegex(ValueError, "positive integer"):
                    audit.release_environment_policy(policy)

    def test_rejects_non_boolean_branch_protection(self) -> None:
        for protected in (None, 1, "true"):
            with self.subTest(protected=protected):
                policy = {
                    **self.policy,
                    "release_environment": {
                        **self.policy["release_environment"],
                        "protected_branches": protected,
                    },
                }
                with self.assertRaisesRegex(ValueError, "must be a boolean"):
                    audit.release_environment_policy(policy)


class ReleaseEnvironmentAuditTests(unittest.TestCase):
    expected = {
        "name": "crates-io",
        "required_reviewers": 1,
        "protected_branches": True,
    }

    @staticmethod
    def environment() -> dict[str, object]:
        # Mirrors GET /repos/{owner}/{repo}/environments/crates-io.
        return {
            "name": "crates-io",
            "protection_rules": [
                {
                    "id": 65476915,
                    "type": "required_reviewers",
                    "prevent_self_review": False,
                    "reviewers": [
                        {
                            "type": "User",
                            "reviewer": {"login": "wallstop", "id": 1045249},
                        }
                    ],
                },
                {"id": 65476916, "type": "branch_policy"},
            ],
            "deployment_branch_policy": {
                "protected_branches": True,
                "custom_branch_policies": False,
            },
        }

    def test_accepts_the_protected_environment(self) -> None:
        self.assertEqual(audit.audit_environment(self.expected, self.environment()), [])

    def test_rejects_missing_or_malformed_protection_rules(self) -> None:
        for rules in (None, {}, "[]"):
            with self.subTest(rules=rules):
                environment = {**self.environment(), "protection_rules": rules}
                self.assertEqual(
                    audit.audit_environment(self.expected, environment),
                    ["protection_rules response was not a list"],
                )

    def test_rejects_an_unprotected_environment(self) -> None:
        # The exact live state issue #265 was filed against.
        environment = self.environment()
        environment["protection_rules"] = []
        environment["deployment_branch_policy"] = None
        self.assertEqual(
            audit.audit_environment(self.expected, environment),
            [
                "environment has 0 required reviewers; policy requires 1",
                "deployment branch policy is not configured",
            ],
        )

    def test_rejects_fewer_reviewers_than_policy_requires(self) -> None:
        environment = self.environment()
        environment["protection_rules"][0]["reviewers"] = []
        self.assertEqual(
            audit.audit_environment(self.expected, environment),
            ["environment has 0 required reviewers; policy requires 1"],
        )

    def test_rejects_a_malformed_reviewers_field_instead_of_len_passing(self) -> None:
        # len() of a dict or string would satisfy the reviewer count with
        # zero real reviewers; only a list counts.
        for reviewers in ({"type": "User"}, "wallstop", 1, True):
            with self.subTest(reviewers=reviewers):
                environment = self.environment()
                environment["protection_rules"][0]["reviewers"] = reviewers
                self.assertEqual(
                    audit.audit_environment(self.expected, environment),
                    ["environment has 0 required reviewers; policy requires 1"],
                )

    def test_rejects_a_payload_for_a_different_environment(self) -> None:
        environment = self.environment()
        environment["name"] = "staging"
        self.assertEqual(
            audit.audit_environment(self.expected, environment),
            ["audited environment 'staging' is not 'crates-io'"],
        )

    def test_accepts_more_reviewers_than_policy_requires(self) -> None:
        environment = self.environment()
        environment["protection_rules"][0]["reviewers"].append(
            {"type": "User", "reviewer": {"login": "second", "id": 2}}
        )
        self.assertEqual(audit.audit_environment(self.expected, environment), [])

    def test_rejects_an_unset_branch_policy(self) -> None:
        environment = self.environment()
        environment["deployment_branch_policy"] = None
        self.assertEqual(
            audit.audit_environment(self.expected, environment),
            ["deployment branch policy is not configured"],
        )

    def test_rejects_custom_branch_policies_in_place_of_protection(self) -> None:
        # custom_branch_policies=True means deployments follow an arbitrary
        # custom list; only protected-branch restriction satisfies the policy.
        environment = self.environment()
        environment["deployment_branch_policy"] = {
            "protected_branches": False,
            "custom_branch_policies": True,
        }
        self.assertEqual(
            audit.audit_environment(self.expected, environment),
            [
                "deployment branch policy must restrict deployments to "
                "protected branches"
            ],
        )


if __name__ == "__main__":
    unittest.main()
