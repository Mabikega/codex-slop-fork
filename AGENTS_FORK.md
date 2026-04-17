# AGENTS_FORK.md

This file is a fork-specific supplement to `AGENTS.md`.

The goal of this repository is to keep this project as a lightly maintained fork of `openai/codex` that can keep merging upstream changes with minimal or no human conflict resolution.

## Fork Docs

- `AGENTS_FORK.md`, `README.md`, and `README.ja.md` must always be kept up to date.
- If the fork gains, removes, renames, or significantly reshapes a feature, seam, storage layout, command, or workflow, update every relevant fork doc in the same change when applicable.
- Keep user-facing fork documentation local to `README.md`, with `README.ja.md` maintained as the Japanese counterpart.
- Keep `README.md` and `README.ja.md` aligned in scope and behavior so the Japanese doc does not drift behind the English one.
- Do not leave fork documentation as a stale description of an older design.

## Current Fork Structure

This section describes the actual structure of this fork, not generic upstream Codex structure.

- GitHub Actions policy: keep only fork-owned workflows whose filenames start with `fork-`.
- The current fork-owned GitHub Actions workflows are:
  - `.github/workflows/fork-release.yml`
  - `.github/workflows/fork-tag-release.yml`
- The only `.github/` support files that should remain are files directly required by those fork workflows.
- Current required workflow support files are:
  - `.github/scripts/install-musl-build-tools.sh`
  - `.github/scripts/rusty_v8_bazel.py`
  - `.github/scripts/rusty_v8_module_bazel.py`
- Remove upstream or non-fork GitHub Actions workflows and workflow-only support files when they are no longer needed by the fork.
- Do not add new non-fork workflow files unless the user explicitly asks for an exception.
- During upstream merges, do not bring back upstream `.github/` files unless they are required by the current fork workflows.

- Core fork logic lives under `codex-rs/core/src/slop_fork/`.
- Current core fork modules are:
  - `account_rate_limits.rs`
  - `account_switching.rs`
  - `auth_accounts.rs`
  - `auth_sync.rs`
  - `automation.rs`
  - `autoresearch/`
  - `config.rs`
  - `pilot.rs`
  - `saved_account_limit_averages.rs`
  - `mod.rs`
- TUI fork logic lives under `codex-rs/tui/src/slop_fork/`.
- Current TUI fork modules are:
  - `autoresearch_command.rs`
  - `auto_command.rs`
  - `external_auth.rs`
  - `app_server.rs`
  - `event.rs`
  - `login_settings_view.rs`
  - `pilot_command.rs`
  - `rate_limit_poller.rs`
  - `runtime_event.rs`
  - `schedule_parser.rs`
  - `status_line.rs`
  - `ui_automation.rs`
  - `ui_autoresearch.rs`
  - `ui_login.rs`
  - `ui_pilot.rs`
  - `ui_rate_limits.rs`
  - `ui.rs`
  - `mod.rs`
- `ui.rs` is the TUI fork controller seam. Login popup rendering, saved-account rate-limit logic,
  automation UI logic, Autoresearch UI logic, and Pilot UI logic live in `ui_login.rs`,
  `ui_rate_limits.rs`, `ui_automation.rs`, `ui_autoresearch.rs`, and `ui_pilot.rs` as internal
  submodules so upstream-facing hooks stay thin.
- App-server fork logic currently lives in:
  - `codex-rs/app-server/src/slop_fork_account_rate_limits.rs`
  - `codex-rs/app-server/src/slop_fork_automation.rs`
  - `codex-rs/app-server/src/slop_fork_pilot.rs`
- Upstream-facing hotspots should stay thin and delegate into these fork-owned modules instead of accumulating fork policy locally.
- Fork-specific persisted state currently lives in:
  - `~/.codex/.accounts/`
  - `~/.codex/.accounts/.rate-limits.json`
  - `~/.codex/config-slop-fork.toml`
  - `~/.codex/codex-slop-fork-automations.toml`
  - `<repo>/.codex/codex-slop-fork-automations.toml`
  - `~/.codex/.codex-slop-fork-automation-state.json`
  - `~/.codex/.codex-slop-fork-autoresearch-state.json`
  - `~/.codex/.autoresearch-snapshots/`
  - `~/.codex/.codex-slop-fork-pilot-state.json`
- Autoresearch sessions are project-local and currently center on `autoresearch.md`,
  `autoresearch.sh`, optional checks/ideas files, and `autoresearch.jsonl`; `autoresearch.md`
  can carry the primary metric, hard constraints, ordered staged targets, additional metrics,
  optional composite-score policy, exploration policy, discovery policy, candidate contract,
  selection policy, and hidden constraints/unknowns for the native loop. `autoresearch.jsonl`
  can now also carry bounded discovery findings, first-class approach entries, and benchmark runs
  annotated with `approach_id`. Open-ended research mode keeps non-git snapshots per approach
  under `~/.codex/.autoresearch-snapshots/` so multiple candidate lineages can survive between
  cycles.

## Primary Rule

Treat every fork feature as an overlay, not as a rewrite of upstream Codex.

When choosing between:

- a simpler local change that edits an upstream hotspot directly
- a slightly more indirect change that keeps fork behavior namespaced and isolated

prefer the isolated overlay.

## Architectural Rules

- Keep fork-only core logic under `codex-rs/core/src/slop_fork/`.
- Keep fork-only TUI logic under `codex-rs/tui/src/slop_fork/`.
- Upstream-facing files should contain only thin delegation hooks into the fork overlay.
- Do not spread fork policy across unrelated upstream modules.
- Do not add fork-only behavior to generic upstream helpers when an explicit fork wrapper can be used instead.
- Do not widen shared public surface area unless it is required as a stable seam.

## Design For Change

We want code with strong changeability: after the structure is in place, the next similar feature should feel like it mostly writes itself by plugging into the existing seam.

Prefer designs with:

- high cohesion: related fork policy lives together
- low coupling: adding fork behavior should not require touching many unrelated files
- clear seams: explicit hooks, adapters, controllers, and wrappers that make extension obvious
- local change: a new fork feature should usually be add-more, not rewrite-more
- composition over invasive conditionals in upstream hotspots

If the next similar feature would require editing multiple hotspots, threading new flags through generic upstream code, or copying logic into another place, treat that as a design smell and refactor toward a better seam first.

## Hotspot Discipline

The following files are high-churn upstream hotspots and should stay as close to upstream as possible:

- `codex-rs/core/src/auth.rs`
- `codex-rs/core/src/codex.rs`
- `codex-rs/tui/src/chatwidget.rs`
- `codex-rs/tui/src/app.rs`
- `codex-rs/tui/src/app_event.rs`
- `codex-rs/tui/src/slash_command.rs`

Allowed changes in those files:

- add a thin hook
- delegate into `slop_fork`
- pass through a fork event or effect
- wire a fork slash command into existing registries

Avoid:

- embedding fork business logic directly in those files
- building fork-specific state machines there
- changing upstream behavior for non-fork features

## Mergeability Discipline

- Small fork changes can still conflict if they claim line ownership inside helper clusters, `impl` blocks, or other upstream churn zones.
- Do not add fork-only helper functions, convenience methods, or local utilities directly into shared upstream files unless there is no cleaner seam.
- When fork behavior needs new supporting logic, prefer implementing that logic under `slop_fork/` and reaching it through an existing hook.
- If an upstream file must change, prefer modifying one existing seam over adding new fork-owned insertion points in the middle of the file.
- Treat line ownership as part of mergeability: "thin hook only" means minimizing both fork logic and fork-controlled placement inside upstream code.

## Upstream Sync Policy

- Sync this fork only to official upstream release tags.
- Treat final semver releases as the allowed upstream base, for example `rust-v0.114.0`.
- Do not sync this fork to upstream `main`, arbitrary upstream commits, prerelease tags, release candidates, alpha builds, beta builds, nightly builds, or any other non-release upstream state unless the user explicitly instructs otherwise for a one-off exception.
- If a tag or upstream ref is ambiguous, treat it as disallowed until it is confirmed to be a final release.
- Prefer staying on the currently selected release line until the next upstream final release is intentionally chosen.

## Upstream Merge Workflow

When bringing new upstream changes into the fork:

- Fetch upstream tags and choose the exact final release tag first. Do not start from `main` and then try to "back into" a release later.
- Before starting the merge, ensure the worktree is clean by committing any in-progress changes so the merge starts from an explicit, recoverable fork state.
- Create a pre-merge tag before rewriting branch state, replaying commits, or converting commits into uncommitted changes so it is always possible to return to the exact pre-merge state.
- Prefer a history-preserving integration branch that starts from the current fork `main` (or its safety snapshot) and merges the chosen upstream release tag into it.
- Keep the fork's existing commit history and the upstream commit history, and layer merge-resolution or adaptation commits on top of that combined history rather than flattening or replacing it.
- Use a rebuild or reconstruction on top of the chosen release tag only as a fallback when the history-preserving merge path is disproportionately noisy, misleading, or risky.
- If a rebuild fallback is used, preserve the old fork tip on an explicit backup branch so the prior fork history remains available for blame, bisect, and audit work.
- Replay only the intended fork delta. Do not preserve unrelated branch baggage, stale merge resolutions, or carried-forward upstream-only files just because they existed in an older local branch.
- If a file differs from the chosen upstream release for reasons unrelated to the fork feature, restore the release-tag version first and then reapply only the minimal fork seam that is actually required.
- When a merge conflict touches a file that is not fork-owned, start from the exact upstream release version and make the conflict resolution prove why every remaining non-upstream line must exist. The default answer for shared files is "take upstream", not "keep ours".
- If upstream deleted code, that deletion wins by default. Only keep the deleted code when it is required for an explicit fork feature, is isolated behind a fork-owned seam, and that reason is documented in the merge commit or follow-up patch.
- Treat target-specific code paths as first-class merge surfaces. A stale macOS- or Windows-only block is still stale even if Linux CI passes because it compiles a stub or a different module.
- Prefer proving provenance file-by-file in high-churn hotspots: first check whether the file should be byte-for-byte upstream, and only keep a diff when there is a clear fork-owned reason.
- After each upstream sync merge or replay, run `python3 scripts/audit_upstream_sync_merges.py --history` and inspect every current candidate before considering the merge complete.
- During every upstream merge, explicitly review whether new upstream capabilities create a better seam or implementation path for an existing fork feature, including areas such as command handling, looping, hooking, account management, and similar integration points. Do not limit the merge to conflict resolution if upstream now offers a cleaner way to express the same fork behavior.
- It is good to adopt those better upstream seams and adjust the fork implementation accordingly, but only if the fork features still work as intended after the change.
- During every upstream merge, explicitly review whether any fork feature has become partially or fully redundant because upstream gained a very similar capability. If so, prefer deleting or shrinking the fork overlay instead of preserving older fork code out of habit.
- Before each new upstream merge, audit every known fork-only workaround commit against upstream history. If upstream already addressed that exact problem, drop the fork workaround commit and keep the upstream solution instead of carrying a redundant local patch forward.
- After the merge or replay, verify that all fork features still work as intended, that fork logic still lives under `slop_fork/`, that upstream hotspots only contain thin delegation hooks, and that generated docs or schemas are regenerated only when the fork feature truly changes them.
- Do not treat "it builds" or "tests passed" as sufficient proof after an upstream merge. Double-check and triple-check that the actual fork features still work end to end in the merged tree, especially around thin hooks, event wiring, turn lifecycle handling, account flows, automation, Autoresearch, Pilot, and any other fork-owned behavior that can silently lose call sites during an upstream sync.
- When compile failures appear after a release rebase, first suspect mixed-version state before assuming the fork feature itself is wrong.

## Conflict Avoidance Rules

- Treat upstream-owned capability areas such as shared tool plumbing, `code_mode`, plugin marketplace models, collaboration payloads, and generic TUI navigation as sealed unless an explicit fork seam already exists.
- Do not reshape shared upstream structs, enums, tool specs, or protocol payloads in place for fork behavior. Prefer fork-owned adapters, wrappers, or post-processing layers.
- Avoid fork-owned edits in `codex-rs/core/src/tools/spec.rs`, `codex-rs/core/src/tools/router.rs`, `codex-rs/core/src/tools/context.rs`, `codex-rs/core/src/tools/code_mode.rs`, `codex-rs/core/src/tools/code_mode_description.rs`, `codex-rs/core/src/tools/code_mode_runner.cjs`, `codex-rs/core/src/plugins/marketplace.rs`, `codex-rs/tui/src/app.rs`, and `codex-rs/tui/src/multi_agents.rs` unless there is no cleaner seam.
- When a fork feature needs behavior in one of those files, add or reuse one thin hook and keep the real policy under `slop_fork/`. Do not add extra helper logic beside the hook.
- Prefer additive compatibility shims over rewrites of upstream behavior. If upstream introduces a new API or export surface, keep fork compatibility by layering on top rather than replacing the shared implementation path.
- Do not edit upstream-owned docs or generated descriptive text for fork behavior when a fork doc or fork-owned appendix will do. Keep user-facing fork documentation local to `README.md` and `README.ja.md`, and keep `AGENTS_FORK.md` focused on fork-maintainer rules.
- In tests, do not assert exact upstream-generated wording unless the wording is fork-owned. Prefer semantic assertions over full-string ownership of upstream descriptions and tool help text.
- Before editing a hotspot, ask whether the same outcome can be achieved by delegation, wrapping, alias registration, fork-owned state, or post-processing. If yes, use that approach instead of widening the shared change.
- If a fork change must touch a hotspot, keep the patch local, document why the seam was insufficient, and prefer taking upstream behavior when there is no explicit fork requirement to differ.

## Behavior Rules

- Preserve upstream Codex behavior by default.
- If a change is fork-only, it must be clearly namespaced as fork behavior.
- Do not change existing upstream commands unless the user explicitly requests it.
- Do not touch normal upstream commands like `/fast` unless there is an explicit fork requirement to do so.
- Fork UX should reuse existing UI primitives when possible.
- Prefer command-based or lightweight popup-based flows over large new TUI views.

## Storage Rules

Fork-specific persisted state must remain outside normal upstream config/state whenever practical.

Current fork storage conventions:

- active auth stays upstream-compatible at `~/.codex/auth.json`
- saved extra accounts live at `~/.codex/.accounts/*.json`
- each saved account file should be `auth.json`-compatible
- fork-only account rate-limit snapshots live at `~/.codex/.accounts/.rate-limits.json`
- fork-only settings live at `~/.codex/config-slop-fork.toml`

Do not invent new storage layouts unless there is a clear need.

## Auth Rules

- `auth.json` compatibility is more important than internal elegance.
- Manual user recovery must stay easy: a user should be able to promote a saved account file back into `~/.codex/auth.json`.
- Generic upstream auth operations should keep their upstream semantics.
- Fork account preservation, mirroring, activation, autoswitching, and related policy should live in explicit fork wrappers.

## Event And UI Rules

- Fork TUI events should stay namespaced under a single fork event surface.
- Prefer one `AppEvent::SlopFork(...)` style entrypoint over many top-level fork event variants.
- Fork UI orchestration should live in a fork controller, not inside `ChatWidget` itself.
- If a fork submenu completes successfully, prefer returning to the normal composer state unless the UX explicitly needs to stay open.

## Implementation Quality

Write fork code to a high engineering standard, but do it in ways that preserve the fork's mergeability and locality.

- Optimize first for correctness, readability, and local change. Performance matters, but do not trade maintainability for speculative micro-optimizations.
- Prefer self-explanatory names, small cohesive modules, straightforward control flow, and minimal nesting. Reduce duplication and hidden coupling.
- Hardcode only values that are genuinely stable. Put fork-tunable values behind config, constants, or typed parameters at the ownership boundary.
- Use the idioms of the language you are working in. In Rust especially, prefer strong typing, exhaustive matches, clear ownership, and error handling that preserves actionable context.
- Add comments to capture intent, invariants, and non-obvious tradeoffs. Do not use comments to restate obvious code.
- Include validation, logging, and error handling where failures would otherwise be silent, ambiguous, or hard to recover from.
- Refactor when it materially improves structure, but prefer additive extraction and better seams over broad rewrites of upstream hotspots.
- Keep code modular and extensible so the next related fork feature can usually slot into the same seam with another local change.
- Follow security best practices by default: validate inputs, avoid unnecessary privilege or state mutation, and do not introduce fork-only shortcuts that weaken upstream safety expectations.
- Use tests to lock in behavior when practical, especially around fork policy, persistence, account handling, event flow, and other merge-sensitive logic.

## Review Standard

Before finalizing a fork change, check:

- Is the fork logic isolated under `slop_fork/`?
- Did we keep upstream hotspots to thin hooks only?
- Did we avoid changing generic upstream semantics?
- Is persisted fork state kept in fork-specific files and directories?
- Would the next similar fork feature likely fit the same seam with another small local change?
- Would this diff be likely to merge cleanly on top of a newer upstream Codex version?
- For upstream merges specifically: did we double-check and triple-check that every important fork feature still works in the live code paths, not only in tests, helper code, or replay-only paths?

If the answer to any of those is no, refactor before finalizing.

## Review Workflow

After each larger fork change, run an explicit post-implementation review pass by creating a sub-agent with `fork_context=false`, `model=gpt-5.4`, and `reasoning_effort=high`.

Sub-agents can take a long time to run. Always wait for them to finish and return their results before deciding the review is complete.

The required workflow is:

- implement the change
- run the normal local verification for the affected crate or area
- create the sub-agent with `fork_context=false`, `model=gpt-5.4`, and `reasoning_effort=high`, focused only on the changed code
- ask it to hunt for bugs, regressions, mergeability risks, code quality problems, and whether the fork changes stay out of the way of original/upstream code so future merges remain low-conflict
- let it report findings first
- fix the reported issues
- rerun the relevant verification

This review pass is required for larger code changes, not only when something already looks suspicious.

The review should stay fork-aware:

- prioritize correctness bugs, behavioral regressions, and upstream-merge risks
- call out when fork logic leaked into upstream hotspots more than necessary
- check whether the change stays out of upstream code paths and avoids claiming unnecessary line ownership in upstream files
- do not treat backward compatibility with older fork-specific behavior or older fork-only seams as a goal; prefer the cleaner current fork design when there is a conflict
- prefer concrete file-level findings over broad commentary

Do not skip the fix step after the report. The expected flow is review, findings, fixes, and verification.

## Bias

When in doubt:

- choose mergeability over cleverness
- choose explicit fork wrappers over implicit global behavior changes
- choose smaller seams over wider rewrites
- choose compatibility and recoverability over abstraction
