"""Exercise the shell workflow write-policy boundary with hostile variants."""

from pathlib import Path
import subprocess
import tempfile
import unittest


class ReviewWritePolicyTests(unittest.TestCase):
    def test_only_trusted_review_job_permission_is_allowed(self) -> None:
        root = Path(__file__).resolve().parent.parent
        source = (root / "scripts/check-workflows.sh").read_text()
        policy = source.split("# Phase 9: forbid checked-in Dependabot auto-merge.")[1]
        policy = policy.split("# Result.")[0]
        caller = (root / ".github/workflows/openai-review.yml").read_text()
        cases = [
            ("trusted", "openai-review.yml", caller, True),
            ("permissions-first", "openai-review.yml", caller.replace("    permissions:\n      contents: read\n      pull-requests: write\n", "").replace("    uses:", "    permissions:\n      contents: read\n      pull-requests: write\n    uses:"), True),
            ("commented-job", "openai-review.yml", caller.replace("  review:", "  review: # advisory"), True),
            ("sibling-job", "openai-review.yml", caller + "\n  other:\n    runs-on: ubuntu-latest\n    permissions:\n      pull-requests: write\n    steps:\n      - run: echo unsafe\n", False),
            ("workflow-scope", "openai-review.yml", caller.replace("permissions:\n  contents: read", "permissions:\n  contents: read\n  pull-requests: write"), False),
            ("trusted-sibling", "openai-review.yml", caller.replace("  review:", "  other:") + "\n  review:\n    permissions:\n      pull-requests: write\n    runs-on: ubuntu-latest\n", False),
            ("other-file", "other.yml", caller, False),
            ("untrusted-ref", "openai-review.yml", caller.replace("@v1.1.0", "@main"), False),
            ("contents-write", "openai-review.yml", caller.replace("contents: read", "contents: write"), False),
            ("issues-write", "openai-review.yml", caller.replace("pull-requests: write", "issues: write"), False),
            ("automatic-merge", "openai-review.yml", caller + "\n# example\nenv:\n  COMMAND: gh pr merge\n", False),
        ]
        for name, filename, content, allowed in cases:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as directory:
                tree = Path(directory)
                workflows = tree / ".github/workflows"
                workflows.mkdir(parents=True)
                (workflows / filename).write_text(content)
                header = (
                    "set -euo pipefail\n"
                    "YELLOW= GREEN= RED= NC=\n"
                    "VIOLATIONS=0\n"
                    "TMP_DEPENDABOT_AUTOMERGE_VIOLATIONS=violations\n"
                    ": > violations\n"
                )
                result = subprocess.run(
                    ["bash", "-c", header + policy + '\ntest "$VIOLATIONS" = 0\n'],
                    cwd=tree,
                    text=True,
                    capture_output=True,
                    check=False,
                )
                self.assertEqual(result.returncode == 0, allowed, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
