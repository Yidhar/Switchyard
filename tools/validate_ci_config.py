"""Validate that GitHub Actions keeps the required Switchyard quality gates."""

from __future__ import annotations

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
WORKFLOW_PATH = ROOT / ".github" / "workflows" / "rust-windows.yml"

REQUIRED_SNIPPETS = (
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings",
    "cargo test --workspace --all-targets",
    "python tools/validate_ci_config.py",
    "ruff format --check .",
    "ruff check .",
    "tags:",
    '"v*"',
    "tauri-apps/tauri-action",
    "actions/upload-artifact@v4",
    "contents: write",
    "Collect release assets",
    "SHA256SUMS.txt",
    "Publish GitHub release",
    "gh release create",
    "gh release upload",
)


def missing_required_snippets(workflow_text: str) -> tuple[str, ...]:
    """Return CI contract snippets that are absent from the workflow text."""
    return tuple(snippet for snippet in REQUIRED_SNIPPETS if snippet not in workflow_text)


def main() -> int:
    """Run the workflow contract validation."""
    workflow_text = WORKFLOW_PATH.read_text(encoding="utf-8")
    missing = missing_required_snippets(workflow_text)

    if not missing:
        return 0

    missing_items = "\n".join(f"- {snippet}" for snippet in missing)
    sys.stderr.write(f"CI workflow is missing required checks:\n{missing_items}\n")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
