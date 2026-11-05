# spavn-cli

**The terminal-native coding agent that ships as one Rust binary.**

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Release](https://img.shields.io/github/v/release/tugayoktayokay/spavn-cli?label=release)](https://github.com/tugayoktayokay/spavn-cli/releases)
[![CI](https://github.com/tugayoktayokay/spavn-cli/actions/workflows/ci.yml/badge.svg)](https://github.com/tugayoktayokay/spavn-cli/actions/workflows/ci.yml)
[![Rust 2021](https://img.shields.io/badge/rust-2021%20edition-orange.svg)](Cargo.toml)
[![Platforms](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey.svg)](#requirements)
[![Dependencies](https://img.shields.io/badge/dependencies-3-success.svg)](Cargo.toml)

`spavn-cli` streams from a local or hosted model, drives real tools against your filesystem and
shell, and keeps every session on disk. No daemon, no database, no telemetry, no async runtime,
no Node/Python sidecar — three crates and the shell you already use.

```sh
$ spavn "audit src/guard.rs, then write the missing regression tests"
```

**This repository is the CLI.** `spavn.dev` hosts a separate, unrelated product — a multiplayer
web platform for teams. Same brand, different codebase, no shared code or releases. Everything
documented here is the local binary in this repo.

---

## Screenshots

Captured from the running TUI, also published on the product page at <https://spavn.dev>. Exact HUD
wording moves between releases, so the prose below states what each frame demonstrates and, where
the current source differs, says so explicitly.

### The dock, live

![SPAVN TUI dock with a queued message, tool cards with a diffstat, and a live context meter](assets/shots/spavn-hero-tui.png)

One turn, start to finish. The user turn carries an image attachment, rendered as the vision badge
`[🖼️ arch-diagram.png (142KB)]`. Three tool cards follow: `read src/main.rs (425..560)` reporting
`135 lines · 1ms`, an `edit` whose title already carries the diffstat (`edit src/main.rs (+48 / -9
lines)`) with a `[Syntax Check: RS Braces Balanced · LSP: 0 errors]` verdict in the body, and a
`bash` run reporting `test result: ok. 78 passed`. The assistant answer streams below them, closed
by the turn metrics (`TTFT 310ms · 2.4s · 84.2 tok/s`).

The dock stays pinned while all of that happens: access mode (`SOVEREIGN`), phase (`CODE`), the
resolved model, a queue slot (`#1: Queued … ↑: Edit · Ctrl+X: Cancel`), a context-budget bar
(`9% (18k/200k)`), and the key hints (`/: Menu · /model · /login · Shift+Tab: Access · \: Phase`).
Typing during a turn never scrolls the dock away — the message either steers the running turn or
queues behind it.

Card shape in the current build (`src/hud/toolcard.rs` documents the same layout):

```text
╭─ ⚡ bash  [local] git status --short
│  M install.sh
│  M src/http.rs
│  … (37 lines hidden)
╰─ ✓ 39 lines · 14ms
```

### `/model` — BYOK cloud plus local workstations

![The /model picker listing a local llama-swap workstation above the cloud BYOK providers](assets/shots/spavn-models-fleet.png)

Local servers are auto-discovered from their `/v1/models` root, so a llama-swap, LM Studio, or Ollama
box appears in the same list as the cloud providers, with its context window and measured decode
speed, rather than being a separate mode you switch into. `/login` configures keys from inside the
picker; the filter row at the top is a live search.

### `/bench` and `/fleet`

![Live model benchmark showing TTFT, decode speed and a tool-calling probe, above the remote host radar](assets/shots/spavn-bench-fleet.png)

`/bench` runs the real harness against an endpoint and prints `TTFT` (VRAM load + prefill), decode
speed as `N tok/s` with produce/total seconds, and a tool-call probe that ends in `✅ PASS (read
JSON)` or `⚠️ NO TOOL CALL` — a model that looks fine on paper fails exactly here. `/fleet` then
sweeps every configured host with `uptime` (over `ssh -o ConnectTimeout=3` for remote targets) and
prints reachability with round-trip latency (`🟢 ONLINE (38ms)` plus the host's uptime line).

---

## Why another coding agent?

| | |
| --- | --- |
| **One binary** | `cargo build --release` produces a single ~4 MB executable. Nothing to install alongside it. |
| **Local-first** | Ollama and LM Studio are built-in providers; any OpenAI-compatible `/v1/models` endpoint you point at becomes a model list. |
| **Guarded, not just prompted** | Permission and phase gates are enforced in the tool layer, not by asking the model nicely. |
| **Boring dependencies** | `serde`, `serde_json`, `ureq` (+ rustls). No tokio, no ratatui, no crossterm, no reqwest. |
| **Everything on disk** | Sessions are JSONL, tool output spills to files, and every AI action lands in an HMAC-chained audit log. |

---

## Requirements

- **macOS** (aarch64, x86_64) or **Linux** (x86_64 prebuilt; aarch64 builds from source and is supported by the installer). The TUI uses `termios`; Windows targets exist in the release matrix but are untested and have no installer.
- **Rust stable**, edition 2021, to build from source.
- **`rg`** on `PATH` for the `search` tool. Remote targets fall back to `grep -rn`, local ones do not.
- Optional: `ssh` for remote host targets, `git` for `/diff` and `/commit`, whatever interpreter your quota and memory helper scripts need.

---

## Install

Four ways in. Pick one.

### 1. One line (macOS, Linux)

```sh
curl -fsSL https://raw.githubusercontent.com/tugayoktayokay/spavn-cli/main/install.sh | sh
```

The one-liner lands in `$HOME/.local/bin`, alongside an `spvn` symlink.

### 2. Cargo

```sh
cargo install --git https://github.com/tugayoktayokay/spavn-cli.git --locked
```

Cargo installs to `~/.cargo/bin`; add that directory to `PATH` if `spavn --version` is not found.

### 3. Prebuilt archive

Download from [Releases](https://github.com/tugayoktayokay/spavn-cli/releases), or fetch directly:

```sh
# macOS Apple Silicon
curl -fsSLO https://github.com/tugayoktayokay/spavn-cli/releases/latest/download/spavn-aarch64-apple-darwin.tar.gz
curl -fsSLO https://github.com/tugayoktayokay/spavn-cli/releases/latest/download/spavn-aarch64-apple-darwin.tar.gz.sha256
shasum -a 256 -c spavn-aarch64-apple-darwin.tar.gz.sha256
tar -xzf spavn-aarch64-apple-darwin.tar.gz
mv spavn "$HOME/.local/bin/spavn" && ln -sf "$HOME/.local/bin/spavn" "$HOME/.local/bin/spvn"
spavn --version
```

The archive contains the bare `spavn` binary — no installer inside it. If you would rather have the
staged-inode-plus-signature path, extract first and hand the file to the installer:
`sh install.sh --local ./spavn` (run from a checkout, not from the archive).

| Platform | Archive |
| --- | --- |
| macOS Apple Silicon | `spavn-aarch64-apple-darwin.tar.gz` |
| macOS Intel | `spavn-x86_64-apple-darwin.tar.gz` |
| Linux x86_64 (glibc) | `spavn-x86_64-unknown-linux-gnu.tar.gz` |
| Windows x86_64 | `spavn-x86_64-pc-windows-msvc.zip` |
| Linux aarch64 | no archive — the installer falls back to `cargo install` |

Each Unix archive ships with a `.sha256` sibling. Windows has no installer script — unzip and run
`spavn.exe`.

The current release is [`v0.1.0`](https://github.com/tugayoktayokay/spavn-cli/releases/tag/v0.1.0). When
an archive exists for your platform the one-liner uses it; otherwise it falls back to
`cargo install`. Both paths end in the same signed, probed binary.

### 4. From source

```sh
git clone https://github.com/tugayoktayokay/spavn-cli.git
cd spavn-cli
cargo build --release
./install.sh --local target/release/spavn
```

### What the installer does

1. Picks the archive for `uname -s` / `uname -m`, or builds from source when no archive exists.
2. Copies the binary onto a **fresh inode** next to the destination (`--local <path>` skips the download).
3. Ad-hoc signs it on macOS (`codesign -s - -f`) and clears every xattr (`xattr -c`).
4. Atomically `mv`s it into `$SPAVN_INSTALL_DIR` (default `$HOME/.local/bin`).
5. Symlinks `spvn` → `spavn`, so the alias can never be a second stale copy.
6. Runs `spavn --version` and **fails loudly** if the new binary cannot exec.

Nothing else is written: your config, sessions, and audit log appear under `~/.spavn` only on
first run.

### PATH, upgrade, uninstall

```sh
# make sure the install dir is reachable
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc && exec zsh
spavn --version            # spavn v0.1.0 (macos/aarch64)

# upgrade: re-run the installer, it stages a new inode and re-signs
curl -fsSL https://raw.githubusercontent.com/tugayoktayokay/spavn-cli/main/install.sh | sh

# uninstall
rm -f ~/.local/bin/spavn ~/.local/bin/spvn
rm -rf ~/.spavn            # also deletes sessions, audit log, and config
```

---

## Quick start

```sh
spavn                           # interactive TUI
spavn "explain src/router.rs"   # single headless turn, then exit
spavn -a safe -P plan           # ask-everything access, read-only plan phase
spavn -r                        # interactive session picker
spavn --help                    # every flag, path, and env var
```

### Against a local model

```sh
# Built-in Ollama provider (http://127.0.0.1:11434/v1) — works with zero config
spavn --model ollama:qwen2.5-coder "refactor the spill module"

# Any other OpenAI-compatible server: point discovery at its /v1 root once
#   LM Studio     http://localhost:1234/v1
#   llama.cpp     http://localhost:8080/v1
#   vLLM          http://localhost:8000/v1
export SPAVN_LOCAL_MODELS_URL=http://localhost:1234/v1
spavn --model local "refactor the spill module"
```

`local` and `local:<model-id>` resolve against `local_discovery_url`; `local_base_url` pins the
base URL when the server does not expose `/v1/models`. No API key is sent and nothing leaves the
machine.

### Against a hosted model

```sh
export OPENAI_API_KEY=sk-...
export ANTHROPIC_API_KEY=sk-ant-...
spavn --model sonnet "write the migration"    # short aliases resolve per provider
spavn --model gpt-5.4 "write the migration"
```

Inside the TUI, `/login`, `/byok`, and `/models` handle keys and base URLs without touching a
shell profile.

### First run: what to do after installing

Nothing has been configured yet, and `spavn` tells you so instead of failing at the first request.

```sh
$ spavn
👋 First-run setup: No active API key or local model configured yet. Opening provider setup wizard (/login)...
```

The wizard (`/login`) writes a provider group into `~/.spavn/config.json` with `0600` and picks it as
the active model. Afterwards:

```sh
$ spavn --version            # spavn v0.1.0 (macos/aarch64)
$ spavn                      # straight into the TUI
```

`~/.spavn` appears on first run only — `config.json`, `sessions/`, `audit/`, `tool-output/`, `pins/`,
all `0700`/`0600`. No files are written into the repository or your `$HOME` root.

| You want to | Do this |
| --- | --- |
| Use a cloud provider | `/login` (or `/byok` for a custom base URL), then `/models` |
| Use a server already running locally | `/models` — local `/v1/models` roots are auto-discovered |
| Check what the binary can do before trusting it | `spavn --help`, then `/help` inside the TUI |
| Run one prompt and leave | `spavn -q "summarize this repo"` |
| Undo an edit the model made | `/undo` |

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

`spavn --help` prints the same reference without opening the TUI. The model default it shows is
whatever `default_model` resolves to on your machine:

```text
SPAVN v0.1.0 (macos/aarch64) — Native Rust Coding Agent & Harness
Usage: spavn [options] [prompt...]

Run without arguments to open the interactive TUI. A prompt is executed
headless, then the TUI opens only when -i/--interactive is given.

Options:
  -m, --model <key>           Select model (default: <your default_model>)
  -a, --access <mode>         Access mode: safe | auto | sovereign
  -P, --phase <phase>         Agent phase: plan | code
  -H, --host <host>           Default target host (default: local)
  -s, --session <id|fork:id>  Resume or fork a specific session
  -c, --continue              Continue latest session
  -r, --resume                Interactive session picker
  -i, --interactive           Start interactive TUI after initial prompt
  -p, --prompt <text>         Explicit prompt (same as a positional argument)
  -t, --think                 Toggle thinking stream display
  -q, --quiet                 Quiet/headless output mode
  -j, --json                  Structured JSON output mode
  -V, --version               Print version and exit
  -h, --help                  Print this help message

Examples:
  spavn                                  Open the interactive TUI
  spavn "explain src/router.rs"           One headless turn, then exit
  spavn -a safe -m local "fix the tests"  Explicit mode, model, prompt
  spavn -c                               Continue the latest session
  spavn -s fork:session-17938104.jsonl    Fork a session and keep history

Paths:
  config    ~/.spavn/config.json      (override the root with SPAVN_HOME)
  sessions  ~/.spavn/sessions/         resumable JSONL, mode 0600
  audit     ~/.spavn/audit/            HMAC-chained action ledger

Environment:
  SPAVN_MODEL, SPAVN_HOME, SPAVN_LANG, SPAVN_SHELL
  SPAVN_LOCAL_MODELS_URL, SPAVN_LOCAL_BASE_URL, SPAVN_FORCE_ANTHROPIC
  OPENAI_API_KEY, ANTHROPIC_API_KEY, GEMINI_API_KEY, OPENROUTER_API_KEY,
  DEEPSEEK_API_KEY, GROQ_API_KEY, XAI_API_KEY, SPAVN_API_KEY, SPAVN_BASE_URL

Files:
  ~/.spavn/PROFILE.md               user profile injected into the prompt
  ~/.spavn/skills/                  on-demand domain instructions
  ~/.spavn/tool-output/             spilled tool output, newest 40 kept
  ~/.spavn/audit.key                HMAC key for the audit chain

Inside the TUI: /help lists every slash command, / lists the menu.
Docs: https://github.com/tugayoktayokay/spavn-cli#readme
```

---

## Model routing

Endpoint resolution is layered; the first hit wins:

1. **Explicit key** — `--model <key>`, `SPAVN_MODEL`, or `default_model` in config.
2. **User providers** — `providers[]` in `~/.spavn/config.json`, including quota-pooled gateways (`quota_quick_script` / `quota_full_script`).
3. **Built-in catalog** — OpenAI, Anthropic, Gemini, OpenRouter, DeepSeek, MiniMax, GLM, Groq, xAI, Microsoft GitHub, OpenCode Zen, and Ollama. Short aliases (`sonnet`, `opus`) resolve inside their provider.
4. **Local discovery** — `GET /v1/models` against `local_discovery_url`; `local` picks the loaded model, `local:<id>` pins one.
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
| `Enter` | Submit. While a turn is running, the line is queued or steered live, per `busy_input_mode`. |
| `Esc` | Cancel the running turn, or close the open menu. |
| `Tab` | Accept the highlighted `/command` or `@file` suggestion; press again to cycle. |
| `Shift+Tab` | Cycle access mode (`safe` → `auto` → `sovereign`). |
| `\` | Toggle phase (`plan` ↔ `code`) — only when the input is empty. |
| `↑` | Recall a queued message for editing. |
| `Ctrl+X` | Drop the newest queued message. |
| `Ctrl+U` | Clear the input. |
| `Ctrl+W` | Delete the previous word (or the selection). |
| `Ctrl+K` | Delete from the cursor to the end of the input. |
| `Ctrl+S` | Open the skill picker. |
| `Ctrl+R` | Reload prompt, config, and skills. |
| `Shift+←` / `Shift+→` | Extend or shrink the selection. |
| `Ctrl+C` | Clear the input; exits when it is already empty. |
| `Ctrl+Q` | Leave the input loop. `Ctrl+D` exits when the buffer is empty. |

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
| `/login`, `/provider`, `/connect`, `/auth` | Provider login: CLI auth, browser flow, or local server. |
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
| `/goal`, `/goals`, `/g` | Goals panel. `/goal <text>` adds one, `/goal done <n>` completes it. |
| `/todo`, `/tasks` | Live task list. `/todo <text>` adds, `/todo done <n>` toggles, `/todo clear` resets. The dock bar also follows `- [ ]` lines in the model's text. |
| `/undo`, `/u` | Restore the last `write`/`edit` snapshot. |
| `review <target>`, `/refactor <target>`, `/security <target>`, `/test <target>`, `/docs <target>` | Switch to that role and dispatch a focused pass over the given target. |
| `/think` | Toggle reasoning display. |
| `/skill`, `/skills` | List, add, show, or remove skills. |
| `/host`, `/tmp` | Target host and scratch paths. |
| `/fleet`, `/f` | Parallel reachability and uptime sweep over every configured host. |
| `/quota` | Provider quota state. |
| `/reload`, `/r`, `/rb` | Live reload of the core prompt, config, and skills. |
| `/lang`, `/language`, `/dil`, `/tr`, `/en` | UI language. |
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
| `lsp` | Symbol navigation (`definition`, `references`, `symbols`) and a generic MCP client (`op: "mcp"`). | MCP servers come from config only. `op: "mcp"`, `server: "<name>"` lists that server's tools; add `tool` + `arguments` to call one. |
| *(MCP)* | Anything an `mcpServers` entry exposes. | Not a builtin: discovered per server, gated like a mutating tool unless the entry sets `readOnly: true`. |

The task bar in the dock is not a tool: it tracks the `- [ ]` checklist items the model writes
into its own text, so progress advances as steps are completed without an extra round trip.

Oversized `bash` output is truncated in the middle for the context window, but the dropped bytes
are spilled to `~/.spavn/tool-output/` so the model can page them back in instead of re-running
the command. Spills are capped at 8 MiB each, `0600`, and the 40 newest are kept.

### MCP servers

There is no builtin desktop-control tool and no hardcoded path to one. Anything with an MCP server
is wired in through `mcpServers` in `~/.spavn/config.json`, and the model reaches it through the
`lsp` tool:

```jsonc
"mcpServers": {
  "desktop": { "command": "~/bin/open-computer-use", "args": ["mcp"] }
}
```

```jsonc
// list what the server exposes
{ "op": "mcp", "server": "desktop" }
// call one of its tools
{ "op": "mcp", "server": "desktop", "tool": "click", "arguments": { "x": 640, "y": 320 } }
```

Every configured server name is listed in the `lsp` tool description, so the model can find them
without a second prompt. Servers not marked `readOnly` pass the mutation gate, and `command` is
config-only — the model can never supply or change it.

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
  "default_model": "local",
  "fallback_cloud_model": "sonnet",
  "language": "en",
  "busy_input_mode": "queue",
  "local_discovery_url": "http://localhost:1234/v1",
  "local_base_url": null,
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
      "protocol": null,
      "context_window": 200000,
      "models": [
        {
          "key": "work/sonnet",
          "model_id": "claude-sonnet-4-6",
          "label": "Sonnet (gateway)",
          "badge": "GW",
          "desc": "Internal gateway",
          "context_window": 200000
        }
      ]
    }
  ],
  "hosts": [
    {
      "key": "buildbox",
      "ssh_target": "build@10.0.0.5",
      "badge": "BB",
      "desc": "Build server",
      "sensitive": true
    }
  ],
  "mcpServers": {
    "memory": { "command": "noospire-memory", "args": ["--stdio"], "readOnly": true },
    "desktop": { "command": "~/bin/open-computer-use", "args": ["mcp"], "readOnly": false },
    "remote": { "url": "https://mcp.internal/sse", "readOnly": false }
  }
}
```

| Field | Meaning |
| --- | --- |
| `default_model` | Endpoint key used when `--model` and `SPAVN_MODEL` are absent. |
| `fallback_cloud_model` | Cloud key used for hybrid escalation: when a **local** model loops (`repeat_count >= 2`) or stops responding, that step is retried on this model. `none` disables it. |
| `language` | `en` or `tr` — the UI language, same as `/lang`. |
| `busy_input_mode` | `queue` (default) or `steer`: what `Enter` does while a turn is running. |
| `local_discovery_url` | OpenAI-compatible `/v1` URL probed for local models. |
| `local_base_url` | Pins the base URL for `local` / `local:<id>` keys. |
| `protected_paths` / `allowed_paths` | Path policy inputs, `~` expanded. |
| `quota_quick_script` / `quota_full_script` | Commands whose output becomes the quota badge / picker pool. |
| `memory_recall_bin` / `memory_remember_bin` | External memory bridges. |
| `providers[].protocol` | `anthropic` to force the Messages API on a proxy that is not on `anthropic.com`. |
| `providers[].is_local` | Marks a provider as local; the router never treats it as cloud. |
| `providers[].api_key_env` | Environment variable holding the key; empty + a quota script means a pooled gateway. |
| `providers[].context_window` | Fallback context budget for models that do not set their own. |
| `hosts[].key` | Alias used by `-H`/`--host` and `bash(host=...)`. |
| `hosts[].ssh_target` | `ssh` destination; `local` means "never remote". |
| `hosts[].sensitive` | Require approval in `auto` mode; remote hosts are sensitive even without the flag. |
| `mcpServers[].command` | Launch command, config-only — the model can never supply or alter it. `~` is expanded. |
| `mcpServers[].args` / `env` | Extra argv and environment for a stdio server; `~` is expanded, `PATH`/`HOME` are inherited. |
| `mcpServers[].url` | Streamable-HTTP server instead of a subprocess. |
| `mcpServers[].readOnly` | Claims no side effects; without it, calls pass the mutation gate. |
| `webSearchServer` / `webSearchTool` | Run `web` search through that `mcpServers` entry. Required unless the HTTP pair below is set. |
| `webFetchTool` | Tool used for page fetch on `webSearchServer`; unset keeps the built-in fetch path. |
| `webSearchUrl` | HTTP MCP endpoint for `web`. Empty or unset means no HTTP backend. |
| `webSearchHttpTool` / `webFetchHttpTool` | Tool names your endpoint serves; both are required to talk to it. |
| `webSearchApiKeyHeader` / `webSearchApiKeyEnv` | Header and env var carrying the endpoint's key. |
| `byokCatalog` | `false` serves the provider list purely from `providers`. |

Profile and rules live beside it in `~/.spavn/PROFILE.md`; skills in `~/.spavn/skills/`.

### Environment variables

| Variable | Effect |
| --- | --- |
| `SPAVN_MODEL` | Default model key. |
| `SPAVN_HOME` | Config/state root instead of `~/.spavn`. |
| `SPAVN_BASE_URL`, `SPAVN_API_KEY` | Fallback endpoint when no provider resolves. |
| `SPAVN_FORCE_ANTHROPIC` | Force the Anthropic Messages protocol for a custom proxy. |
| `SPAVN_LOCAL_MODELS_URL`, `SPAVN_LOCAL_BASE_URL` | Override local discovery / local base URL. |
| `SPAVN_WEB_SEARCH_URL` | HTTP MCP endpoint for `web`, when `webSearchUrl` is unset. |
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
├── secrets.env         BYOK keys (0600); flagged as secret material by the path policy
├── PROFILE.md          user profile injected into the system prompt
├── skills/             on-demand domain instructions
├── sessions/           session-<epoch>.jsonl transcripts, resumable and greppable
├── audit/              HMAC-chained ledger, one <YYYY-MM-DD>.jsonl per day
├── audit.key           key material for that chain
├── pins/               architectural rules pinned with /pin, one <project>.json each
├── goals.json          /goal state
├── artifacts/          <project>.jsonl knowledge summaries recalled into later prompts
├── bg/                 logs from bash(background=true), owner-only
└── tool-output/        spilled oversized tool output
```

Plus `~/.claude/skills/` as a second skill source, so existing Claude skill packs work
unchanged.

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
├── markdown.rs      streaming-safe Markdown renderer for tool output
├── slash_commands.rs  slash dispatcher
├── router.rs        endpoint resolution, SSE streaming, retry policy
├── http.rs          ureq agents, manual redirect handling
├── tools.rs         tool schemas, executors, approval flow
├── guard.rs         path policy, shell risk, egress classification
├── config.rs        config, providers, keys, shell detection
├── session.rs       JSONL sessions, atomic writes, file modes
├── memory.rs        recall/remember bridge
├── skills.rs        skill discovery and injection
├── spill.rs         oversized-output spill files
└── spavn_engine.rs  audit chain, secret scrubbing, janitor, goals, pins, wiki
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
cargo test                    # 203 tests: 199 library + 1 opt-in, 3 binary
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
```

CI (`.github/workflows/ci.yml`) runs `cargo fmt --all --check`, `cargo clippy --all-targets -- -D
warnings`, `cargo test --all-targets`, and `cargo build --release` on `ubuntu-latest` and
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

## Troubleshooting

**`spavn` dies instantly with `[1] killed` (exit 137) on macOS.** Apple Silicon refuses to exec an
arm64 binary without a valid code signature, so `amfid` SIGKILLs it before `main()` runs. This
happens when a binary is replaced *in place* (same inode) and its cached signature no longer
matches — the kernel has no output stream to explain itself, hence the bare `killed`. Reinstall
with `sh install.sh --local target/release/spavn`, which stages a fresh inode, ad-hoc signs it,
and probes `--version` before reporting success. `/reload bin` does the same and refuses to exec a
path that fails the probe.

---

## Known limitations

- Windows is in the release matrix but the TUI is developed and tested on macOS and Linux.
- The `search` tool needs `rg` for full-fidelity regex semantics and to respect `.gitignore`.
- Quota pooling, memory bridges, and the janitor expect the external scripts configured for them; without those they degrade to a no-op rather than guessing.
- Sessions are not encrypted — they are `0600` on disk and contain only what you and the model wrote.

---

## Project scope: two separate products

`spavn` is one brand with two independent codebases. Nothing is shared between them — no code, no
schema, no releases, no CLI flags.

| | `spavn-cli` (this repo) | `spavn.dev` (web platform) |
| --- | --- | --- |
| What it is | Local terminal coding agent | Multiplayer web platform for teams |
| Language / stack | Rust, 2021 edition, no async runtime | TypeScript / Node.js 22, Fastify, Prisma, Redis |
| Binary / artifact | One ~4 MB static executable | Docker Compose deployment |
| Installed by | `install.sh`, Cargo, or a release archive | Hosted service |
| Runs | On the machine you are editing | Server-side, reached over the network |
| State | `~/.spavn` on your machine | Server-side database |
| Account required | No | Yes |

Screenshot gallery, product page, and the web build live at **<https://spavn.dev>**. The Rust
implementation in this repository is the CLI; the web platform is developed separately.

---

## License

`spavn-cli` is released under the **MIT License**. See [LICENSE](LICENSE).

```
MIT License — Copyright (c) 2026 Tugay Oktay Okay
```

MIT was chosen deliberately: this is a tool people run against their own machines and vendor into
their own workflows, so there is no reason to impose copyleft or a contributor licence agreement.
You may use it commercially, fork it, rebrand it, and ship it closed-source. The only obligation is
keeping the copyright notice, and there is no warranty.

Contributions are accepted under the same MIT terms — see [CONTRIBUTING.md](CONTRIBUTING.md).

Third-party crates keep their own licences: `serde` 1.0 and `serde_json` 1.0 are `MIT OR
Apache-2.0`, `ureq` 3.5 is `MIT OR Apache-2.0`, and `rustls` 0.23 is `Apache-2.0 OR ISC OR MIT`.
`cargo deny` is not wired into CI, but the dependency tree is three direct crates deep on purpose.
