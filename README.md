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
| クリック (`#n ...`) | Issue をブラウザで開く (`gh issue view --web`) |
| ホイール | カーソル下のペインをスクロール |

- 左ペインは Project の Status ごとに Issue を表示します（Done は更新日時が新しい順に最大10件）
- 🟢 はその Issue のイベントに一致した Hook を実行中であることを示します
- マウス入力を有効にしているため、ターミナルでの文字選択は Shift (iTerm2 / Terminal.app では Option) を押しながら行ってください

## 設定 (`.ghboard.json`)

カレントディレクトリ、なければホームディレクトリの `.ghboard.json` を読み込みます。

```json
{
  "listen": "wss://ws-{cluster}.pusher.com/app/{app_key}?protocol=7&client=rust&version=1.0.0",
  "channel": "gh-board",
  "hooks": {
    "issues:*": [
      { "type": "command", "command": "echo ${{ issue_number }}" }
    ],
    "issue_comment:created": [
      { "type": "command", "command": "echo ${{ event_name }}:${{ event_action }} #${{ issue_number }}" }
    ]
  }
}
```

- `listen`: 購読する WebSocket の URL
- `channel` (任意): Pusher のチャンネル名。指定時は接続後に `pusher:subscribe` を送信します
- `hooks`: HookType → HookAction の配列
  - HookType: `{event_name}:{event_action}`。`*` は任意の文字列に一致します
  - HookAction: `{ "type": "command", "command": "..." }` を `sh -c` で実行します

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

Pusher の envelope（`{"event": "...", "data": "<payload の JSON 文字列>"}`）で届いた場合は `data` を展開します。`repository` が引数と一致しないイベントは Hook を実行しません。

## 開発

```sh
cargo test
# Pusher 互換のモックサーバー（ws://127.0.0.1:8765）
cargo run --example mock_server -- "<owner>/<repo>"
```
