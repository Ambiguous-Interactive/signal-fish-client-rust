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
        payload = {
            "check_runs": [
                {"id": 1, "name": "CI Required", "status": "completed", "conclusion": "failure"},
                {"id": 3, "name": "CI Required", "status": "completed", "conclusion": "success"},
                {"id": 2, "name": "Docs Required", "status": "completed", "conclusion": "success"},
            ]
        }
        self.assertEqual(checks.check_results(self.policy, payload), [])

    def test_reports_missing_pending_and_failed_checks(self) -> None:
        payload = {
            "check_runs": [
                {"id": 1, "name": "CI Required", "status": "in_progress", "conclusion": None}
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


if __name__ == "__main__":
    unittest.main()
