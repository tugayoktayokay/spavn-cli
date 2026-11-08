# Changelog

## 2026-11-08 — `v0.2.10`: Seamless `/provider` & `/model` Hub, Masked Key Entry, Live `/v1/models` Discovery & Local Port Scanner

### Interactive Menu Live Search & `/provider` Hub (`src/hud/menus.rs`, `src/hud/pickers.rs`, `src/router/mod.rs`, `src/main.rs`)
- **Uninterrupted Live Typing in `select_interactive_menu`**: Removed the single-byte `q`, `k`, and `j` shortcuts when `query` was empty so typing queries starting with `q` (e.g. `qwen`), `k` (`kimi`), or `j` never exits the menu or hijacks navigation. Upgraded input decoding to full UTF-8 (`std::str::from_utf8`) so multi-byte Turkish characters and pasted strings filter immediately.
- **Masked API Key Entry (`prompt_secret_line`)**: Entering or pasting API keys in `/provider`, `/login`, `/byok`, or `/model` now masks characters in real time (`sk-p••••••••••••cdef`) while keeping `'o'` / `'open'` visible for one-keystroke browser portal launch and supporting `Backspace`, `Ctrl+U`, and `Esc`.
- **Inline Key Prompt from `/model`**: Selecting a locked BYOK provider model directly inside `/model` now prompts for and saves its API key on the spot instead of requiring a separate `/byok` run.
- **Automatic Local Server Port Scanning (`probe_local_endpoints`)**: Automatically probes standard local LLM ports (`11434` Ollama, `1234` LM Studio, `8080` llama.cpp / llama-swap / MLX, `8000` vLLM / SGLang) with a sub-millisecond localhost TCP pre-check (`120ms` timeout) and persists discovered endpoints. If no local server is running on standard ports, `/provider` → `Local Server` prompts for a custom local/LAN URL.
- **Live `/v1/models` Discovery (`fetch_remote_models`)**: Custom and built-in providers query `/v1/models` (OpenAIBearer and Anthropic `x-api-key` headers) to auto-discover live server models alongside the curated catalog.
- **Provider-Prefixed Endpoint Routing, Updated 2026 BYOK Catalog & First-Run Persistence**: Updated `builtin_byok_catalog()` and context-window routing for Anthropic (`claude-sonnet-5-5`, `claude-opus-5-5`, `claude-fable-5-1`, `claude-haiku-5-5` — all 1M context), OpenAI (`gpt-6.1-sol`, `gpt-6-astra`, `gpt-6.1-terra`, `gpt-6-luna`, `gpt-5.6-sol`, `gpt-5.5` — 1.05M context, plus `gpt-5.4` / `gpt-5.4-mini`), Google Gemini (`gemini-4-argon`, `gemini-3.8-flash`, `gemini-3.6-flash`, `gemini-3.5-flash-lite` — 1M context, replacing deprecated 2.5 IDs), DeepSeek (`deepseek-v4-pro`, `deepseek-flash` — 1M context), Z.ai GLM (`glm-5.3`, `glm-5.3-flash`, `glm-5.1`), Groq LPU (`openai/gpt-oss-120b`, `qwen/qwen3.8-27b`, `minimaxai/minimax-m2.7`, `llama-3.3-70b-versatile`), and OpenRouter. Selecting a model from a built-in BYOK group emits `<provider_id>:<model_key>` for unambiguous routing, and first-run onboarding persists `cfg.default_model`.

## 2026-11-08 — `v0.2.9`: Claim Gate & Loop Guard, Keyless Web Search Chain, Verbatim Compaction, Real LSP Client & Isolated Worktree Sub-Agents

### Built-in Web Search With Provider Fallback (`src/tools/web_search.rs`, `src/tools/web.rs`, `src/http.rs`, `src/config.rs`)
- **Bug**: `web op=search` refused to run unless the user had set up an MCP search backend. Sessions with no backend never got a single search result.
- **Fix**: Search now goes through an ordered provider chain with automatic fallback. With no API key it uses public keyless search APIs: Parallel's public MCP, Exa's public MCP, then Firecrawl's keyless search. Next come a configured SearXNG instance and the DuckDuckGo, Yahoo and Brave HTML scrapers. Setting `TAVILY_API_KEY` or `BRAVE_API_KEY` puts that API first. `EXA_API_KEY` and `FIRECRAWL_API_KEY` switch those providers to their authenticated APIs. Errors, rate limits and empty results pass the query to the next provider, and the answer lists every provider that failed before it. `webSearchProviders` sets the order (an empty list turns search off). The tool takes `limit` and `recency`. A configured MCP search backend is still tried first.

### Claim Gate & Verification Levels (`src/harness.rs`, `src/turn.rs`, `src/memory.rs`)
- **Bug**: In 19 of 29 logged turns that ended with "düzelttim / kalıcı / çözüldü", nothing was run after the last edit, and any passing compile command counted as proof.
- **Fix**: Each turn records the strongest check run after the last mutation: `None < Compile < Test < Runtime`. A final answer that claims success while the level is below `Test` is sent back once or twice: the model must run a real test/smoke or say plainly what was not verified. If no real check follows, the answer is accepted with a visible `⚠ Doğrulama: … (davranış test edilmedi)` footer. An honest "not tested" disclosure, or a prompt that forbids running commands, is accepted without forcing a run. The legacy verification nudge no longer fires on top of the claim gate.
- **Failing tests stay failing**: when a test command fails in a turn, a later green run only counts as proof if it is the same command or a broader one. For example, `cargo test` clears a failed `cargo test parser`, but `pytest tests/a.py` does not clear a failed `pytest`. Until that happens, no shell call counts above `Compile`, including a passing run after an earlier green one. The gate message and footer name the command that is still failing (`⚠ Hâlâ başarısız / Still failing: …`). This footer appears even when the answer makes no success claim.
- **Loop guard**: identical calls warn at 3 repeats, are blocked (not executed) at 4, and stop the turn at 6. A repeated failing call with the same error counts as identical. Counters for a path reset when that path is edited, so re-reading after an edit is never blocked.

### Compaction Keeps User Messages Verbatim (`src/session.rs`)
- **Bug**: Earlier user messages were cut to 4 lines / 260 chars, so a later "canlıya al" could vanish while an earlier "canlıya alma" survived.
- **Fix**: Every real user message is carried verbatim in a numbered `[User Messages — verbatim, chronological; later messages override earlier ones]` section (16k-char budget; the last 8 messages and anything ≤500 chars are never cut). The section survives repeated compaction without nesting, and both the LLM and the local deterministic paths use it. The LLM summary must include `Constraints & Decisions (latest wins)`.

### Edit Safety & Go Workspaces (`src/tools/edit.rs`, `src/tools/mod.rs`, `src/spavn_engine.rs`)
- **Fix**: A text edit to a file that changed on disk since Spavn last read or wrote it now fails with `Edit error [FS_STALE_VERSION]`; before, it only warned. Spavn's own writes update the tracked version, so consecutive edits still succeed.
- **Fix**: `go.work` repos run `go vet ./...` inside each `use` module (preferring modules with modified files) and never at the workspace root, where it always failed.
- Fuzzy self-heal output now names the changed lines and the similarity score.

### Real Language Server Client (`src/tools/lsp.rs`, `src/tools/schema.rs`, `src/config.rs`)
- `lsp` is now a std-only LSP client (Content-Length JSON-RPC over stdio). It caches one server per workspace and supports `definition`, `references`, `hover`, `symbols`, `diagnostics`, `implementation`, `type_definition` and `workspace_symbol`. Defaults: rust-analyzer, typescript-language-server, pyright, gopls, clangd; override them with `lspServers` in `~/.spavn/config.json`.
- Server stderr is kept and shown when a server fails, including the rustup hint `rustup component add rust-analyzer`. Queries retry while the server is indexing, and URIs are normalized. The old ripgrep heuristic is used only when no server binary exists, and its output is labeled `[heuristic: …]`.

### Parallel, Isolated Sub-Agents (`src/tools/subagent.rs`)
- `subagent` runs up to 8 tasks concurrently. `scout` agents are read-only (24 steps by default); `worker` agents get full tools (40 steps by default, capped at 80) and a 15-minute timeout.
- In git repos each worker runs in its own `git worktree` seeded with the parent's uncommitted state. Its patch is applied with `git apply --check` followed by an atomic `git apply`. A conflicting patch touches nothing: it is saved to `~/.spavn/patches/<id>.patch` and reported. The worktree is always removed afterward.
- Under SAFE access, workers are refused because a background agent cannot ask for approval. Multiple workers are also refused in non-git directories. Model-call errors are reported instead of `(no tool activity recorded)`.

### Spill Retention (`src/spill.rs`, `src/main.rs`)
- **Bug**: `~/.spavn/tool-output/` kept only the newest 40 files across all sessions. One context-shake pass writes a spill per old tool result (52 in one observed turn), so it evicted spills other live sessions had just been handed and its own `recover:` paths — the model's next `read` hit `No such file`.
- **Fix**: Spills now live 7 days (`SPILL_MAX_AGE_MS`); oldest-first eviction only kicks in when the directory exceeds 256 MB (`SPILL_BUDGET_BYTES`), and never removes the newest file.

### Role Dispatch (`src/spavn_engine.rs`)
- **Bug**: Auto role keywords matched anywhere, including inside words and URLs: a SharePoint link to `Test Bulguları.xlsx` routed a comparison request to the TEST role ("write tests"); `latest`/`Authenticate` hit `test`/`auth`.
- **Fix**: A keyword must start a word (Turkish suffixes still match: `incelesene`, `testleri`), and URL tokens are ignored.

### MCP Transport & Truncation (`src/tools/mcp_subagent.rs`, `src/config.rs`, `src/tools/shell.rs`)
- **Fix**: The truncation notice printed its counts swapped (`first 18496 of 16000 chars`); it now reads `first <kept> of <total>`.
- **Fix**: HTTP MCP servers now send configured `headers` from `mcpServers`, parse both JSON and SSE (`text/event-stream`) responses, and report HTTP status errors cleanly instead of dumping raw bodies.
- **Fix**: Public `web op=fetch` calls now prompt only in `SAFE` mode, so read-only research during `PLAN` mode runs uninterrupted in `AUTO` and `SOVEREIGN`.

## 2026-11-07 — `v0.2.7`: Bottom-Pinned Input Dock, Mid-Turn Typing Persistence, Clean `/copy` Regions & Self-Update Fixes

### Bottom-Pinned Input Dock (`src/ansi.rs`, `src/hud/dock.rs`, `src/hud/input.rs`, `src/hud/mod.rs`)
- **Fix**: Eliminated the 3-row blank padding under `╰─` when no `/` or `@` completion popup is active so the idle input dock sits flush on the bottom terminal row just like the busy spinner dock.
- **Unanchored First-Frame Geometry**: `compute_dock_geometry(None, ...)` now moves up by `total - 1` rows from the bottom terminal row (`\x1b[999B` after reserving rows with `\n\n\n`) so the dock's bottom border lands exactly on the last row without scrolling or overwriting the final lines of assistant output. When a suggestion popup closes, the dock automatically re-anchors to the bottom row.

### Mid-Turn Typing Persistence (`src/hud/input.rs`, `src/hud/mod.rs`)
- **Fix**: `LiveSpinner` restarts across tool calls and multi-step LLM turns no longer clear `state.typing_buf` on intermediate `stop()` calls. Text typed while the agent is working remains visible in the input box across every tool transition and preserves trailing spaces until `take_spinner_draft()` hands it to `read_bottom_dock_input` at turn end.
- **Fix**: Removed an `else` branch in the multi-byte escape sequence loop that cleared `typing_buf` on unrecognized `ESC` sequences.

### Clean `/copy` Regions & Copy-Ready Prompt Discipline (`src/hud/mod.rs`, `src/slash_commands.rs`, `src/clipboard.rs`, `src/memory.rs`)
- **Interactive `/copy` Picker**: Section copies strip the `### Heading` line (used only as the menu label), unwrap single fenced code blocks without ```` ``` ```` markers, and strip `> ` blockquote prefixes and outer wrapping quotes (`"..."` / `“...”`) so copied options are immediately paste-ready.
- **System Constitution**: Added Rule #8 instructing models to place copy-ready replies, message drafts, prompts, and snippets inside fenced code blocks (` ```text `).

### Self-Update & Provider Routing Fixes (`src/update.rs`, `src/slash_commands.rs`, `src/hud/menus.rs`, `src/router/mod.rs`)
- **`src/update.rs`**: Stage the extracted binary before cleaning up the temporary archive directory (`base`) and match `"spavn"` first in `find_binary`.
- **`src/slash_commands.rs`**: Reset scroll region before `exec()` on `/update` and return `SlashOutcome::Handled` instead of exiting the TUI loop if `exec()` fails.
- **`src/hud/menus.rs` & `src/router/mod.rs`**: Disambiguate duplicate model keys across providers using `provider_id:key` so selecting a model from a secondary provider routes to the selected provider's `base_url`.

## 2026-11-07 — `v0.2.4`: Busy-Draft Preservation, HUD Polish & Markdown Expansion Fixes

### Busy-Draft Preservation (`src/hud/mod.rs`, `src/hud/input.rs`, `src/main.rs`)
- **Bug**: Text typed into the busy-spinner input while a turn was running was silently discarded when the spinner tore down — the main prompt re-opened with a `None` prefill and hours of mid-flight typing vanished ("şak diye siliniyor").
- **Fix**: New process-global draft slot (`SPINNER_DRAFT: Mutex<Option<String>>` + `take_spinner_draft()`). On spinner teardown, a non-empty `typing_buf` (plus cursor/selection/`is_typing` state) is transferred to the draft slot; the main prompt loop (`main.rs`) now feeds it back as `initial_prefill`. Draft is consume-once and survives every spinner→prompt transition (only a full process exit discards it).
- **Tests** (`hud::draft_tests`): empty-slot → `None`, consume-once semantics, last-write-wins overwrite — serialized behind a test lock since the slot is process-global.

### Markdown Link/Table Rendering Fixes (`src/markdown.rs`)
- **Nested-image links**: `[![img](imgurl)](linkurl)` now render correctly — the link-target scan no longer stops at the first `]` (image's closer) and mis-corrupts nested links (common in contributors/README tables).
- **Balanced bracket scanning** + probe regression: link/table inline expansion edge cases covered by probe-verified table cell rendering.

### Spinner & HUD Polish (`src/hud/input.rs`, `src/hud/mod.rs`, `src/hud/pickers.rs`, `src/session.rs`)
- Removed a provably dead clamp on `tail_start` (invariant already guarantees ≥ 1 via the teardown loop).
- Unified the duplicated `tr_en("Turkish", "English")` phase-label branches in the HUD into a single ternary; dropped the now-unused `is_en` import (`pickers.rs` picked up the tr_en import instead).
- `/model` picker busy-mode label and live-draft behavior kept intact.

### Context Quality (`src/turn.rs`, `src/session.rs`)
- This release rides on the `v0.2.3` model-aware prune budget (local models keep full tool output up to ~50% of window, cloud unchanged at ~25%).

## 2026-11-06 — `v0.2.3`: Local Model Context Retention Fix (Forget-on-Compaction)

### Root Cause — Early Skeletonization on Local Models (`src/turn.rs`)
- **Bug**: `compact_messages_for_model` computed the skeletonization budget as `(max_ctx_tokens / 4).clamp(4_000, 64_000)` for *all* models. For a 128k-window local model that threshold was **32k tokens (25% of the window)** — far too low. Compaction would hide prior tool results (`read`/`edit` output) before the turn anchor, and local models (which degrade badly on truncated context) forgot earlier work within a single session.
- **Fix**: Budget is now model-aware. Local models skeletonize only at ~**50% of the window** (`(max_ctx_tokens / 2).clamp(8_000, 96_000)`), cloud stays at ~**25%** (unchanged). Small-window local models are clamped to a sane 8k–96k range so they never get an oversized budget.

### Regression Tests (`src/turn.rs`)
- `local_prune_budget_keeps_full_tool_output_until_50pct_window`: 160 tool steps (~50k tokens) sit above the old 32k local budget but below the new ~65k one — must stay full (was skeletonizing, now preserved).
- `cloud_prune_budget_stays_at_25pct_window`: 240 tool steps (~74k tokens) past the 64k cloud budget still skeletonize — cloud behavior unchanged.

### Bundled Changes in This Release
- **In-session `/update` (`/upgrade`, `/upd`)**: On-demand version check with install-channel detection (Homebrew / cargo / `install.sh`).
- **Automatic update notice**: Single background release check on startup at `updateCheckIntervalHours` intervals; never downloads or executes code automatically.
- **i18n**: Remaining standalone Turkish strings wrapped in `tr_en()`.
- **Installer**: Fixed prebuilt-asset fetch and source-build fallback in `install.sh`.

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
