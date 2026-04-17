#!/usr/bin/env python3
"""Helpers for the fork release workflow."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from urllib.request import Request, urlopen


REPO_ROOT = Path(__file__).resolve().parent.parent
UPSTREAM_REPO = "openai/codex"
PROTECTED_PATHS = [
    ".gitattributes",
    ".github/workflows",
    ".github/scripts/install-musl-build-tools.sh",
    ".github/scripts/rusty_v8_bazel.py",
    ".github/scripts/rusty_v8_module_bazel.py",
    "AGENTS.md",
    "AGENTS_FORK.md",
    "README.md",
    "README.ja.md",
    "codex-rs/core/src/agent/agent_names.txt",
]


def run_command(cmd: list[str], capture: bool = False) -> str:
    result = subprocess.run(
        cmd,
        cwd=REPO_ROOT,
        check=True,
        text=True,
        capture_output=capture,
    )
    if capture:
        return result.stdout.strip()
    return ""


def latest_upstream_release(token: str | None) -> dict[str, str]:
    request = Request(
        f"https://api.github.com/repos/{UPSTREAM_REPO}/releases/latest",
        headers={
            "Accept": "application/vnd.github+json",
            **({"Authorization": f"Bearer {token}"} if token else {}),
        },
    )
    with urlopen(request) as response:
        payload = json.load(response)

    tag_name = payload["tag_name"]
    return {
        "tag_name": tag_name,
        "html_url": payload["html_url"],
        "version": version_from_upstream_tag(tag_name),
    }


def version_from_upstream_tag(tag_name: str) -> str:
    prefix = "rust-v"
    if not tag_name.startswith(prefix):
        raise ValueError(f"Unsupported upstream tag format: {tag_name}")
    return tag_name.removeprefix(prefix)


def release_tag_for_version(version: str) -> str:
    return f"codex-slop-fork-v{version}"


def failure_tag_for_upstream_tag(upstream_tag: str) -> str:
    safe_upstream_tag = upstream_tag.replace("/", "-")
    return f"codex-slop-fork-failed-merge-{safe_upstream_tag}"


def staging_ref_for_release_tag(release_tag: str) -> str:
    return f"release-staging/{release_tag}"


def restore_protected_paths(base_ref: str) -> None:
    for path in PROTECTED_PATHS:
        run_command(["git", "checkout", base_ref, "--", path])


def sync_upstream_readme(upstream_ref: str) -> None:
    upstream_readme = run_command(["git", "show", f"{upstream_ref}:README.md"], capture=True)
    readme_path = REPO_ROOT / "README_UPSTREAM.md"
    readme_path.write_text(f"{upstream_readme}\n", encoding="utf-8")
    run_command(["git", "add", "README_UPSTREAM.md"])


def existing_merge_commit_sha(upstream_ref: str) -> str:
    merge_subject = f"Merge upstream release {upstream_ref}"
    return run_command(
        [
            "git",
            "log",
            "--first-parent",
            "--format=%H",
            "--max-count",
            "1",
            "--grep",
            f"^{merge_subject}$",
            "HEAD",
        ],
        capture=True,
    )


def merge_upstream(args: argparse.Namespace) -> int:
    run_command(["git", "config", "merge.ours.driver", "true"])
    merge_cmd = ["git", "merge", "--no-ff", "--no-commit", args.upstream_ref]
    merge_result = subprocess.run(
        merge_cmd,
        cwd=REPO_ROOT,
        check=False,
        text=True,
        capture_output=True,
    )
    if merge_result.returncode != 0:
        if merge_result.stdout:
            print(merge_result.stdout, file=sys.stderr, end="")
        if merge_result.stderr:
            print(merge_result.stderr, file=sys.stderr, end="")
        subprocess.run(["git", "merge", "--abort"], cwd=REPO_ROOT, check=False)
        return 2

    merge_head_result = subprocess.run(
        ["git", "rev-parse", "-q", "--verify", "MERGE_HEAD"],
        cwd=REPO_ROOT,
        check=False,
        text=True,
        capture_output=True,
    )
    if merge_head_result.returncode != 0:
        merge_sha = existing_merge_commit_sha(args.upstream_ref)
        if not merge_sha:
            ancestor_result = subprocess.run(
                ["git", "merge-base", "--is-ancestor", args.upstream_ref, "HEAD"],
                cwd=REPO_ROOT,
                check=False,
                text=True,
            )
            if ancestor_result.returncode != 0:
                raise RuntimeError(
                    "Upstream release is already present on the current branch, "
                    "but no matching merge commit was found."
                )
            merge_sha = run_command(["git", "rev-parse", "HEAD"], capture=True)
        print(json.dumps({"merge_sha": merge_sha}))
        return 0

    restore_protected_paths(args.base_ref)
    sync_upstream_readme(args.upstream_ref)

    commit_message = (
        f"Merge upstream release {args.upstream_ref}\n\n"
        f"Upstream release: {args.upstream_url}\n"
    )
    run_command(["git", "commit", "--quiet", "-m", commit_message])

    merge_sha = run_command(["git", "rev-parse", "HEAD"], capture=True)
    print(json.dumps({"merge_sha": merge_sha}))
    return 0


def upstream_release_metadata(args: argparse.Namespace) -> int:
    if args.upstream_tag:
        upstream_tag = args.upstream_tag
        upstream_url = f"https://github.com/{UPSTREAM_REPO}/releases/tag/{upstream_tag}"
        version = version_from_upstream_tag(upstream_tag)
    else:
        release = latest_upstream_release(os.environ.get("GITHUB_TOKEN"))
        upstream_tag = release["tag_name"]
        upstream_url = release["html_url"]
        version = release["version"]

    payload = {
        "upstream_tag": upstream_tag,
        "upstream_url": upstream_url,
        "version": version,
        "release_tag": release_tag_for_version(version),
        "failure_tag": failure_tag_for_upstream_tag(upstream_tag),
    }
    payload["staging_ref"] = staging_ref_for_release_tag(payload["release_tag"])
    print(json.dumps(payload))
    return 0


def package_npm(args: argparse.Namespace) -> int:
    binary_path = Path(args.binary_path).resolve()
    output_path = Path(args.output_path).resolve()
    output_path.parent.mkdir(parents=True, exist_ok=True)

    binary_name = args.binary_name
    if binary_name is None:
        binary_name = "codex-slop-fork.exe" if args.os == "win32" else "codex-slop-fork"

    with tempfile.TemporaryDirectory(prefix="codex-slop-fork-npm-") as tmp_dir:
        package_root = Path(tmp_dir)
        vendor_dir = package_root / "vendor"
        vendor_dir.mkdir(parents=True, exist_ok=True)

        staged_binary = vendor_dir / binary_name
        shutil.copy2(binary_path, staged_binary)
        if args.os != "win32":
            staged_binary.chmod(0o755)

        package_json = {
            "name": "codex-slop-fork",
            "version": args.version,
            "license": "Apache-2.0",
            "description": f"codex-slop-fork binary package for {args.os}/{args.cpu}",
            "bin": {
                "codex-slop-fork": f"vendor/{binary_name}",
            },
            "files": ["vendor"],
            "os": [args.os],
            "cpu": [args.cpu],
        }
        (package_root / "package.json").write_text(
            json.dumps(package_json, indent=2) + "\n",
            encoding="utf-8",
        )

        license_path = REPO_ROOT / "LICENSE"
        if license_path.exists():
            shutil.copy2(license_path, package_root / "LICENSE")

        readme_path = REPO_ROOT / "README.md"
        if readme_path.exists():
            shutil.copy2(readme_path, package_root / "README.md")

        npm_executable = "npm.cmd" if os.name == "nt" else "npm"
        pack_dir = package_root / "pack"
        pack_dir.mkdir(parents=True, exist_ok=True)
        pack_output_raw = subprocess.check_output(
            [npm_executable, "pack", "--json", "--pack-destination", str(pack_dir)],
            cwd=package_root,
            text=True,
        )
        pack_output = json.loads(pack_output_raw)
        tarball_name = pack_output[0]["filename"]
        shutil.move(str(pack_dir / tarball_name), output_path)

    print(json.dumps({"output_path": str(output_path)}))
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    metadata_parser = subparsers.add_parser(
        "upstream-release-metadata",
        help="Print upstream release metadata as JSON.",
    )
    metadata_parser.add_argument(
        "--upstream-tag",
        help="Explicit upstream release tag. Defaults to the latest stable upstream release.",
    )

    merge_parser = subparsers.add_parser(
        "merge-upstream",
        help="Merge an upstream release into the current branch.",
    )
    merge_parser.add_argument("--upstream-ref", required=True, help="Git ref to merge.")
    merge_parser.add_argument("--base-ref", required=True, help="Ref used to restore protected paths.")
    merge_parser.add_argument("--upstream-url", required=True, help="Upstream release URL.")

    package_parser = subparsers.add_parser(
        "package-npm",
        help="Package a built binary as a per-platform npm tarball.",
    )
    package_parser.add_argument("--version", required=True, help="Package version.")
    package_parser.add_argument("--os", required=True, help="npm os value.")
    package_parser.add_argument("--cpu", required=True, help="npm cpu value.")
    package_parser.add_argument("--binary-path", required=True, help="Path to the compiled binary.")
    package_parser.add_argument("--output-path", required=True, help="Path for the final tarball.")
    package_parser.add_argument("--binary-name", help="Filename to use inside the package.")

    return parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()

    if args.command == "upstream-release-metadata":
        return upstream_release_metadata(args)
    if args.command == "merge-upstream":
        return merge_upstream(args)
    if args.command == "package-npm":
        return package_npm(args)

    parser.error(f"Unknown command: {args.command}")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
