# Desktop app — design source

`app-shell.html` is the reference layout for the SPAVN desktop app. It is a **static design
surface**: real HTML/CSS, no build step, no framework, no network requests. Open it in any
browser to review the shell; the same markup ships inside the app's webview.

## What it commits to

- **Layered neutral surfaces.** One tinted canvas, one quiet sidebar, one raised card. Depth
  comes from three surface tokens and hairline borders, not from glow or gradients.
- **One accent colour.** Used only for the active rail item, the primary action, focus and the
  active plan step. Semantic colour (green/amber/red) appears only where it carries meaning:
  exit status, approval state, diff additions and deletions.
- **Mono is metadata, sans is prose.** Paths, durations, counts and keys are monospaced.
  Sentences are not. This is what separates an app from a terminal emulator.
- **The approval card is the product.** A guarded command is not a `[y/N]` line: it shows the
  command, *why* it is being asked, and the decisions the runtime actually implements —
  `y` runs it once, `N` denies, `s` switches to sovereign. The card must not advertise options
  the gate in `src/tools.rs` cannot honour.

## States

The shell renders four conversation states, selected by the URL hash so each can be reviewed
in isolation instead of clicked through in a scripted scenario:

| Hash | State |
| :--- | :--- |
| `#dark&state=conv` | a finished turn, including an approval card and a diff |
| `#dark&state=streaming` | a response mid-flight: caret, stop button, interrupt hint |
| `#dark&state=empty` | first run: no session, no context, three starting points |
| `#dark&state=error` | a failed deploy with the real cause and three recoveries |

`light` replaces `dark`, and `&side=off` hides the right panel. The hash always reflects what
is on screen, so any state can be linked to or captured in CI.

The empty state also suppresses what does not exist yet: session name, branch, dirty count and
the context meter. Reporting `0 files changed` on first run is a lie, not a detail.

## Tokens

Both themes declare the identical token set, so nothing is hardcoded per theme:

| Group | Tokens |
| :--- | :--- |
| Surfaces | `--bg` `--surface` `--elevated` `--sunken` |
| Lines | `--line` `--line-2` |
| Text | `--fg` `--fg-2` `--fg-3` |
| Accent | `--accent` `--accent-2` `--accent-w` `--accent-ink` |
| Status | `--ok` `--warn` `--bad` (+ `-w` washes) |
| Elevation | `--shadow-1` `--shadow-2` `--shadow-3` |
| Space / radius | `--s-1…--s-6`, `--r-sm/md/lg/xl` |

`--accent-ink` is the text colour to use **on** `--accent`: dark ink in dark mode, white in
light mode.

## Verified, not assumed

- All body text ≥ **4.5:1** against every surface it sits on, in both themes
  (`--fg` 15.0:1 dark / 17.2:1 light; `--fg-3` 5.6:1 / 5.0:1).
- Primary button label: **6.56:1** dark, **5.16:1** light.
- All **4 states × 2 themes** render with exactly one pane in flow, a visible composer, the
  correct send/stop swap, and no horizontal overflow.
- No horizontal overflow at 1400, 1180, 1000 and 760 px wide; the right panel collapses at
  1180 and the rail at 760.
- The thread is the only scroll container; `scrollHeight == clientHeight` on the shell itself.
- Icons that carry meaning are inline SVG, not font glyphs, so they never fall back to a
  system symbol on another machine.

## Known limits

- Below **640 px** the diff body is wider than its column by design (`.diff` scrolls on the x
  axis). A desktop window should not offer that width; treat 760 px as the floor.
- Text at 10.5–11 px is legible but tight on low-density (non-Retina) displays.
- The palette lists the slash commands that exist in `src/slash_commands.rs`. Keep it in sync
  when a command is added or removed.
