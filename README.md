# Codex Slop Fork

[English](README.md) | [日本語](README.ja.md)

A fork of Codex that was created by asking Codex itself to "pretty please add this feature, no bugs".

I have no interest in maintaining this fork long-term.

## Install

Install the latest release package for your platform with npm or Bun.
For npm, the `--force` flag is intentional so the stable `releases/latest/download/...` URL refreshes to the newest package version on upgrade.
Rerun the same command to update to the latest release.
The installed command is `codex-slop-fork`.

Linux x64:

```sh
npm install -g --force https://github.com/Mabikega/codex-slop-fork/releases/latest/download/codex-slop-fork-npm-linux-x64.tgz
# or
bun install -g https://github.com/Mabikega/codex-slop-fork/releases/latest/download/codex-slop-fork-npm-linux-x64.tgz
```

Linux arm64:

```sh
npm install -g --force https://github.com/Mabikega/codex-slop-fork/releases/latest/download/codex-slop-fork-npm-linux-arm64.tgz
# or
bun install -g https://github.com/Mabikega/codex-slop-fork/releases/latest/download/codex-slop-fork-npm-linux-arm64.tgz
```

macOS x64:

```sh
npm install -g --force https://github.com/Mabikega/codex-slop-fork/releases/latest/download/codex-slop-fork-npm-darwin-x64.tgz
# or
bun install -g https://github.com/Mabikega/codex-slop-fork/releases/latest/download/codex-slop-fork-npm-darwin-x64.tgz
```

macOS arm64:

```sh
npm install -g --force https://github.com/Mabikega/codex-slop-fork/releases/latest/download/codex-slop-fork-npm-darwin-arm64.tgz
# or
bun install -g https://github.com/Mabikega/codex-slop-fork/releases/latest/download/codex-slop-fork-npm-darwin-arm64.tgz
```

Windows x64:

```sh
npm install -g --force https://github.com/Mabikega/codex-slop-fork/releases/latest/download/codex-slop-fork-npm-win32-x64.tgz
# or
bun install -g https://github.com/Mabikega/codex-slop-fork/releases/latest/download/codex-slop-fork-npm-win32-x64.tgz
```

Windows arm64:

```sh
npm install -g --force https://github.com/Mabikega/codex-slop-fork/releases/latest/download/codex-slop-fork-npm-win32-arm64.tgz
# or
bun install -g https://github.com/Mabikega/codex-slop-fork/releases/latest/download/codex-slop-fork-npm-win32-arm64.tgz
```

## Added Features

### Multi-account support

- Active auth still lives in `~/.codex/auth.json`.
- Saved accounts live in `~/.codex/.accounts/` as normal `auth.json`-compatible files.
- Rate-limit metadata is stored in `~/.codex/.accounts/.rate-limits.json`.
- Settings live in `~/.codex/config-slop-fork.toml`.
- On startup, the fork mirrors the current active auth into saved accounts when needed.

Use `/accounts` to manage saved accounts.

`/accounts` covers browser login, device-code login, API-key login, switching, removal, renaming
misnamed saved-account files, limit inspection, and fork-only settings.

If the latest saved `/usage` snapshot says a saved ChatGPT account is now `free` while its saved
auth still points at a paid tier, `/accounts` marks it as `subscription ran out`. The switch and
limits popups surface that state, startup switches away from such an active account when another
saved account is available, and the delete flow warns that removing the saved auth is irreversible.

`/logout` only clears the active auth. It does not delete saved accounts under `~/.codex/.accounts/`.

### Automatic account switching

When enabled, the fork can switch to another saved account after a ChatGPT rate-limit style failure.

Settings:

- `auto_switch_accounts_on_rate_limit = true`
- `follow_external_account_switches = false`
- `api_key_fallback_on_all_accounts_limited = false`
- `auto_start_five_hour_quota = false`
- `auto_start_weekly_quota = false`
- `show_account_numbers_instead_of_emails = false`
- `show_average_account_limits_in_status_line = false`

Those settings can be toggled from `/accounts -> Settings`, or edited directly in:

- `~/.codex/config-slop-fork.toml`

The switcher ranks saved ChatGPT accounts by the lowest latest known usage snapshot. API-key
accounts are only used when every saved ChatGPT account is unavailable and fallback is enabled.

When `follow_external_account_switches` is enabled, a running session can adopt account changes
written by another Codex instance.

When `show_account_numbers_instead_of_emails` is enabled, fork account menus and switch
notifications replace saved ChatGPT account emails with `Account N`. The numbering is assigned by
sorting the saved ChatGPT accounts by UID when available.

### Telemetry defaults

Analytics are disabled by default in this fork.

If you want to opt in, set:

- `[analytics] enabled = true`

This only changes the analytics default. Feedback remains a separate setting.

### Saved account limits overview

`/accounts -> Saved account limits` shows the latest known usage and reset times for saved accounts.

When `show_average_account_limits_in_status_line` is enabled, the status line can also append the
average remaining 5-hour and weekly saved-account limits next to the active account, for example
`5h 95% (63%) · weekly 99% (27%)`. The extra averages are hidden when there is only one saved
ChatGPT account.

It also supports:

- refreshing due account limits
- force-refreshing all saved ChatGPT accounts
- manually checking cached untouched windows and starting them

Untouched quota behavior:

- a cached window is treated as untouched when it shows `0%` usage and the cached
  `reset_after_seconds` still equals the full `limit_window_seconds`
- if a cached `reset_at` is already in the past, the fork treats that cached window as reset and
  eligible to be started again
- manual and automatic quota starts send one tiny request for the affected account and then refresh
  only that account's `/usage` cache entry
- automatic quota starts are opt-in and use cache on startup instead of forcing a fresh `/usage` fetch
  for every saved account on every boot

Maintenance note:

- the fork TUI keeps `codex-rs/tui/src/slop_fork/ui.rs` as the dispatch/controller seam and splits
  login popup rendering, saved-account rate-limit handling, automation UI code, Autoresearch UI
  code, and Pilot UI code into dedicated internal modules so future fork changes stay local

### Automation engine

`$auto` creates follow-up prompts that run after a completed turn or on a timer.

Examples:

- `$auto on-complete "continue working on this"`
- `$auto on-complete --now "continue working on this"`
- `$auto on-complete --last-user-message`
- `$auto on-complete --times 10 "continue working on this"`
- `$auto on-complete --until 14:00 --round-robin "msg1" "msg2" "msg3"`
- `$auto on-complete --policy 'bash ./.codex/automation/next-message.sh' "continue working on this"`
- `$auto every 10m "run tests"`
- `$auto every --now --last-user-message 10m`
- `$auto every "0 14 * * 1-5" "check deploy"`
- `$auto list`
- `$auto show session:auto-1`
- `$auto pause session:auto-1`
- `$auto resume session:auto-1`
- `$auto rm session:auto-1`

`--now` queues the configured message immediately and counts as the first run. It cannot be
combined with `--policy`.

`--last-user-message` (or `-l`) snapshots the current thread's most recent text user message when
the automation is created, so later user turns do not change it.

Behavior:

- `automation_enabled = false` disables execution without deleting saved definitions
- `automation_default_scope` controls the default storage scope for new rules
- `automation_shell_timeout_ms` sets the default timeout for `--policy` shell commands
- `automation_disable_notify_script = true` suppresses the legacy `notify` script for automation-triggered turns only
- `automation_disable_terminal_notifications = true` suppresses terminal desktop notifications for automation-triggered turns only
- supports `session`, `repo`, and `global` scope
- supports `--times` and `--until`
- supports round-robin messages
- supports interval syntax like `10m`, `2h`, `1d`, and five-field cron for timer rules
- rounds second-based intervals up to whole minutes
- supports shell-policy commands that inspect the last response on `stdin` and return a JSON decision on `stdout`

Persisted automation files:

- global definitions: `~/.codex/codex-slop-fork-automations.toml`
- repo definitions: `<repo>/.codex/codex-slop-fork-automations.toml`
- runtime state: `~/.codex/.codex-slop-fork-automation-state.json`

### Pilot autonomous runs

`$pilot` runs an assistant-controlled autonomous loop without fabricating a new user message for
every continuation cycle. The same persisted Pilot run can also be inspected and controlled through
the experimental app-server methods `pilot/read`, `pilot/start`, and `pilot/control`, with
`pilot/updated` notifications streaming state transitions to subscribed clients.

Examples:

- `$pilot start --for 4h Improve benchmark accuracy end-to-end`
- `$pilot status`
- `$pilot pause`
- `$pilot resume`
- `$pilot wrap-up`
- `$pilot stop`

Behavior:

- Pilot uses controller-owned continuation turns instead of prompting the model to remember to keep
  going on its own
- each Pilot cycle is injected as a developer instruction, not as a synthetic user turn
- TUI and app-server clients coordinate through the saved run state under a file lock, so status
  reads and control actions stay shared even when a thread is not currently loaded
- scheduling still happens only at idle boundaries on a loaded thread, so the outer loop stays
  controller-owned instead of relying on prompt text
- `--for` is a hard scheduling limit for new work; once the deadline is reached, Pilot schedules a
  final wrap-up cycle instead of continuing indefinitely
- `wrap-up` stops broad exploration and asks the model to finish cleanly with a final report
- `pause` lets the active cycle finish but stops further Pilot cycles
- `stop` prevents further Pilot cycles; if a Pilot-controlled turn is already running, it may still
  finish
- disconnecting a client does not fabricate more work; Pilot only schedules new cycles while some
  loaded client or listener reaches an idle boundary for that thread

Persisted Pilot files:

- runtime state: `~/.codex/.codex-slop-fork-pilot-state.json`

### Autoresearch benchmark and research loops

`$autoresearch` runs assistant-controlled optimize or open-ended research loops with native
benchmark tools and session files in the project worktree.

Examples:

- `$autoresearch init "Create an OCR project with CER < 5% on dataset X"`
- `$autoresearch init --open "Explore OCR approaches with CER < 5% on dataset X"`
- `$autoresearch start --max-runs 50 reduce benchmark wall clock time`
- `$autoresearch start --mode research --max-runs 50 search for better OCR architectures`
- `$autoresearch optimize unit test runtime without changing semantics`
- `$autoresearch status`
- `$autoresearch portfolio`
- `$autoresearch discover teacher-student OCR ideas`
- `$autoresearch pause`
- `$autoresearch resume`
- `$autoresearch wrap-up`
- `$autoresearch stop`
- `$autoresearch clear`

Project-local session files:

- `autoresearch.md`
- `autoresearch.sh`
- `autoresearch.checks.sh`
- `autoresearch.ideas.md`
- `autoresearch.jsonl`

Behavior:

- `init` still scaffolds the workspace for optimize mode, including a structured `autoresearch.md`
  plus benchmark/check scripts when it can define them
- `init --open` switches setup into evaluation-first research mode: the model should define the
  evaluator, candidate contract, selection policy, and open questions before it commits to any
  concrete implementation
- the model gets native tools `autoresearch_init`, `autoresearch_run`, `autoresearch_log`,
  `autoresearch_request_discovery`, `autoresearch_log_discovery`, and
  `autoresearch_log_approach`
- if `autoresearch.sh` exists, benchmark runs must use it
- `autoresearch.checks.sh` is optional and can veto `keep`
- `autoresearch.md` can now carry a structured setup with a primary metric, hard constraints,
  ordered staged targets on the primary metric, additional metrics, optional composite-score mode,
  an explicit exploration policy, a discovery policy, hidden constraints/unknowns, a candidate
  contract, and a selection policy
- for reliable staged-target validation before the first journal config exists, write the
  `Primary Metric` section with explicit bullets such as `- Name: latency_ms`, `- Unit: ms`, and
  `- Direction: lower`
- staged targets let the loop keep escalating milestone goals on the same primary metric, such as
  `latency_ms <= 500 ms` and then `latency_ms <= 400 ms`, instead of treating the first threshold
  as the natural stopping point
- `start --mode optimize` runs the existing hill-climbing benchmark loop
- `start --mode research` runs an outer research loop that keeps a portfolio of approaches,
  schedules bounded discovery automatically, and uses the current best candidate as the active
  working context when appropriate
- beyond local benchmark iterations, autoresearch can now queue one bounded discovery pass at a
  time; that pass audits the local repo, can do targeted external research, can use parallel
  sub-agents for distinct questions, and then logs findings back into `autoresearch.jsonl`
- research mode can also queue discovery proactively to refresh portfolio diversity instead of
  waiting for a plateau
- normal benchmark cycles stay separate from discovery cycles; the model should request discovery
  when it hits plateaus, weak assumptions, architecture search needs, evaluation gaps, or other
  cases where broader evidence is more useful than another immediate local tweak
- research mode tracks candidate approaches in the journal with approach ids, families, statuses,
  rationale, risks, and sources so experiments can be compared across distinct lines of attack
- `$autoresearch portfolio` prints the current candidate list, grouped by approach id and family
- `$autoresearch discover [focus]` manually queues a bounded discovery pass
- staged targets must stay ordered from easier to harder, use the primary metric, and use
  compatible units; if that section is malformed, status, prompts, and tool feedback will surface
  a warning until it is fixed
- when the current worktree is inside git, `keep` commits the experiment result and `discard`
  restores the last accepted git revision
- when the current worktree is not inside git, `keep` refreshes a filesystem snapshot and
  `discard` restores that snapshot instead
- in research mode, non-git snapshots are kept per approach under the shared autoresearch snapshot
  root so promising candidates can survive across later exploration
- `autoresearch.md`, benchmark scripts, ideas, and the JSONL journal are preserved across
  discards so the session itself stays intact
- `clear` removes runtime state and the JSONL journal, but leaves the session docs and scripts in
  place

Persisted Autoresearch files:

- runtime state: `~/.codex/.codex-slop-fork-autoresearch-state.json`
- non-git accepted snapshots: `~/.codex/.autoresearch-snapshots/`

### Additional instruction injection

The fork can append extra instructions from `~/.codex/config-slop-fork.toml`.

Available keys:

- `instructions = "..."` adds a global instruction block.
- `instruction_files = ["CLAUDE.md", "GEMINI.md"]` reads extra project docs alongside `AGENTS.md`.
  Absolute paths are also supported, and discovered files are still filtered by the effective filesystem sandbox policy.
- `[projects."/abs/path"]` supports project-scoped `instructions` and `instruction_files`.
