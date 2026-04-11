# Codex Slop Fork

[English](README.md) | [日本語](README.ja.md)

Codex 自身に「お願いだからこの機能を追加して。バグはなしで」と頼んで作らせた Codex のフォークです。

このフォークを長期的に保守するつもりはありません。

## インストール

利用中のプラットフォーム向け最新リリースパッケージは npm または Bun でインストールできます。
npm の `--force` は、固定の `releases/latest/download/...` URL でも更新時に最新パッケージへ差し替えるために付けています。
同じコマンドを再実行すると最新版へ更新できます。
インストールされるコマンド名は `codex-slop-fork` です。

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

## 追加機能

### マルチアカウント対応

- 現在アクティブな認証情報は引き続き `~/.codex/auth.json` に保存されます。
- 保存済みアカウントは通常の `auth.json` 互換ファイルとして `~/.codex/.accounts/` に保存されます。
- レート制限メタデータは `~/.codex/.accounts/.rate-limits.json` に保存されます。
- 設定は `~/.codex/config-slop-fork.toml` に保存されます。
- 起動時、このフォークは必要に応じて現在のアクティブ認証情報を保存済みアカウントへミラーします。

保存済みアカウントの管理には `/accounts` を使います。

`/accounts` では、ブラウザログイン、デバイスコードログイン、API キーログイン、切り替え、削除、誤った保存済みアカウントファイル名のリネーム、制限確認、フォーク専用設定を扱えます。

保存済み ChatGPT アカウントの最新 `/usage` スナップショットが `free` になり、保存済み認証情報側は有料プランのままなら、`/accounts` はそのアカウントを `subscription ran out` と表示します。切り替えポップアップと制限ポップアップでもこの状態を表示し、起動時は別の保存済みアカウントがあれば自動で切り替え、削除フローでは復元不能であることを警告します。

`/logout` はアクティブな認証情報だけを消去します。`~/.codex/.accounts/` 配下の保存済みアカウントは削除しません。

### 自動アカウント切り替え

有効化すると、このフォークは ChatGPT のレート制限系の失敗後に別の保存済みアカウントへ切り替えられます。

設定項目:

- `auto_switch_accounts_on_rate_limit = true`
- `follow_external_account_switches = false`
- `api_key_fallback_on_all_accounts_limited = false`
- `auto_start_five_hour_quota = false`
- `auto_start_weekly_quota = false`
- `show_account_numbers_instead_of_emails = false`
- `show_average_account_limits_in_status_line = false`

これらの設定は `/accounts -> Settings` から切り替えるか、次のファイルを直接編集できます。

- `~/.codex/config-slop-fork.toml`

スイッチャーは、保存済み ChatGPT アカウントを、最新で判明している使用量スナップショットが最も低いものから優先します。API キーアカウントは、保存済み ChatGPT アカウントがすべて利用不能で、かつフォールバックが有効な場合にのみ使われます。

`follow_external_account_switches` を有効にすると、実行中のセッションは別の Codex インスタンスによって書き込まれたアカウント変更を取り込めます。

`show_account_numbers_instead_of_emails` を有効にすると、フォークのアカウント一覧や切り替え通知では、保存済み ChatGPT アカウントのメールアドレスを表示せず `Account N` を表示します。番号は、保存済み ChatGPT アカウントを利用可能なら UID 順に並べて決まります。

### テレメトリの既定値

このフォークでは、analytics は既定で無効です。

有効化したい場合は、次を設定してください。

- `[analytics] enabled = true`

これは analytics の既定値だけを変えるものです。feedback は別設定のままです。

### 保存済みアカウント制限の一覧

`/accounts -> Saved account limits` では、保存済みアカウントの最新の使用量とリセット時刻を表示します。

`show_average_account_limits_in_status_line` を有効にすると、ステータスラインに、アクティブアカウントの横へ保存済みアカウントの平均残量も追記できます。たとえば `5h 95% (63%) · weekly 99% (27%)` のようになります。追加の平均表示は、保存済み ChatGPT アカウントが 1 つしかない場合は非表示です。

この画面では次も行えます。

- 期限の来たアカウント制限の更新
- すべての保存済み ChatGPT アカウントの強制更新
- 手つかずのままキャッシュされたウィンドウの手動確認と開始

手つかず quota の挙動:

- キャッシュされたウィンドウは、使用率が `0%` で、かつキャッシュされた `reset_after_seconds` が `limit_window_seconds` の全量とまだ一致している場合に手つかずと見なされます
- キャッシュされた `reset_at` がすでに過去なら、そのキャッシュ済みウィンドウはリセット済みと見なされ、再開可能になります
- 手動開始と自動開始はいずれも対象アカウントに対して小さなリクエストを 1 回送り、その後そのアカウントの `/usage` キャッシュエントリだけを更新します
- 自動 quota 開始はオプトインであり、起動時には毎回すべての保存済みアカウントへ新しい `/usage` 取得を強制せず、キャッシュを使います

保守メモ:

- フォークの TUI は `codex-rs/tui/src/slop_fork/ui.rs` をディスパッチ兼コントローラの継ぎ目として維持し、ログインポップアップ描画、保存済みアカウントのレート制限処理、オートメーション UI コード、Autoresearch UI コード、Pilot UI コードを専用の内部モジュールへ分割して、今後のフォーク変更を局所化しています

### オートメーションエンジン

`$auto` は、ターン完了後またはタイマーに応じて実行されるフォローアッププロンプトを作成します。

例:

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

`--now` は、設定したメッセージをすぐにキューへ入れ、その実行を 1 回目として数えます。
`--policy` とは併用できません。

`--last-user-message` (または `-l`) は、オートメーション作成時点のスレッド内で直近のテキスト
ユーザーメッセージを固定で取り込み、その後のユーザー入力で変わらないようにします。

挙動:

- `automation_enabled = false` は、保存済み定義を削除せずに実行だけを無効化します
- `automation_default_scope` は、新しいルールの既定保存スコープを制御します
- `automation_shell_timeout_ms` は、`--policy` シェルコマンドの既定タイムアウトを設定します
- `automation_disable_notify_script = true` は、オートメーション由来のターンに限って従来の `notify` スクリプトを抑止します
- `automation_disable_terminal_notifications = true` は、オートメーション由来のターンに限って端末のデスクトップ通知を抑止します
- `session`、`repo`、`global` スコープをサポートします
- `--times` と `--until` をサポートします
- ラウンドロビンのメッセージをサポートします
- `10m`、`2h`、`1d` のような間隔構文と、5 フィールド cron によるタイマールールをサポートします
- 秒ベースの間隔は分単位へ切り上げます
- 最後のレスポンスを `stdin` から読み取り、JSON の判断を `stdout` に返すシェルポリシーコマンドをサポートします

永続化されるオートメーションファイル:

- グローバル定義: `~/.codex/codex-slop-fork-automations.toml`
- リポジトリ定義: `<repo>/.codex/codex-slop-fork-automations.toml`
- 実行時状態: `~/.codex/.codex-slop-fork-automation-state.json`

### Pilot 自律実行

`$pilot` は、継続ごとに新しいユーザーメッセージを捏造せず、アシスタント主導の自律ループを実行します。
同じ保存済み Pilot 実行は、experimental な app-server メソッド `pilot/read`、
`pilot/start`、`pilot/control` と通知 `pilot/updated` からも参照、制御できます。

例:

- `$pilot start --for 4h ベンチマーク精度を end-to-end で改善する`
- `$pilot status`
- `$pilot pause`
- `$pilot resume`
- `$pilot wrap-up`
- `$pilot stop`

挙動:

- Pilot は、モデル自身に「継続を覚えさせる」のではなく、コントローラ側で継続を管理します
- 各 Pilot サイクルは合成ユーザーターンではなく developer 指示として注入されます
- TUI クライアントと app-server クライアントは、ファイルロック付きの保存済み Pilot 状態を
  共有して参照、制御します。スレッドが未ロードでも状態確認や制御は可能です
- 次サイクルのスケジュール自体はロード済みスレッドの idle 境界でのみ行われるため、外側の
  ループはプロンプト文ではなくコントローラ側が保持します
- `--for` は新規作業のハードなスケジュール上限です。期限到達後は無限継続せず、最後の wrap-up サイクルを 1 回だけ実行します
- `wrap-up` は広い探索を止め、きれいに締める最終レポートを求めます
- `pause` は現在のサイクル完了までは許可しますが、その後の Pilot サイクルを止めます
- `stop` は以後の Pilot サイクルを止めます。Pilot 制御中のターンがすでに走っている場合は、そのターンだけは完了することがあります
- クライアント切断だけで新しい作業が勝手に増えることはありません。Pilot は、そのスレッドを
  読み込んでいるクライアントやリスナーが idle 境界に到達したときだけ次サイクルを積みます

永続化される Pilot ファイル:

- 実行時状態: `~/.codex/.codex-slop-fork-pilot-state.json`

### Autoresearch ベンチマーク/研究ループ

`$autoresearch` は、プロジェクト内のセッションファイルとネイティブなベンチマーク
ツールを使って、アシスタント主導の optimize ループまたは open-ended research ループを実行します。

例:

- `$autoresearch init "Create an OCR project with CER < 5% on dataset X"`
- `$autoresearch init --open "Explore OCR approaches with CER < 5% on dataset X"`
- `$autoresearch start --max-runs 50 ベンチマークの経過時間を短くする`
- `$autoresearch start --mode research --max-runs 50 より良い OCR アーキテクチャを探す`
- `$autoresearch 振る舞いを変えずに unit test の実行時間を短くする`
- `$autoresearch status`
- `$autoresearch portfolio`
- `$autoresearch discover teacher-student OCR ideas`
- `$autoresearch pause`
- `$autoresearch resume`
- `$autoresearch wrap-up`
- `$autoresearch stop`
- `$autoresearch clear`

プロジェクト内のセッションファイル:

- `autoresearch.md`
- `autoresearch.sh`
- `autoresearch.checks.sh`
- `autoresearch.ideas.md`
- `autoresearch.jsonl`

挙動:

- `init` は従来どおり optimize mode 向けの作業ツリーを作らせます。構造化された `autoresearch.md` と、定義できる場合はベンチマーク/チェック用スクリプトを含みます
- `init --open` は evaluation-first な research mode 用セットアップです。モデルは、特定の実装へ固定する前に evaluator、candidate contract、selection policy、open questions を定義するべきです
- モデルには `autoresearch_init`、`autoresearch_run`、`autoresearch_log`、
  `autoresearch_request_discovery`、`autoresearch_log_discovery`、
  `autoresearch_log_approach` のネイティブツールが渡されます
- `autoresearch.sh` が存在する場合、ベンチマーク実行はそれを使う必要があります
- `autoresearch.checks.sh` は任意ですが、失敗した場合は `keep` を拒否できます
- `autoresearch.md` には、primary metric、hard constraints、primary metric 上の順序付き staged targets、additional metrics、任意の composite-score mode、exploration policy を構造化して持たせられます
- `autoresearch.md` には discovery policy、hidden constraints / unknowns、candidate contract、selection policy も書けます
- staged targets を最初の journal config 前から確実に検証したい場合は、`Primary Metric` セクションを `- Name: latency_ms`、`- Unit: ms`、`- Direction: lower` のような明示的な bullet で書いてください
- staged targets を使うと、たとえば `latency_ms <= 500 ms` を達成した後も、次の `latency_ms <= 400 ms` へ自動で目標を繰り上げながら同じ指標を継続的に改善できます
- `start --mode optimize` は従来の hill-climbing ベンチマークループを実行します
- `start --mode research` は外側の research ループを実行します。approach の portfolio を保持し、bounded discovery を自動で入れ、適切なときは最有力 candidate を active work context として使います
- ローカルなベンチマーク反復とは別に、autoresearch は 1 回ずつ bounded discovery pass をキューできます。この pass は repo を監査し、必要なら限定的に外部調査し、distinct な問いごとに並列 sub-agent を使い、その結果を `autoresearch.jsonl` に記録します
- research mode では plateau を待たず portfolio diversity を補充するための discovery も自動でキューできます
- 通常の benchmark cycle と discovery cycle は分離されています。plateau、weak assumption、architecture search、evaluation gap などで広い証拠集めが次のローカル変更より有益なときに discovery を使います
- research mode は、approach id、family、status、rationale、risks、sources を journal に記録し、異なる方針同士を比較できるようにします
- `$autoresearch portfolio` は現在の candidate 一覧を approach id と family つきで表示します
- `$autoresearch discover [focus]` は bounded discovery pass を手動でキューします
- staged targets は、primary metric を使い、互換性のある unit を使い、易しい目標から難しい目標へ順序づけられている必要があります。壊れている場合は、status、prompt、tool feedback に警告が出続けます
- 現在の作業ツリーが git 管理下なら、`keep` はその実験結果をコミットし、`discard` は最後に受理された git リビジョンへ戻します
- git 管理外なら、`keep` はファイルシステムスナップショットを更新し、`discard` はそのスナップショットを復元します
- research mode の non-git snapshot は共有の autoresearch snapshot root 配下で approach ごとに保持されるため、有望な candidate を後からまた復元できます
- `autoresearch.md`、ベンチマークスクリプト、ideas、JSONL ジャーナルは discard 後も保持されるため、セッション自体は残ります
- `clear` は実行時状態と JSONL ジャーナルを削除しますが、セッション文書やスクリプトは残します

永続化される Autoresearch ファイル:

- 実行時状態: `~/.codex/.codex-slop-fork-autoresearch-state.json`
- 非 git ワークツリー用の accepted スナップショット: `~/.codex/.autoresearch-snapshots/`

### 追加指示の注入

このフォークは `~/.codex/config-slop-fork.toml` から追加指示を追記できます。

利用可能なキー:

- `instructions = "..."` はグローバルな指示ブロックを追加します。
- `instruction_files = ["CLAUDE.md", "GEMINI.md"]` は `AGENTS.md` と並ぶ追加のプロジェクト文書を読み込みます。絶対パスもサポートされ、見つかったファイルは有効なファイルシステムサンドボックスポリシーで引き続きフィルタされます。
- `[projects."/abs/path"]` は、プロジェクト単位の `instructions` と `instruction_files` をサポートします。
