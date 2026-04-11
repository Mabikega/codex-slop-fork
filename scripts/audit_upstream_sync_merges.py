#!/usr/bin/env python3
"""Audit upstream sync merges for likely stale conflict resolutions.

This is a heuristic check. It looks for conflicted shared files in upstream sync
merges where the merge result stayed much closer to the fork parent than the
upstream parent, which is a common signature of accidentally preserving stale
fork-side content after upstream deleted or replaced code.
"""

from __future__ import annotations

import argparse
import dataclasses
import subprocess
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent
SYNC_SUBJECT_PREFIXES = (
    "merge: sync upstream rust-v",
    "merge: replay fork on rust-v",
)
FORK_OWNED_PREFIXES = (
    ".gitattributes",
    ".github/",
    "AGENTS.md",
    "AGENTS_FORK.md",
    "README.md",
    "README.ja.md",
    "README_UPSTREAM.md",
    "codex-rs/core/src/agent/agent_names.txt",
    "codex-rs/core/src/slop_fork/",
    "codex-rs/tui/src/slop_fork/",
    "codex-rs/login/src/slop_fork/",
    "codex-rs/app-server/src/slop_fork_",
)
FORK_OWNED_SUBSTRINGS = (
    "/slop_fork/",
    "/snapshots/",
    "tests/suite/slop_fork_",
)


@dataclasses.dataclass(frozen=True)
class PathAudit:
    merge: str
    subject: str
    path: str
    reason: str
    ours_added: int
    ours_deleted: int
    upstream_added: int
    upstream_deleted: int
    commits_after_merge: int
    worktree_modified: bool


def run_git(args: list[str], capture: bool = True) -> str:
    result = subprocess.run(
        ["git", *args],
        cwd=REPO_ROOT,
        check=True,
        text=True,
        capture_output=capture,
    )
    if capture:
        return result.stdout
    return ""


def is_fork_owned(path: str) -> bool:
    if any(path.startswith(prefix) for prefix in FORK_OWNED_PREFIXES):
        return True
    return any(token in path for token in FORK_OWNED_SUBSTRINGS)


def list_sync_merges() -> list[tuple[str, str]]:
    out = run_git(["log", "--merges", "--format=%H%x09%s"])
    merges: list[tuple[str, str]] = []
    for line in out.splitlines():
        commit, subject = line.split("\t", 1)
        if subject.startswith(SYNC_SUBJECT_PREFIXES):
            merges.append((commit, subject))
    return merges


def conflicted_paths(commit: str) -> list[str]:
    body = run_git(["show", "--summary", "--format=%B", commit])
    paths: list[str] = []
    for line in body.splitlines():
        stripped = line.strip()
        if not stripped.startswith("#"):
            continue
        path = stripped.removeprefix("#").strip()
        if "/" in path:
            paths.append(path)
    return paths


def diff_numstat(base: str, head: str, path: str) -> tuple[int, int]:
    out = run_git(["diff", "--numstat", base, head, "--", path])
    for line in out.splitlines():
        added, deleted, _path = line.split("\t", 2)
        if _path != path:
            continue
        try:
            return int(added), int(deleted)
        except ValueError:
            return 0, 0
    return 0, 0


def commits_after_merge(commit: str, path: str) -> int:
    out = run_git(["rev-list", "--count", f"{commit}..HEAD", "--", path])
    return int(out.strip() or "0")


def worktree_modified(path: str) -> bool:
    out = run_git(["status", "--short", "--", path])
    return bool(out.strip())


def audit_merge(
    commit: str,
    subject: str,
    min_upstream_delta: int,
    min_skew_ratio: float,
    min_residue_additions: int,
    max_upstream_deleted: int,
    min_our_deleted: int,
) -> list[PathAudit]:
    parent_ours = run_git(["rev-parse", f"{commit}^1"]).strip()
    parent_upstream = run_git(["rev-parse", f"{commit}^2"]).strip()

    findings: list[PathAudit] = []
    for path in conflicted_paths(commit):
        if is_fork_owned(path):
            continue
        ours_added, ours_deleted = diff_numstat(parent_ours, commit, path)
        upstream_added, upstream_deleted = diff_numstat(parent_upstream, commit, path)
        ours_delta = ours_added + ours_deleted
        upstream_delta = upstream_added + upstream_deleted
        reason: str | None = None
        if upstream_delta >= min_upstream_delta:
            skew_ratio = (upstream_delta + 1.0) / (ours_delta + 1.0)
            if skew_ratio >= min_skew_ratio:
                reason = "closer_to_ours_than_upstream"
        if (
            reason is None
            and upstream_added >= min_residue_additions
            and upstream_deleted <= max_upstream_deleted
            and ours_deleted >= min_our_deleted
            and ours_deleted > ours_added
        ):
            reason = "preserved_extra_code_on_top_of_upstream"
        if reason is None:
            continue
        findings.append(
            PathAudit(
                merge=commit,
                subject=subject,
                path=path,
                reason=reason,
                ours_added=ours_added,
                ours_deleted=ours_deleted,
                upstream_added=upstream_added,
                upstream_deleted=upstream_deleted,
                commits_after_merge=commits_after_merge(commit, path),
                worktree_modified=worktree_modified(path),
            )
        )
    return findings


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--min-upstream-delta",
        type=int,
        default=50,
        help="Minimum merge-vs-upstream delta before a conflicted file is interesting.",
    )
    parser.add_argument(
        "--min-skew-ratio",
        type=float,
        default=1.75,
        help="Minimum (merge-vs-upstream)/(merge-vs-ours) skew ratio to flag.",
    )
    parser.add_argument(
        "--min-residue-additions",
        type=int,
        default=40,
        help="Minimum merge-vs-upstream added lines to flag a likely preserved residue block.",
    )
    parser.add_argument(
        "--max-upstream-deleted",
        type=int,
        default=5,
        help="Maximum merge-vs-upstream deleted lines for a residue-style candidate.",
    )
    parser.add_argument(
        "--min-our-deleted",
        type=int,
        default=40,
        help="Minimum merge-vs-ours deleted lines for a residue-style candidate.",
    )
    parser.add_argument(
        "--history",
        action="store_true",
        help="Include historical candidates that were touched after the merge.",
    )
    return parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()

    all_findings: list[PathAudit] = []
    for commit, subject in list_sync_merges():
        all_findings.extend(
            audit_merge(
                commit=commit,
                subject=subject,
                min_upstream_delta=args.min_upstream_delta,
                min_skew_ratio=args.min_skew_ratio,
                min_residue_additions=args.min_residue_additions,
                max_upstream_deleted=args.max_upstream_deleted,
                min_our_deleted=args.min_our_deleted,
            )
        )

    current_findings = [
        finding
        for finding in all_findings
        if finding.commits_after_merge == 0
    ]
    historical_findings = [
        finding
        for finding in all_findings
        if finding.commits_after_merge > 0
    ]

    if not current_findings:
        print("No current outstanding stale-merge candidates found.")
    else:
        print("Current outstanding stale-merge candidates:")
        for finding in current_findings:
            modified = " (worktree modified)" if finding.worktree_modified else ""
            print(
                f"- {finding.path}{modified}: "
                f"merge={finding.merge[:9]} "
                f"reason={finding.reason} "
                f"ours=+{finding.ours_added}/-{finding.ours_deleted} "
                f"upstream=+{finding.upstream_added}/-{finding.upstream_deleted}"
            )

    if args.history and historical_findings:
        print()
        print("Historical candidates already touched after the merge:")
        for finding in historical_findings:
            print(
                f"- {finding.path}: "
                f"merge={finding.merge[:9]} "
                f"reason={finding.reason} "
                f"ours=+{finding.ours_added}/-{finding.ours_deleted} "
                f"upstream=+{finding.upstream_added}/-{finding.upstream_deleted} "
                f"commits_after={finding.commits_after_merge}"
            )

    return 1 if current_findings else 0


if __name__ == "__main__":
    raise SystemExit(main())
