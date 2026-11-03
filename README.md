# spavn

A terminal-native coding agent in a single Rust binary. It streams from local or cloud models (OpenAI-compatible, plus native Anthropic Messages), calls tools against your machine, and keeps every session on disk — no daemon, no telemetry, no runtime dependencies beyond the shell you already use.

```
$ spavn "audit the auth module and write the missing tests"
```

## Highlights

- **Streaming TUI** — token-by-token rendering with a live status bar, interruptible mid-stream, and a model picker you can drive from the keyboard.
- **Local first, cloud when needed** — point `spavn` at LM Studio, Ollama, llama.cpp, or vLLM; switch to a hosted model with `/model` without restarting.
- **Real tools, guarded** — file reads/writes, regex search, shell commands, web search/fetch, persistent memory, and on-demand skills, all behind one permission gate.
- **Plan / Code phases** — a read-only 📐 PLAN phase that hard-locks `write`/`edit`/`bash` at the tool layer (not just the prompt), and a 💻 CODE phase to act. Toggle live with `/plan`, `/code`, or `\`.
- **Three access modes** — from ask-everything to fully autonomous, with destructive commands and secret files still fenced off.
- **Sessions as JSONL** — resumable, forkable, greppable, `0600` on disk.
- **HMAC-chained audit log** — every AI action is recorded in a tamper-evident trail (`/audit`, `/blame`).

## Requirements

- macOS or Linux (the TUI uses termios; Windows is not supported)
- Rust stable (edition 2021)
- `rg` on `PATH` for the `search` tool
- Optional: `ssh` for remote hosts, `node`/`python3` for quota and memory helper scripts

## Install

```sh
git clone https://github.com/tugayoktayokay/spavn-cli.git
cd spavn-cli
cargo build --release
ln -sf "$PWD/target/release/spavn" ~/bin/spavn
```

`~/bin` must be on your `PATH`.

## Quick start

Start the TUI:

```sh
spavn
```

Or run a single turn and exit:

```sh
spavn "explain src/router.rs"
```

### Against a local model

```sh
export SPAVN_LOCAL_BASE_URL="http://127.0.0.1:1234/v1"
spavn -m local "write a unit test for parse_args"
```

`-m local` asks the configured discovery endpoint which model is loaded and picks it. Use `-m local:<model-id>` to pin a specific one.

### Against a hosted model

```sh
export OPENAI_API_KEY="sk-..."
spavn -m gpt-5.4 "..."
```

Keys can also live in `~/.spavn/secrets.env` (see [State and storage](#state-and-storage)).

## CLI

| Flag | Description |
| --- | --- |
| `-m, --model <key>` | Model key, provider-prefixed model (`openai:gpt-5.4`), or `local[:id]` |
| `-a, --access <mode>` | `safe`, `auto` (default), or `sovereign` |
| `-P, --phase <phase>` | `plan` (read-only) or `code` (default); useful for scripted plan runs |
| `-H, --host <alias>` | Default target host for tools (`local` by default) |
| `-c, --continue` | Resume the most recent session |
| `-r, --resume` | Open the session picker |
| `-t, --think` | Show the reasoning stream |
| `-h, --help` | Usage summary |

Any remaining positional arguments are joined into the prompt. With a prompt, `spavn` runs one turn and exits.

## Model routing

`resolve_endpoint` tries, in order:

1. `local` / `local:<model-id>` when `local_base_url` is configured.
2. Providers defined in `~/.spavn/config.json` (`providers[]`), matched by model key, model ID, or `<provider>:<model>`.
3. The built-in catalog: `openai`, `anthropic`, `gemini`, `openrouter`, `deepseek`, `minimax`, `glm`, `groq`, `xai`, `ollama`.
4. Any model currently advertised by the local discovery endpoint.
5. Fallback: first configured provider, else `SPAVN_BASE_URL`, else `https://api.openai.com/v1`; key from `SPAVN_API_KEY`, else `OPENAI_API_KEY`.

**Anthropic:** endpoints whose base URL is `api.anthropic.com` (the built-in `anthropic` / `sonnet` / `opus` keys) speak the **native Anthropic Messages API** (`POST /v1/messages`, `x-api-key` + `anthropic-version`, `max_tokens`, `content_block_delta` / `input_json_delta` SSE). spavn converts its internal message/tool format to Anthropic's block format (system extraction, `tool_use` / `tool_result` blocks, consecutive-role merging) and maps `stop_reason` back to the shared `finish_reason`. To point a *custom* Anthropic-protocol proxy whose host is not `anthropic.com` at the native path, set `SPAVN_FORCE_ANTHROPIC=1`. OpenAI-compatible Anthropic proxies keep using the OpenAI path automatically.

Inside the TUI:

- `/model` (`/m`) opens the two-column provider/model picker; `/local` starts on the local provider.
- `/model <key>` switches directly.
- `/byok` manages API keys and custom base URLs.

## Interactive mode

| Key | Action |
| --- | --- |
| `Enter` | Send the message |
| `Esc` | Cancel the running turn or close a menu |
| `Tab` | Cycle access mode |
| `Shift+Tab` | Cycle access mode |
| `\` | Toggle phase (📐 PLAN ↔ 💻 CODE) |
| `Ctrl+R` | Reload configuration |
| `Up` / `Down` | History or menu navigation |
| `Ctrl+C` | Interrupt the agent |

While a turn is running, input either **steers** the agent immediately or is **queued**, depending on `busy_input_mode`.

### Slash commands

| Command | Purpose |
| --- | --- |
| `/model`, `/local` | Provider/model picker |
| `/access <mode>` | Switch permission mode |
| `/plan`, `/code` | Switch phase: read-only plan vs act |
| `/byok`, `/keys` | API key and provider manager |
| `/resume`, `/sessions` | Session picker |
| `/new`, `/fork` | New session, or branch the current one |
| `/compact [focus]` | Compact the context, optionally around a topic |
| `/clear` | Clear the transcript |
| `/undo` | Revert the last file change made by the agent |
| `/queue`, `/qedit` | Inspect or edit queued messages |
| `/think` | Toggle the reasoning stream |
| `/role [name]` | Switch expertise role (Auto, Coder, Reviewer, Security, Test, Docs, Refactor) |
| `/host [alias]` | Choose the default tool target |
| `/skills`, `/skill add\|show\|rm` | Manage skills |
| `/pin <note>`, `/pins`, `/unpin <id>` | Persistent project rules |
| `/goal`, `/goal <text>`, `/goal done <id>` | Goals |
| `/janitor` | Run quality gates (check, types, lint, test) |
| `/audit`, `/blame <file>` | Audit trail and per-file history |
| `/wiki` | Project module map |
| `/fleet` | Remote host radar |
| `/quota` | Provider quota card |
| `/lang`, `/tr`, `/en` | Interface language |
| `/reload` | Reload the binary and configuration |
| `/help`, `/menu` | Command reference and picker |
| `/exit`, `/quit` | Leave |

## Tools

| Tool | What it does |
| --- | --- |
| `read` | Read a file (with optional line range) |
| `write` | Create or overwrite a file |
| `edit` | Exact string replacement, with undo support |
| `search` | Regex search via `rg` |
| `bash` | Run a shell command locally or on a configured host |
| `web` | Web search, or fetch a URL as clean Markdown |
| `memory` | Recall or record persistent notes and decisions |
| `skill` | Load a skill's instructions on demand |

Skills are discovered in `~/.spavn/skills` and `~/.claude/skills`; inline `@skill-name` activates one for a single prompt.

## Plan / Code phases

Two orthogonal phases control what the agent is *allowed* to do, independent of access mode and role:

| Phase | Tools | Use for |
| --- | --- | --- |
| 📐 `plan` | `read`, `search`, `web`, `memory`, `skill` | Research, impact analysis, design, review |
| 💻 `code` | everything | Implementation |

The phase is enforced in `execute_tool` (`src/tools.rs`), not in the prompt. In `plan` mode a call to `write`, `edit`, or `bash` returns a structured read-only notice and the file is never touched — the model cannot "decide" to write, the harness refuses. `memory` stays available so a plan can end with a recorded decision.

Switch live with `/plan`, `/code`, or the `\` key (the current phase is always shown in the status bar). Start a scripted read-only run with `spavn -P plan "design the migration"`. The current phase is independent of `/role`: a role shapes *how* the model thinks, the phase controls *what it may do*.

## Permissions and path policy

Every tool call passes through `src/guard.rs`.

| Mode | Behaviour |
| --- | --- |
| `safe` | Prompts for every write and shell command |
| `auto` (default) | Prompts only for destructive operations |
| `sovereign` | No prompts, except for secret files |

Path decisions resolve in this order:

1. Explicit allow (`allowed_paths`, workspace roots) → allow.
2. Secret directories (`~/.ssh`, `~/.aws`, …) → ask, even in `sovereign`.
3. Sensitive home directories → ask.
4. Writes to system directories → deny.
5. Temp directories → allow.
6. Anything else → ask.

Shell commands are classified as `Safe`, `Destructive` (e.g. `rm`, `chmod -R`, `git reset --hard`, `git push --force`, `docker`, `systemctl`), or `Catastrophic` (e.g. `mkfs`, `diskutil erase`, `shutdown`). `Catastrophic` is always blocked. The classifier also looks inside `sh -c`/`bash -c`/`eval`, follows `cd`, and flags `curl | sh` and variable command names. A closed stdin or a bare Enter at the approval prompt declines.

## Configuration

`~/.spavn/config.json` (created on first run, mode `0600`):

```json
{
  "default_model": "local",
  "fallback_cloud_model": "claude-sonnet-4-6",
  "busy_input_mode": "steer",
  "language": "en",
  "local_discovery_url": "http://127.0.0.1:1234/v1/models",
  "local_base_url": "http://127.0.0.1:1234/v1",
  "quota_quick_script": null,
  "quota_full_script": null,
  "memory_recall_bin": null,
  "memory_remember_bin": null,
  "protected_paths": ["~/Documents/important"],
  "allowed_paths": ["~/work"],
  "providers": [
    {
      "id": "work",
      "title": "Work gateway",
      "base_url": "https://gateway.example.com/v1",
      "api_key_env": "WORK_API_KEY",
      "is_local": false,
      "context_window": 128000,
      "models": [
        {
          "key": "work:fast",
          "model_id": "fast-1",
          "label": "Fast",
          "badge": "cloud",
          "desc": "Gateway fast tier"
        }
      ]
    }
  ],
  "hosts": [
    {
      "key": "local",
      "ssh_target": "local",
      "badge": "Local",
      "desc": "Local development machine"
    }
  ]
}
```

`api_key_env` accepts a bare variable name, `env:NAME`, or `${NAME}`. If it is empty, the provider is treated as keyless (local).

Environment variables:

| Variable | Purpose |
| --- | --- |
| `SPAVN_HOME` | State directory (default `~/.spavn`) |
| `SPAVN_MODEL` | Default model key |
| `SPAVN_LOCAL_BASE_URL` | Local OpenAI-compatible endpoint |
| `SPAVN_BASE_URL` | Fallback endpoint |
| `SPAVN_API_KEY` | Fallback API key |
| `ANTHROPIC_API_KEY` | Key for the built-in `anthropic` / `sonnet` / `opus` models |
| `SPAVN_FORCE_ANTHROPIC` | `1` to use the native Anthropic path on a non-`anthropic.com` proxy |
| `SPAVN_SHELL` | Shell used by the `bash` tool |
| `SPAVN_LANG` | `tr` or `en` |

## State and storage

```
~/.spavn/                0700
├── config.json          0600
├── secrets.env          0600   # KEY=value, one per line
├── sessions/            0700   # session-<timestamp>.jsonl, 0600
├── skills/              0700   # <name>.md
├── audit/               0700   # HMAC-chained JSONL
└── ...
```

Sessions are plain JSONL: system prompt, user and assistant turns, tool calls, tool results. Nothing is synced anywhere.

## Security model

- Writes and destructive shell commands require approval outside `sovereign`; the catastrophic tier is never auto-approved.
- Secret directories are contentious even in `sovereign` (`even_sovereign` paths).
- Session files, configuration, and the state directory are created with restrictive permissions and re-hardened on load.
- The audit log chains every action with an HMAC, so tampering is detectable.

Known limitations, tracked in [CHANGELOG.md](CHANGELOG.md):

- Interpreter indirection (`python -c`, `node -e`, `find -exec`, `xargs`) is not resolved by the shell gate.
- Symlink races (TOCTOU) between the policy check and the write are possible.
- Interrupting a turn stops rendering, but the underlying read thread may linger up to 90 seconds.
- No retry or backoff around upstream failures.

## Development

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

CI runs the same on `ubuntu-latest` and `macos-latest` (`.github/workflows/ci.yml`).

Layout:

```
src/
├── main.rs          CLI parsing, agent loop, slash commands
├── hud.rs           ANSI renderer, termios input, pickers
├── router.rs        endpoint resolution, HTTP/SSE streaming
├── http.rs          ureq agents for streaming and one-shot calls
├── tools.rs         tool implementations and approval flow
├── guard.rs         path policy and shell risk classification
├── config.rs        config, providers, API keys, shell detection
├── session.rs       JSONL sessions, 0600/0700 hardening
├── skills.rs        skill discovery and injection
├── memory.rs        recall/remember bridge
└── spavn_engine.rs  audit trail, janitor, goals, wiki, fleet
```

## License

[MIT](LICENSE) — free to use, modify, and contribute. See the [contributing notes](CONTRIBUTING.md).
