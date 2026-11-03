# Contributing

Thanks for contributing to spavn. Keep the project's identity: one binary, boring dependencies, zero daemons.

## Setup

```sh
cargo build --release
cargo test
```

## Quality gates (CI must stay green)

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Rules:

- No new runtime dependencies without discussion in the PR description.
- No comments in code unless they explain *why*, not *what*.
- Every behavior change ships with a test where the logic is pure; TUI/HTTP paths get a mock-based integration test.
- Security behavior (guard, sessions, config) changes must not widen what `sovereign` may do without an explicit changelog note.

## Pull request checklist

- [ ] `cargo fmt`, `clippy -D warnings`, `cargo test` pass locally
- [ ] `CHANGELOG.md` updated (what, why, known limits)
- [ ] `README.md` updated if user-facing behavior changed
- [ ] No secrets, tokens, or personal paths committed
