# gh-board-cli

GitHub Project のボードを表示しつつ、WebSocket で受信した GitHub イベントに応じて Hook を実行する TUI です。

## Setup

```sh
mise install
cargo build --release
```

[gh](https://cli.github.com/) にログイン済みであること（`gh project list` / `gh api graphql` / `gh issue view` を使用します）。Project の取得には `read:project` スコープが必要です（`gh auth refresh -s read:project`）。

## Usage

```sh
gh-board-cli
```

| Key | 動作 |
| --- | --- |
| `q` / `Esc` / `Ctrl-C` | 終了 |
| `r` | ボード再読込 |
| `Tab` | ペイン切替 |
| `j` / `k` / `↑` / `↓` | スクロール |
| `G` / `End` | イベントログの末尾に追従 |
| クリック (`#n ...`) | Issue をブラウザで開く (`gh issue view --web`) |
| ホイール | カーソル下のペインをスクロール |

- 左ペインは `gh project list` で取得した開いている Project ごとに、Status 別の Issue を表示します（Done は更新日時が新しい順に最大10件）
- 🟢 はその Issue のイベントに一致した Hook を実行中であることを示します
- 右ペインには受信イベントと Hook の出力を表示します。Hook の出力は1行ずつ届いた時点で表示し、`[#n]` でどの Issue の Hook かを示します
- マウス入力を有効にしているため、ターミナルでの文字選択は Shift (iTerm2 / Terminal.app では Option) を押しながら行ってください

## 設定 (`.ghboard.yaml`)

YAML 形式の設定ファイルを、次の順で探して最初に見つかったものを読み込みます。

1. カレントディレクトリの `.ghboard.yaml` → `.ghboard.yml`
2. ホームディレクトリの `.ghboard.yaml` → `.ghboard.yml`

```yaml
listen: wss://ws-{cluster}.pusher.com/app/{app_key}?protocol=7&client=rust&version=1.0.0
channel: gh-board
targets:
  - owner/repo01
  - owner/repo02
hooks:
  - on:
      - "issues:*"
      - "issue_comment:*"
    status:
      - 'In Progress'
      - 'In Review'
    command: |
      echo ${{ issue_number }}
  - on:
      - "issue_comment:created"
    command: 'echo "${{ event_name }}:${{ event_action }} #${{ issue_number }}"'
```

> [!TIP]
> YAML では、値に ` #`（コメント扱い）や `: ` が含まれると意図どおりに読み込まれません。`command` はクォートで囲むか、`|` のブロック形式で書くと安全です。

- `listen`: 購読する WebSocket の URL
- `channel` (任意): Pusher のチャンネル名。指定時は接続後に `pusher:subscribe` を送信します
- `targets` (任意): Hook の対象リポジトリ。空・未指定なら全リポジトリが対象です
- `hooks`: HookAction の配列
  - `on`: 実行対象とする HookType の配列。いずれかに一致すれば実行します
    - HookType: `{event_name}:{event_action}`。`*` は任意の文字列に一致します
  - `status` (任意): 実行対象とする Issue の Project ステータス。空・未指定ならステータスに関係なく実行します
  - `command`: `sh -c` で実行するコマンド

### 組み込み変数

`${{ 変数名 }}` で埋め込めます。payload の各項目名がそのまま変数名になります（`repository` / `event_name` / `event_action` / `issue_number`、およびそれ以外に含まれる項目）。

- 存在しない変数は空文字に展開されます
- 文字列以外の値は JSON として展開されます
- 値がオブジェクト・配列の場合は `.` 区切りで中身を参照できます（例: `${{ issue.labels.0.name }}`）

> [!WARNING]
> 変数はエスケープせずにコマンドへ展開されます。Issue タイトルやコメント本文など外部入力をシェルに渡す場合は注意してください。

### 受信する payload

```json
{
  "repository": "owner/repo",
  "event_name": "issues",
  "event_action": "opened",
  "issue_number": 1
}
```

- `repository`: リポジトリ名 (`{owner}/{repo}`)
- `event_name`: `github.event_name` の内容（`issues`, `issue_comment` など）
- `event_action`: `github.event.action` の内容（省略時は空文字）
- `issue_number`: Issue 番号（数値または数字の文字列）。🟢 の表示に使います

Pusher の envelope（`{"event": "...", "data": "<payload の JSON 文字列>"}`）で届いた場合は `data` を展開します。

### Hook の実行条件

1. `targets` が空、または `repository` が `targets` に含まれる
2. HookType `{event_name}:{event_action}` が HookAction の `on` のいずれかに一致する
3. HookAction の `status` が空・未指定、または Issue の Project ステータスが `status` に含まれる（HookAction ごとに判定）
   - `issue_number` がある場合、`gh issue view <issue_number> -R <repository> --json comments,projectItems` で Issue の状態を取得します
   - `gh` の実行に失敗した場合は Hook を実行しません
   - Issue が複数の Project に入っている場合は、いずれかのステータスが含まれれば実行します
   - ステータス名は大文字・小文字を区別して比較します
   - `issue_number` がないイベントでは、`status` を指定した HookAction は実行しません
4. `issue_number` があり、Issue にコメントがある場合、最新コメントの `(id, body)` が前回の取得値と異なる（初回は実行）
   - Issue にコメントが無い場合は判定せずに実行します
   - 3 で全ての HookAction が実行対象外になった場合は判定しません（前回値も更新しません）
   - 前回値はリポジトリ + Issue 番号ごとに保持します（起動中のみ）

実行しなかった HookAction は、右ペインに `skipped: <HookType> #n: <理由>` と表示します。

### Hook の実行順序

- 別の Issue の Hook は並行して実行します
- 同じ Issue へのイベントは受信順に1つずつ処理します（コメントの前回値との比較が順序に依存するため）
- 1つのイベントに一致した複数の HookAction は、設定順に1つずつ実行します

## 開発

```sh
cargo test
# Pusher 互換のモックサーバー（ws://127.0.0.1:8765）
cargo run --example mock_server -- "<owner>/<repo>"
# ポートと Issue 番号を指定（#1 と #2 に交互にイベントを送信）
cargo run --example mock_server -- "<owner>/<repo>" 8765 1,2
```
