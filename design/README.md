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
  command, *why* it is being asked, and three decisions (Deny / Once / Allow for session).

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
- No horizontal overflow and a visible composer at 1400, 1280, 1100 and 1000 px wide.
- The thread is the only scroll container; `scrollHeight == clientHeight` on the shell itself.
- Icons that carry meaning are inline SVG, not font glyphs, so they never fall back to a
  system symbol on another machine.

## Still missing (do not ship without these)

- Empty state (no session), first-run/onboarding, error state, and a long streaming response.
- Mid-width behaviour between 1000 px and the 1080 px panel breakpoint.
- Keyboard focus rings on every interactive element, and the `⌘K` command palette.
