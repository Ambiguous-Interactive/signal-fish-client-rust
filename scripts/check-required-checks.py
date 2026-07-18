#!/usr/bin/env python3
"""Fail unless every configured aggregate check completed successfully."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any


def load_json(path: Path) -> Any:
    with path.open(encoding="utf-8") as stream:
        return json.load(stream)


def required_jobs(policy: dict[str, Any]) -> list[str]:
    checks = policy.get("required_checks")
    if not isinstance(checks, list) or not checks:
        raise ValueError("policy must define a non-empty required_checks list")
    jobs: list[str] = []
    for check in checks:
        if not isinstance(check, dict) or not isinstance(check.get("job"), str):
            raise ValueError("every required check must define a job name")
        jobs.append(check["job"])
    if len(jobs) != len(set(jobs)):
        raise ValueError("required check job names must be unique")
    return jobs


def check_results(policy: dict[str, Any], payload: dict[str, Any]) -> list[str]:
    runs = payload.get("check_runs")
    if not isinstance(runs, list):
        raise ValueError("check-runs payload must define check_runs")
    latest: dict[str, dict[str, Any]] = {}
    for run in runs:
        if not isinstance(run, dict) or not isinstance(run.get("name"), str):
            continue
        current = latest.get(run["name"])
        if current is None or int(run.get("id", 0)) > int(current.get("id", 0)):
            latest[run["name"]] = run

    failures = []
    for job in required_jobs(policy):
        run = latest.get(job)
        if run is None:
            failures.append(f"{job}: missing")
        elif run.get("status") != "completed" or run.get("conclusion") != "success":
            failures.append(
                f"{job}: status={run.get('status', 'missing')}, "
                f"conclusion={run.get('conclusion', 'missing')}"
            )
    return failures


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check-runs", type=Path, required=True)
    parser.add_argument("--policy", type=Path, required=True)
    args = parser.parse_args()
    try:
        failures = check_results(load_json(args.policy), load_json(args.check_runs))
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"required-check error: {error}", file=sys.stderr)
        return 1
    if failures:
        print("Required checks are not all successful:", file=sys.stderr)
        for failure in failures:
            print(f"- {failure}", file=sys.stderr)
        return 1
    print("All configured required checks completed successfully.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
