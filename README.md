# spavn

**A terminal-native coding agent in one Rust binary.**

`spavn` streams from a local or hosted model, drives real tools against your filesystem and
shell, and keeps every session on disk. No daemon, no database, no telemetry, no async runtime,
no Node/Python sidecar — three crates and the shell you already use.

```
$ spavn "audit src/guard.rs, then write the missing regression tests"
```

---

## Why another coding agent?

| | |
| --- | --- |
| **One binary** | `cargo build --release` produces a single ~3 MB executable. Nothing to install alongside it. |
| **Local-first** | LM Studio, Ollama, llama.cpp, vLLM and anything OpenAI-compatible are first-class endpoints, discovered automatically. |
| **Guarded, not just prompted** | Permission and phase gates are enforced in the tool layer, not by asking the model nicely. |
| **Boring dependencies** | `serde`, `serde_json`, `ureq` (+ rustls). No tokio, no ratatui, no crossterm, no reqwest. |
| **Everything on disk** | Sessions are JSONL, tool output spills to files, and every AI action lands in an HMAC-chained audit log. |

---

## Requirements

- **macOS** (aarch64, x86_64) or **Linux** (x86_64, aarch64). The TUI uses `termios`; Windows builds exist in the release matrix but are untested.
- **Rust stable**, edition 2021, to build from source.
- **`rg`** on `PATH` for the `search` tool (falls back to a built-in scanner).
- Optional: `ssh` for remote host targets, `git` for `/diff` and `/commit`, `node`/`python3` for quota and memory helper scripts.

---

## Install

Prebuilt archives are published by `.github/workflows/release.yml` on every `v*` tag
(`spavn-<target>.tar.gz` + `.sha256`).

```sh
# From the repository (downloads the release archive, falls back to cargo)
curl -fsSL https://raw.githubusercontent.com/tugayoktayokay/spavn-cli/main/install.sh | sh

# Or from source
git clone https://github.com/tugayoktayokay/spavn-cli.git
cd spavn-cli
cargo build --release
./install.sh --local target/release/spavn
```

`install.sh` installs to `$HOME/.local/bin` (override with `SPAVN_INSTALL_DIR`), writes an
`spvn` alias, and stages a fresh inode before signing on macOS so `amfid` never kills a
replaced binary.

---

## Quick start

```sh
spavn                       # interactive TUI
spavn "explain src/router.rs"   # single turn, then exit
spavn -a safe -P plan       # ask-everything access, read-only plan phase
spavn -r                    # interactive session picker
```

### Against a local model

```sh
# LM Studio (default probe: http://localhost:1234/v1)
# Ollama (default probe: http://localhost:11434/v1)
# llama.cpp server, vLLM, text-generation-webui — any OpenAI-compatible /v1
spavn --model local/qwen3-coder "refactor the spill module"
```

Local endpoints are probed through `local_discovery_url` (or `SPAVN_LOCAL_MODELS_URL`); no API
key is required and no request ever leaves the machine.

### Against a hosted model

```sh
export OPENAI_API_KEY=sk-...
export ANTHROPIC_API_KEY=sk-ant-...
spavn --model claude-sonnet-4.6 "write the migration"
```

Inside the TUI, `/login`, `/byok`, and `/models` handle keys and base URLs without touching a
shell profile.

---

## CLI reference

| Flag | Meaning |
| --- | --- |
| `-m`, `--model <key>` | Select model or provider endpoint (default: `default_model` from config, or `SPAVN_MODEL`). |
| `-a`, `--access <mode>` | `safe` \| `auto` \| `sovereign` (aliases: `ask`/`1`, `default`/`2`, `full`/`yolo`/`3`). |
| `-P`, `--phase <phase>` | `plan` (read-only) \| `code` (apply). |
| `-H`, `--host <host>` | Default target host for tools: `local` or a configured SSH alias. |
| `-s`, `--session <id\|file\|latest\|fork:<id>>` | Resume or fork a specific session. |
| `-c`, `--continue` | Continue the newest session. |
| `-r`, `--resume` | Open the interactive session picker. |
| `-i`, `--interactive` | Run the initial prompt, then stay in the TUI. |
| `-p`, `--prompt <text>` | Explicit prompt (same as a positional argument). |
| `-t`, `--think` | Toggle reasoning-token display. |
| `-q`, `--quiet` | Headless output: no TUI, no spinners. |
| `-j`, `--json` | Structured JSON events on stdout (implies `--quiet`). |
| `-V`, `--version` | Version, OS, and architecture. |
| `-h`, `--help` | Usage summary. |

Anything that is not a flag is joined into the prompt, so `spavn fix the bug` works.

---

## Model routing

Endpoint resolution is layered; the first hit wins:

1. **Explicit key** — `--model <key>`, `SPAVN_MODEL`, or `default_model` in config.
2. **User providers** — `providers[]` in `~/.spavn/config.json`, including quota-pooled gateways (`quota_quick_script` / `quota_full_script`).
3. **Built-in catalog** — OpenAI, Anthropic, Gemini, DeepSeek, Groq, xAI, OpenRouter, GLM, MiniMax, plus local probes.
4. **Local discovery** — `GET /v1/models` against every `local_discovery_url`; found models become `local/<id>`.
5. **Fallback** — an env-only endpoint from `SPAVN_BASE_URL` + `SPAVN_API_KEY`.

Two wire protocols are supported:

- **OpenAI chat-completions** (`/chat/completions`, SSE, `tools`) — the default for everything else.
- **Anthropic Messages** (`/v1/messages`, `x-api-key`, `anthropic-version`) — selected automatically for `anthropic.com` and for any base URL when `SPAVN_FORCE_ANTHROPIC=1`.

Streaming is SSE over `ureq` with per-read timeouts (`timeout_per_read`) rather than a total-body
budget, so a slow model never trips the transport while a dead socket still fails fast. `429` and
`5xx` responses carry `[retry-after=N]` back to the loop for classified retry.

---

## Interactive TUI

The dock at the bottom of the terminal shows the provider, phase, access mode, a live task bar
derived from `- [ ]` checklist lines, a context-budget meter, and the current model.

| Key | Action |
| --- | --- |
| `Enter` | Send. While a turn is running, the message is queued as a live steer. |
| `Esc` | Cancel the running turn, or close the open menu. |
| `Tab` | Cycle the input mode. |
| `Shift+Tab` | Cycle access mode (`safe` → `auto` → `sovereign`). |
| `\` | Toggle phase (`plan` ↔ `code`) — only when the input is empty. |
| `↑` | Recall a queued message for editing. |
| `Ctrl+X` | Drop the newest queued message. |
| `Ctrl+U` | Clear the input. |
| `Ctrl+W` | Delete the previous word. |
| `Ctrl+K` | Delete from the cursor to the end of the input. |
| `Shift+←` / `Shift+→` | Extend or shrink the selection. |
| `Ctrl+C`, `Ctrl+Q` | Leave the input loop. `Ctrl+D` exits when the buffer is empty. |

Rendered tool output uses diff cards: the header carries `path (+added -removed)`, the body is a
block-coloured hunk with a per-block line-number gutter, and elisions collapse to `⋯`.

---

## Slash commands

| Command | Purpose |
| --- | --- |
| `/`, `/menu` | Arrow-key main menu. |
| `/model`, `/m`, `/local` | Two-column provider (←/→) and model (↑/↓) picker. |
| `/models` | List every resolvable endpoint. |
| `/bench [model]` | TTFT, tokens/s, and a tool-call round trip against the active or named model. |
| `/byok`, `/b`, `/keys` | Manage API keys and custom base URLs. |
| `/login`, `/provider`, `/connect` | Provider login: CLI auth, browser flow, or local server. |
| `/access`, `/a` | Change access mode. |
| `/plan`, `/code`, `/act` | Switch phase. |
| `/role` | Expertise role: Auto, Coder, Reviewer, Security, Test, Docs, Refactor. |
| `/queue`, `/qlist`, `/qedit`, `/qpop`, `/qclear`, `/qcancel` | Inspect and edit the steer queue. |
| `/resume`, `/sessions`, `/new`, `/fork` | Session picker, fresh session, branch a session. |
| `/recap` | Summary of the last step, plus an idle-triggered recap. |
| `/compact`, `/c` | Compact the transcript against the context budget. |
| `/diff`, `/d` | Git diff, optionally filtered by path. |
| `/commit`, `/cm` | Review changes and commit with a Conventional Commit message. |
| `/blame <file>`, `/audit` | File change history and the HMAC-chained action trail. |
| `/janitor`, `/j` | Run the project quality gates (check, types, lint, test). |
| `/wiki`, `/w` | Instant module and architecture map of the repository. |
| `/pin <note>`, `/pins`, `/unpin <id>` | Persist architectural rules into the project context. |
| `/goal <text>`, `/goals`, `/goal done <n>` | Track project goals. |
| `/todo`, `/tasks` | Live task list; `- [ ]` lines in the model's text drive the progress bar. |
| `/undo`, `/u` | Restore the last `write`/`edit` snapshot. |
| `/review`, `/refactor`, `/security`, `/test`, `/docs` | Prompt shortcuts for a focused pass. |
| `/think`, `/t` | Toggle reasoning display. |
| `/skill`, `/skills` | List, add, show, or remove skills. |
| `/host`, `/tmp` | Target host and scratch paths. |
| `/quota` | Provider quota state. |
| `/reload`, `/r`, `/rb` | Live reload of the core prompt, config, and skills. |
| `/lang`, `/dil`, `/tr`, `/en` | UI language. |
| `/clear` | Clear the transcript (the session file keeps its history). |
| `/exit`, `/quit`, `/x` | Leave the TUI. |

---

## Tools

The model drives these through native function calling. Every call passes the same guard chain,
and every mutation is snapshotted for `/undo`.

| Tool | What it does | Notes |
| --- | --- | --- |
| `read` | Read a file, or disjoint ranges of it (`path:12-40,90-120`). | Never truncates mid-line. |
| `edit` | Replace `old_text` with `new_text`, or apply several non-overlapping edits atomically. | Requires surrounding context to be unique. |
| `write` | Create or overwrite a file. | Temp file + rename; a crash never leaves a half-written target. |
| `bash` | Run a shell command locally or on a configured SSH host. | Non-interactive; risk-classified; timeout- and signal-aware. |
| `search` | Regex content search (ripgrep when present). | Skips `target/`, `node_modules/`, `.git/`. |
| `glob` | List files by pattern. | Skips the same directories. |
| `memory` | Recall or record project and user decisions. | Backed by `~/.spavn` or an external recall binary. |
| `skill` | Load domain instructions on demand. | `~/.spavn/skills`, `~/.claude/skills`. |
| `web` | Search or fetch a URL as Markdown. | Egress-classified; redirects re-validated per hop. |
| `subagent` | Delegate isolated read-only investigations with their own context. | One task, or up to four in parallel. |
| `lsp` | Symbol navigation (`definition`, `references`, `symbols`) and a generic MCP client. | MCP servers come from config only. |

The task bar in the dock is not a tool: it tracks the `- [ ]` checklist items the model writes
into its own text, so progress advances as steps are completed without an extra round trip.

Oversized `bash` output is truncated in the middle for the context window, but the dropped bytes
are spilled to `~/.spavn/tool-output/` (bounded size, pruned) so the model can page them back in
instead of re-running the command.

### Parallel tool calls

`read`, `search`, `glob`, `skill`, and `web` are declared parallel-safe. When a model emits several
of them in one turn, they run concurrently — each in its own `ToolContext`, each stamped with a
distinct step number so the code-shifting guard still means what it says. Any write, `bash`,
`subagent`, or MCP call acts as a barrier: everything before it completes, everything after it
waits.

---

## Plan / Code phases

| Phase | Enforced behaviour |
| --- | --- |
| 📐 **PLAN** | `write`, `edit`, and `bash` are rejected at the tool layer regardless of access mode. The model can only inspect. |
| 💻 **CODE** | All tools available, still subject to access mode and path policy. |

Switch with `/plan`, `/code`, an empty-buffer `\`, or `--phase`. The gate lives next to the tool
executor, so a prompt injection cannot talk its way out of the read-only phase.

---

## Permissions and path policy

**Access modes**

| Mode | Behaviour |
| --- | --- |
| 🔒 `safe` | Every tool call asks for confirmation. |
| ⚡ `auto` | Read-only calls run freely; mutations and non-trivial shell commands ask. |
| 🔥 `sovereign` | Full autonomy for ordinary work — destructive shell commands and secret files still ask. |

**Shell risk classification.** Commands are tokenised (quotes, pipes, subshells, redirections,
`sudo`, `rm -rf`, `git push --force`, `dd`, `mkfs`, transfer tools) into `None`,
`Destructive(reason)`, or `Catastrophic(reason)`. `Catastrophic` prompts even in `sovereign`.

**Path policy.** `protected_paths` and `allowed_paths` in config are resolved and matched before a
tool touches a file. Credential-shaped targets (`.ssh`, `.aws`, `.env`, keychains) are treated as
`sensitive` and require explicit approval. Remote hosts are `sensitive` by default; an unknown
non-local alias is never silently trusted.

---

## Security model

- **Egress gate.** `web`, MCP URLs, and any outbound request are classified before a socket opens: loopback, private, link-local, unique-local, and cloud-metadata addresses are refused for public-internet calls. The classifier also unwraps wildcard-DNS forms (`127.0.0.1.nip.io`, `sslip.io`, `xip.io`), decimal/hex/octal IPv4 spellings, and IPv4-mapped IPv6 (`::ffff:127.0.0.1`).
- **Redirect guards.** Redirects are followed manually, and each hop is reclassified — a public URL cannot pivot into `169.254.169.254`.
- **DNS pinning.** A custom resolver validates the address it actually dials, closing the resolve-then-rebind window.
- **MCP containment.** `command` is config-only and never model-reachable; tools on servers not marked `readOnly` pass the mutation gate; child processes get a minimized environment; responses are size-capped; approval state is cleared between turns.
- **Secrets stay out of context.** API keys are read from the environment or `~/.spavn/config.json`; nothing is written into a session file.
- **File modes.** `~/.spavn` is `0700`; sessions, audit log, and spills are `0600`.
- **Audit trail.** Every tool call and file mutation is appended to an HMAC-chained log, so tampering with an earlier entry invalidates the chain. Inspect it with `/audit` and `/blame`.

---

## Configuration

`~/.spavn/config.json` (override the root with `SPAVN_HOME`):

```json
{
  "default_model": "local/qwen3-coder",
  "lang": "en",
  "local_discovery_url": "http://localhost:1234/v1",
  "local_base_url": null,
  "shell": null,
  "protected_paths": ["~/Documents/important"],
  "allowed_paths": ["~/work"],
  "quota_quick_script": null,
  "quota_full_script": null,
  "memory_recall_bin": null,
  "memory_remember_bin": null,
  "providers": [
    {
      "id": "work",
      "title": "Work gateway",
      "base_url": "https://gateway.internal/v1",
      "api_key_env": "WORK_API_KEY",
      "is_local": false,
      "models": [
        {
          "key": "work/sonnet",
          "model_id": "claude-sonnet-4.6",
          "label": "Sonnet (gateway)",
          "badge": "GW",
          "desc": "Internal gateway",
          "context_window": 200000
        }
      ]
    }
  ],
  "hosts": [
    { "alias": "buildbox", "ssh_target": "build@10.0.0.5", "workdir": "/srv/app" }
  ],
  "mcpServers": {
    "memory": { "command": "noospire-memory", "args": ["--stdio"], "readOnly": true },
    "remote": { "url": "https://mcp.internal/sse", "readOnly": false }
  }
}
```

| Field | Meaning |
| --- | --- |
| `default_model` | Endpoint key used when `--model` and `SPAVN_MODEL` are absent. |
| `lang` | `en` or `tr`. |
| `local_discovery_url` | OpenAI-compatible `/v1` URL probed for local models. |
| `local_base_url` | Forces one base URL for `local/*` keys. |
| `shell` | Shell used by the `bash` tool (defaults to `$SHELL`, then `sh`). |
| `protected_paths` / `allowed_paths` | Path policy inputs, `~` expanded. |
| `quota_quick_script` / `quota_full_script` | Commands whose output becomes the quota badge / picker pool. |
| `memory_recall_bin` / `memory_remember_bin` | External memory bridges. |
| `providers[].is_local` | Marks a provider as never leaving the machine, skipping approval for reads. |
| `providers[].api_key_env` | Environment variable holding the key; empty + a quota script means a pooled gateway. |
| `hosts[].ssh_target` | `ssh` destination; `local` means "never remote". |
| `mcpServers[].command` | Launch command, config-only — the model can never supply or alter it. |
| `mcpServers[].readOnly` | Claims no side effects; without it, calls pass the mutation gate. |

Profile and rules live beside it in `~/.spavn/PROFILE.md`; skills in `~/.spavn/skills/`.

### Environment variables

| Variable | Effect |
| --- | --- |
| `SPAVN_MODEL` | Default model key. |
| `SPAVN_HOME` | Config/state root instead of `~/.spavn`. |
| `SPAVN_BASE_URL`, `SPAVN_API_KEY` | Fallback endpoint when no provider resolves. |
| `SPAVN_FORCE_ANTHROPIC` | Force the Anthropic Messages protocol for a custom proxy. |
| `SPAVN_LOCAL_MODELS_URL`, `SPAVN_LOCAL_BASE_URL` | Override local discovery / local base URL. |
| `SPAVN_SHELL` | Shell for the `bash` tool. |
| `SPAVN_LANG` | `en` or `tr`. |
| `SPAVN_INSTALL_DIR` | Installer target directory. |
| `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, `GROQ_API_KEY`, `DEEPSEEK_API_KEY`, `XAI_API_KEY`, `OPENROUTER_API_KEY` | Provider credentials. |

---

## State and storage

Everything lives under `~/.spavn` (or `SPAVN_HOME`), mode `0700`:

```
~/.spavn/
├── config.json         providers, paths, MCP servers
├── PROFILE.md          user profile injected into the system prompt
├── skills/             on-demand domain instructions
├── sessions/           JSONL transcripts, resumable and greppable
├── tool-output/        spilled oversized tool output
└── audit.log           HMAC-chained action trail
```

A session file is append-only JSONL: one message or tool event per line. That makes a session
`tail`-able, `rg`-able, forkable (`-s fork:<id>`), and safe to keep around — no binary blobs, no
lock files, no migration step.

---

## Architecture

```
src/
├── main.rs          CLI parsing, agent loop, tool-call batching, diff cards
├── lib.rs           module wiring and shared types
├── ansi.rs          pure text/ANSI geometry (width math, dock layout) — no I/O
├── state.rs         shared signal store (typing buffer, tasks, queue)
├── hud/
│   ├── mod.rs       renderer, termios, scroll region, dock
│   ├── input.rs     raw key loop, bracketed paste, steering
│   ├── keys.rs      navigation key decoding
│   ├── menus.rs     pickers and menus
│   ├── toolcard.rs  tool cards, diffstat, numbered diff body
│   └── edit.rs      inline line editor
├── router.rs        endpoint resolution, SSE streaming, retry policy
├── http.rs          ureq agents, manual redirect handling
├── tools.rs         tool schemas, executors, approval flow
├── guard.rs         path policy, shell risk, egress classification
├── config.rs        config, providers, keys, shell detection
├── session.rs       JSONL sessions, atomic writes, file modes
├── memory.rs        recall/remember bridge
├── skills.rs        skill discovery and injection
├── spill.rs         oversized-output spill files
└── spavn_engine.rs  audit chain, janitor, goals, wiki, fleet
```

Two invariants keep the code honest:

- **`ansi.rs` is pure.** Every width, truncation, and dock-geometry calculation is a total
  function with no terminal access, so the layout is unit-testable — which is exactly where a
  TUI normally rots.
- **The locks live in one place.** `state.rs` owns all shared mutable state; the tool executor,
  spinner, and slash dispatcher reach it only through free functions, so locking discipline is
  not re-spelled at each call site.

---

## Development

```sh
cargo build --release
cargo test                # 190 unit tests + doc tests
cargo clippy --all-targets
```

CI (`.github/workflows/ci.yml`) runs build, tests, and clippy on `ubuntu-latest` and
`macos-latest`. `.github/workflows/release.yml` builds `aarch64-apple-darwin`,
`x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`, and `x86_64-pc-windows-msvc` archives with
SHA-256 sidecars on every `v*` tag.

Design constraints for contributions:

- No new runtime dependencies without a strong reason; no async runtime.
- Every gate (access, phase, path, egress, MCP) is enforced in code, never only in the prompt.
- New behavior needs a test that fails without the fix.
- No secrets, tokens, or personal absolute paths in the tree.

See [CONTRIBUTING.md](CONTRIBUTING.md) for the full checklist.

---

## Known limitations

- Windows is in the release matrix but the TUI is developed and tested on macOS and Linux.
- The `search` tool needs `rg` for full-fidelity regex semantics and to respect `.gitignore`.
- Quota pooling, memory bridges, and the janitor expect the external scripts configured for them; without those they degrade to a no-op rather than guessing.
- Sessions are not encrypted — they are `0600` on disk and contain only what you and the model wrote.

---

## License

[MIT](LICENSE) — use it, fork it, ship it. Contributions welcome under the same license.
