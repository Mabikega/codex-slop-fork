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
  login popup rendering, saved-account rate-limit handling, and automation UI code into dedicated
  internal modules so future fork changes stay local

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

### Additional instruction injection

The fork can append extra instructions from `~/.codex/config-slop-fork.toml`.

Available keys:

- `instructions = "..."` adds a global instruction block.
- `instruction_files = ["CLAUDE.md", "GEMINI.md"]` reads extra project docs alongside `AGENTS.md`.
  Absolute paths are also supported, and discovered files are still filtered by the effective filesystem sandbox policy.
- `[projects."/abs/path"]` supports project-scoped `instructions` and `instruction_files`.
