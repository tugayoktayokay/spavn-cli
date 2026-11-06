# Changelog

## 2026-11-06 — `v0.2.0`: Modular Core, Autonomous Harness Intelligence, JSON-RPC 2.0 Bridge & Stale-File Guard

### Architecture & Desktop Bridge (`src/tools/`, `src/router/`, `src/hud/`, `src/bridge.rs`)
- **Modular Core**: Split monolithic `tools.rs`, `router.rs`, `hud/`, and `main.rs` into focused submodules (`tools/{edit,read,search,shell,mcp_subagent,schema,web}.rs`, `router/{anthropic,openai,vision}.rs`, `turn.rs`).
- **JSON-RPC 2.0 Bridge (`--rpc`, `bridge.rs`, `events.rs`, `approval.rs`)**: Added bidirectional stdio JSON-RPC 2.0 transport, structured `AgentEvent` emissions, and interactive tool approval plumbing for `spavn-desktop` (Tauri) and IDE extensions.
- **Terminal Title & Task Picker (`hud/title.rs`, `hud/pickers.rs`)**: Added terminal window title management (`WorkingGuard`) for live working state and an interactive arrow-key task picker for `/todo done`.

### Autonomous Harness Intelligence (`tools/edit.rs`, `tools/mod.rs`, `turn.rs`)
- **Signature & Arity-Aware Impact Graph**: Word-boundary caller matching (`contains_word_boundary_ident`) and parameter arity analysis (`extract_function_arity`) immediately flag external call sites affected by a function signature change (`[SPAVN Impact Graph — SIGNATURE ARITY SHIFT]`).
- **`FS_STALE_VERSION` Freshness Guard**: Tracks a 64-bit `FNV-1a` content digest (`read_hashes`) and `mtime` for every read or edited file; rejects blind `write` calls and line-range `edit` mutations when a file was modified externally since it was last read.
- **Canonical Argument & Transparent-Chain Loop Breaker (`canonical_tool_call_sig`)**: Normalizes JSON tool arguments independent of key ordering and treats side-channel tools (`memory`, `skill`) as transparent when detecting consecutive or alternating (`A-B-A-B`) tool loops.
- **AST-Aware Context Compaction & Budget-Relative Pruning**: Retains `fn`, `struct`, `enum`, `trait`, `def`, `class`, and `interface` signatures (`extract_compact_ast_skeleton`) when compacting older tool outputs, and scales the prune threshold dynamically with the model's context window (`max_ctx_tokens / 4`).
- **Zero-Dependency Search/Glob & Local-LLM Micro-Schema**: Native Rust directory/content search fallback when `rg` is absent, recursive `grep`/`find`/`cat` shell interceptor, and a dynamic 6-tool micro-schema (`tool_definitions_for_turn`) for local models.

## 2026-11-05 — Update Checker, crates.io & Homebrew Packaging

### Distribution Channels
- **`spavn.dev/install.sh` synchronized**: Updated the live installer script with atomic `mv`, macOS ad-hoc code-signing fix, and `SPAVN_LOCAL_BIN` support; verified `curl -fsSL https://spavn.dev/install.sh | sh` end to end.
- **Homebrew tap (`tugayoktayokay/homebrew-tap`)**: Enabled `brew install tugayoktayokay/tap/spavn`. `packaging/homebrew/sync-tap.sh <version> [tap-path]` downloads SHA-256 sidecars, rewrites the formula, and commits to the tap automatically.
- **Release workflow**: Automatically pushes the updated formula to `homebrew-tap` when `TAP_GITHUB_TOKEN` is configured, and skips cleanly otherwise.
- Verified SHA-256 parity across all distribution channels.

### Update Check (`update.rs`, `hud/mod.rs`, `slash_commands.rs`, `main.rs`)
- **Automatic update notice**: Performs a single background `GET` on startup at `updateCheckIntervalHours` intervals (default 24h) and displays the exact upgrade command when a newer release exists. Never downloads or executes code automatically.
- **In-session notice**: Parks completed background checks in `PENDING_NOTICE` and prints them cleanly on the next prompt redraw without announcing the same version twice (`should_park`).
- **`/update` (`/upgrade`, `/upd`)**: Immediate on-demand version check with install-channel detection (`/opt/homebrew/Cellar/...` → `brew upgrade spavn`, `~/.cargo/bin` → `cargo install spavn --locked --force`, `~/.local/bin` → `install.sh`).
- **Configuration**: `updateCheck`, `updateCheckIntervalHours`, `updateCheckUrl`, and `SPAVN_UPDATE_CHECK=0`, cached in `<spavn_dir>/update-check.json` (`0600`).

### Packaging & Fixes
- **crates.io**: Added `readme`, `homepage`, `documentation`, `keywords`, `categories`, and `rust-version = "1.80"`; excluded `assets/`, `.github/`, and `packaging/` from the crate archive (1.8 MiB → ~90 KiB).
- **Release matrix**: Added `aarch64-unknown-linux-gnu`.
- Removed baked-in search host from the `web` tool so endpoints come strictly from `webSearchUrl` / `SPAVN_WEB_SEARCH_URL`.

## 2026-11-04 — Minimalist TUI Dock, Atomic Image Chips, `/login` Hub, `subagent`, Windows & Local Q4 Optimizations

### Interface & UX (`hud.rs`, `main.rs`, `slash_commands.rs`)
- **Single-Line Dock**: Collapsed top and bottom status bars into a clean single-line dock without duplicate quota or default host clutter.
- **Atomic Image Chips (`[🖼️ file.png (KB)]`) + iTerm2 OSC 1337 Preview**: Dragged image paths collapse into atomic `[🖼️ file.png (KB)]` pills that delete in one `Backspace`/`Delete`, step cleanly with `←`/`→`, and render inline in iTerm2/WezTerm.
- **Live Queue Editing & Cancel (`↑` / `Ctrl+X`)**: Queued and steer messages preview inline above the active input while busy (`LiveSpinner`), recall back into the editor with `↑`, and cancel with `Ctrl+X` / `Delete` / `/qclear`.
- **Flicker-Free (DEC Mode 2026) Boxed Menus**: Interactive menus (`select_interactive_menu`, `interactive_select_model`) render with full box borders (`╭...╮`, `│...│`, `╰...╯`), ANSI/emoji-aware column alignment, and synchronized frame buffering.
- **`/login` · `/provider` · `/connect`**: Automatic CLI token discovery (`gh auth token`, `gcloud auth`), one-key (`o`) browser API key launcher, and local model server setup hub.

### Local LLM & Agent Core (`router.rs`, `tools.rs`, `main.rs`)
- **Local Model Sampling & XML Tool Recovery**: Removed forced `temperature: 0.2` / `max_tokens: 4096` overrides on local models; added `<function=...><parameter=...>` XML tool-call recovery, automatic indentation alignment (`reindent_replacement_lines`) in `edit`, and `preserve_thinking` (`reasoning_content` retention across the last 6 messages).
- **`subagent` Tool, macOS `sandbox-exec`, `--json` / `--version` / `/bench`**: Isolated read-only `subagent` worker, kernel-level macOS `sandbox-exec` (Seatbelt) write lock in `SAFE` mode, `-j`/`--json` output mode, `-V`/`--version`, and `/bench` decode throughput benchmark.
- **Cross-Platform (Windows + Unix)**: Added Win32 Console (`GetConsoleMode`/`SetConsoleMode`/`GetConsoleScreenBufferInfo`), `SetConsoleCtrlHandler`, `taskkill`, and Microsoft/GitHub Models (`models.inference.ai.azure.com`) + Azure OpenAI support.

## 2026-11-03 — Compact Local Prompt, Guard Hardening & Process/Write Resilience

### System Prompt (Local Models)
- `build_system_prompt(cwd, compact)`: Compact rule set when `endpoint.is_local()`, budgeted symbol map (900 chars local / 2000 chars cloud), budgeted `PROFILE.md`, and automatic system prompt rebuild on `/model`, `/local`, and `/byok`.

### Security (`guard.rs`, `tools.rs`)
- Closed/piped stdin (`EOF`) now denies approval prompts instead of implicitly approving; bare `Enter` denies.
- Inspects nested shell invocations (`bash -c`, `sh -c`, `zsh -lc`, `eval`), resolves relative paths across `cd <protected> && rm -rf x`, treats moving protected paths via `mv` as destructive, and requires approval for `curl … | sh`, `base64 -d | bash`, and dynamic command names (`$VAR args`).
- Sends `EXA_API_KEY` via the `x-api-key` header instead of query parameters.

### Resilience & Tests
- Process-group `SIGKILL` on command timeout/interrupt, 4 MiB output cap, atomic file writes (temp file + rename preserving permissions and symlinks), HTTP status retry (`408/425/429/500/502/503/504/529`) honoring `Retry-After` (≤ 30s), and `TermiosGuard` (`Drop`) raw-mode restoration.

## 2026-11-03 — Modular `markdown.rs` + `slash_commands.rs`, Native Anthropic API, PLAN/CODE Phases & Security Guard

### Modularity & Protocol Support
- **`src/markdown.rs` & `src/slash_commands.rs`**: Extracted the Markdown table/stream renderer and slash-command dispatcher out of `hud.rs` and `main.rs`; `/reload` now preserves the active session via `-s <session>`.
- **Native Anthropic Messages API (`/v1/messages`)**: Automatic `api.anthropic.com` detection (`ModelEndpoint::is_anthropic()`), `SPAVN_FORCE_ANTHROPIC=1` override, SSE `content_block_delta` (`text_delta`/`input_json_delta`/`thinking_delta`) streaming, and role-alternating `tool_use` / `tool_result` conversion.
- **PLAN / CODE Phases (`AgentPhase`)**: Tool-layer read-only enforcement in `PLAN` phase (`write`, `edit`, and `bash` return a structured `READ-ONLY PHASE` notice without touching disk), `\` keybinding toggle, `/plan` and `/code` commands, and `-P, --phase plan|code` CLI flag.
- **Path Policy & Shell Gate (`src/guard.rs`)**: Quote/escape/operator-aware shell tokenizer, three-tier `PathPolicy` (`Allow`, `Ask`, `Deny`), `Safe`/`Destructive`/`Catastrophic` shell risk classifier, `0700`/`0600` session and config file permissions, and `ureq` 3 + `rustls` HTTP streaming.
