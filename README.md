# factsheet

Per-project memory for AI agents. Facts live in `.factsheet/facts.jsonl`,
one JSON object per line. `factsheet` prints them at session start. Agents add,
edit and drop facts with the CLI; users curate them in an editor.

The inject:

```
## Project memory (3)
[ze6] deploy log at /var/log/app; rotation 7d | check: systemctl status app
[q3o] azure vm resize needs deallocate first
[b78] check: systemctl --user status ssh-add-key
---
factsheet = persistent per-project memory (this CLI), injected each session start.
```

The footer continues with the write rules: when to store a fact, and that every
`add`, `edit` or `drop` needs user approval.

## Quick start

### Claude Code plugin

No Rust needed. Inside Claude Code:

```
/plugin marketplace add idrmn/factsheet
/plugin install factsheet@factsheet
```

Start a new session. The plugin downloads the binary for your platform
(Linux, macOS, Windows) into `~/.claude/plugins/data/factsheet-factsheet/bin/`,
prints the project memory at every session start, and puts `factsheet` on the
agent's PATH.

### cargo

Requires a Rust toolchain.

```sh
cargo install factsheet
```

`~/.claude/settings.json`:

```json
{
  "hooks": {
    "SessionStart": [
      { "hooks": [ { "type": "command", "command": "factsheet" } ] }
    ]
  }
}
```

First fact:

```sh
cd my-project
factsheet add "deploy: run make clean first; stale asset cache breaks hashes" -t deploy
factsheet
```

The last command prints the inject. In a git repository the first `add` appends
`/.factsheet/` to `.git/info/exclude`.

Full command guide, fact style and write rules: `factsheet agent`.

## How-to

### Update or uninstall

Plugin:

```
/plugin update factsheet@factsheet
/plugin uninstall factsheet@factsheet
```

cargo:

```sh
cargo install factsheet --force
cargo uninstall factsheet
```

### Hook per project

Put the same `SessionStart` block into `.claude/settings.local.json` in the
project.

### Hook for droid (Factory)

`~/.factory/hooks.json`, event keys at the top level:

```json
{
  "SessionStart": [
    { "hooks": [ { "type": "command", "command": "/home/USER/.cargo/bin/factsheet", "timeout": 15 } ] }
  ]
}
```

### Hook for Codex CLI

`~/.codex/hooks.json` or `<repo>/.codex/hooks.json`:

```json
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "startup|resume",
        "hooks": [ { "type": "command", "command": "factsheet", "timeout": 10 } ]
      }
    ]
  }
}
```

Then run `/hooks` in a session and trust the new hook.

### Hook for Cursor

`~/.cursor/hooks.json`:

```json
{
  "version": 1,
  "hooks": {
    "sessionStart": [ { "command": "./hooks/factsheet-context.sh", "timeout": 10 } ]
  }
}
```

`~/.cursor/hooks/factsheet-context.sh`, executable:

```sh
#!/bin/bash
input=$(cat)
root=$(jq -r '.workspace_roots[0] // empty' <<<"$input")
cd "${root:-$CURSOR_PROJECT_DIR}" || exit 0
ctx=$(factsheet 2>/dev/null)
jq -n --arg c "$ctx" '{additional_context: $c}'
```

### Hook for agy (Antigravity CLI)

agy has no `SessionStart`; `PreInvocation` with a stamp per conversation.

`~/.gemini/config/hooks.json`:

```json
{
  "factsheet-inject": {
    "PreInvocation": [
      { "type": "command", "command": "/home/USER/.gemini/config/factsheet-bootstrap.sh", "timeout": 10 }
    ]
  }
}
```

`~/.gemini/config/factsheet-bootstrap.sh`, executable:

```sh
#!/bin/bash
input=$(cat)
cid=$(jq -r '.conversationId // empty' <<<"$input")
stamp="/tmp/factsheet-inject-${cid}"
if [ -n "$cid" ] && [ ! -e "$stamp" ]; then
  touch "$stamp"
  ws=$(jq -r '.workspacePaths[0] // empty' <<<"$input")
  [ -d "$ws" ] && cd "$ws"
  ctx=$(factsheet 2>/dev/null)
  jq -n --arg c "$ctx" '{injectSteps: (if $c == "" then [] else [{ephemeralMessage: $c}] end)}'
else
  echo '{"injectSteps": []}'
fi
```

### Extend or change the write rules

`user_instructions` in `~/.config/factsheet/config.toml` is appended after the
footer. The directory is `$XDG_CONFIG_HOME`, else `$HOME/.config`, else
`%APPDATA%`. Any TOML string:

```toml
user_instructions = """
Add facts without asking; I review them with `factsheet curate`.
Tag every fact with the ticket id, for example PROJ-123.
Store command recipes with the tag `ref`, never as hot facts.
"""
```

### Curate facts by hand

`factsheet curate` writes every fact to `.factsheet/curate.txt`, opens it in
`$VISUAL`, `$EDITOR`, `vi` or `notepad`, and applies the result when the editor
exits. Facts are grouped by their first tag, hot facts first, `ref` facts after.

```
# factsheet curate. One fact per line: [id] text | check: cmd  #tag #tag  (date)
# delete a line = drop | change text, check or tags = edit | new line without [id] = add
# lines starting with # and the (date) are ignored | empty sheet = abort

# deploy
[ze6] deploy log at /var/log/app; rotation 7d | check: systemctl status app  #deploy  (2026-08-19)

# ref: azure
[q3o] azure vm resize needs deallocate first  #ref #azure  (2026-08-19)
```

Fields are separated by two spaces. A changed text or check refreshes the date;
a tag change alone does not. A parse error, unknown id or empty sheet aborts the
run; the sheet stays on disk. The store is locked while the editor is open.

### Find and clean duplicates

```sh
factsheet find "deploy cache"   # facts ranked by word overlap
factsheet dedup                 # near-duplicate pairs and over-length facts
factsheet ls --stale 30d        # facts not written for 30+ days
factsheet projects              # every project with a store on this machine
```

## Reference

- `factsheet --help`: commands and flags.
- `factsheet agent`: the guide the agent follows, with line types, tag `ref`,
  fact style and the write rules.
- Store: `{project_root}/.factsheet/facts.jsonl`, plus `.lock`, `facts.jsonl.bak`
  (one-step undo after edit or drop) and `curate.txt` while curating.
- Config: `~/.config/factsheet/config.toml` with `max_text` (default 200),
  `hot_warn` (default 40) and `user_instructions`. Created with defaults on
  first run. A non-string value or invalid TOML puts a hint into the inject
  instead of the text.
- Registry: `~/.config/factsheet/projects`, one store path per line, pruned by
  `factsheet projects`.

## Design notes

- Project root is the nearest directory up from CWD with `.factsheet/` or
  `.git`; otherwise CWD. In a project without git, run the first `add` from the
  project root.
- Reserved tag `ref` marks reference facts (keys, IPs, lookup lists). They stay
  out of the inject; a footer lists their tag counts.
- The store is opaque: edit only via `factsheet`.
- `factsheet` never executes stored `check` commands.
- `add`, `edit` and `drop` hold an OS lock on `.factsheet/.lock` while they run.
- `add` appends one JSONL line, after a newline if the file lacks one. `edit` and
  `drop` keep the previous file as `facts.jsonl.bak` and stop if the backup fails.
- A missing store is treated as empty; other read errors stop the command.
- No database, daemon or network. Every command reads the whole file.

## License

MIT.
