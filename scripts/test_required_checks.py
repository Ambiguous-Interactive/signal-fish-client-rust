#!/usr/bin/env python3
"""Tests for check-required-checks.py."""

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "check_required_checks", Path(__file__).with_name("check-required-checks.py")
)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("could not load check-required-checks.py")
checks = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(checks)


class RequiredCheckTests(unittest.TestCase):
    policy = {
        "required_checks": [
            {"workflow": "CI", "job": "CI Required"},
            {"workflow": "Docs", "job": "Docs Required"},
        ]
    }

    def test_accepts_latest_successful_checks(self) -> None:
        payload = [
            {
                "check_runs": [
                    {
                        "id": 1,
                        "name": "CI Required",
                        "status": "completed",
                        "conclusion": "failure",
                    },
                    {
                        "id": 2,
                        "name": "Docs Required",
                        "status": "completed",
                        "conclusion": "success",
                    },
                ]
            },
            {
                "check_runs": [
                    {
                        "id": 3,
                        "name": "CI Required",
                        "status": "completed",
                        "conclusion": "success",
                    }
                ]
            },
        ]
        self.assertEqual(checks.check_results(self.policy, payload), [])

    def test_rejects_malformed_page_in_paginated_payload(self) -> None:
        with self.assertRaisesRegex(ValueError, "every page"):
            checks.check_results(self.policy, [{"check_runs": []}, {}])

    def test_reports_missing_pending_and_failed_checks(self) -> None:
        payload = {
            "check_runs": [
                {
                    "id": 1,
                    "name": "CI Required",
                    "status": "in_progress",
                    "conclusion": None,
                }
            ]
        }
        self.assertEqual(
            checks.check_results(self.policy, payload),
            [
                "CI Required: status=in_progress, conclusion=None",
                "Docs Required: missing",
            ],
        )

    def test_rejects_duplicate_job_names(self) -> None:
        policy = {
            "required_checks": [
                {"workflow": "One", "job": "Required"},
                {"workflow": "Two", "job": "Required"},
            ]
        }
        with self.assertRaisesRegex(ValueError, "unique"):
            checks.required_jobs(policy)

    def test_rejects_empty_or_whitespace_job_names(self) -> None:
        for job in ("", "   "):
            with self.subTest(job=job), self.assertRaisesRegex(ValueError, "job name"):
                checks.required_jobs(
                    {"required_checks": [{"workflow": "CI", "job": job}]}
                )

    def test_accepts_complete_pagination_total(self) -> None:
        payload = [
            {
                "total_count": 3,
                "check_runs": [
                    {
                        "id": 1,
                        "name": "CI Required",
                        "status": "completed",
                        "conclusion": "success",
                    },
                    {
                        "id": 2,
                        "name": "Docs Required",
                        "status": "completed",
                        "conclusion": "success",
                    },
                ],
            },
            {
                "total_count": 3,
                "check_runs": [
                    {
                        "id": 3,
                        "name": "CI Required",
                        "status": "completed",
                        "conclusion": "failure",
                    }
                ],
            },
        ]
        failures = checks.check_results(self.policy, payload)
        self.assertEqual(
            failures, ["CI Required: status=completed, conclusion=failure"]
        )

    def test_rejects_incomplete_pagination_before_judging_checks(self) -> None:
        # A truncated feed could hide a newer failing rerun behind an older
        # success; the missing run must fail the gate regardless of what the
        # fetched pages contain.
        payload = [
            {
                "total_count": 3,
                "check_runs": [
                    {
                        "id": 1,
                        "name": "CI Required",
                        "status": "completed",
                        "conclusion": "success",
                    }
                ],
            },
            {"total_count": 3, "check_runs": []},
        ]
        with self.assertRaisesRegex(ValueError, "pagination incomplete"):
            checks.check_results(self.policy, payload)

    def test_rejects_pages_that_disagree_on_total_count(self) -> None:
        payload = [
            {"total_count": 2, "check_runs": []},
            {"total_count": 3, "check_runs": []},
        ]
        with self.assertRaisesRegex(ValueError, "disagree on total_count"):
            checks.check_results(self.policy, payload)

    def test_rejects_non_integer_total_count(self) -> None:
        with self.assertRaisesRegex(ValueError, "total_count"):
            checks.check_results(
                self.policy, [{"total_count": "2", "check_runs": []}]
            )

    def test_rejects_non_integer_check_run_ids(self) -> None:
        for bad_id in (None, "7", 1.5, True):
            with self.subTest(bad_id=bad_id):
                payload = [
                    {
                        "check_runs": [
                            {
                                "id": bad_id,
                                "name": "CI Required",
                                "status": "completed",
                                "conclusion": "success",
                            }
                        ]
                    }
                ]
                with self.assertRaisesRegex(ValueError, "integer id"):
                    checks.check_results(self.policy, payload)


if __name__ == "__main__":
    unittest.main()
