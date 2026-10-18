# gh-board-cli

GitHub Project のボードを表示しつつ、WebSocket で受信した GitHub イベントに応じて Hook を実行する TUI です。

## Setup

```sh
mise install
cargo build --release
```

[gh](https://cli.github.com/) にログイン済みであること（Project の取得に `gh api graphql` を使用します）。

## Usage

```sh
gh-board-cli "<owner>/<repo>"
```

| Key | 動作 |
| --- | --- |
| `q` / `Esc` / `Ctrl-C` | 終了 |
| `r` | ボード再読込 |
| `Tab` | ペイン切替 |
| `j` / `k` / `↑` / `↓` | スクロール |
| `G` / `End` | イベントログの末尾に追従 |

## 設定 (`.ghboard.json`)

カレントディレクトリ、なければホームディレクトリの `.ghboard.json` を読み込みます。

```json
{
  "listen": "wss://ws-{cluster}.pusher.com/app/{app_key}?protocol=7&client=rust&version=1.0.0",
  "channel": "gh-board",
  "hooks": {
    "issues:*": [
      { "type": "command", "command": "echo ${{ event.issue.id }}" }
    ],
    "issue_comment:created": [
      { "type": "command", "command": "echo ${{ event.comment.body }}" }
    ]
  }
}
```

- `listen`: 購読する WebSocket の URL
- `channel` (任意): Pusher のチャンネル名。指定時は接続後に `pusher:subscribe` を送信します
- `hooks`: HookType → HookAction の配列
  - HookType: `{event_name}:{event.action}`。`*` は任意の文字列に一致します
  - HookAction: `{ "type": "command", "command": "..." }` を `sh -c` で実行します

### 組み込み変数

`${{ 変数名 }}` で埋め込めます。`.` 区切りでフィールド、数字で配列要素を参照できます（例: `${{ event.issue.labels.0.name }}`）。文字列以外の値は JSON として展開されます。

- `event_name`: `github.event_name` の内容
- `event`: `github.event` の内容

> [!WARNING]
> 変数はエスケープせずにコマンドへ展開されます。Issue タイトルやコメント本文など外部入力をシェルに渡す場合は注意してください。

### 受信する payload

```json
{
  "repository": "owner/repo",
  "event_name": "issues",
  "event": { "action": "opened", "...": "..." }
}
```

Pusher の envelope（`{"event": "...", "data": "<payload の JSON 文字列>"}`）で届いた場合は `data` を展開します。`repository` が引数と一致しないイベントは Hook を実行しません。

## 開発

```sh
cargo test
# Pusher 互換のモックサーバー（ws://127.0.0.1:8765）
cargo run --example mock_server -- "<owner>/<repo>"
```
