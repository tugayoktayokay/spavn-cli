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

## Release

One tag drives every channel: GitHub archives, crates.io and the Homebrew tap are all read from
the same `v*` tag, so nothing is rebuilt per channel.

```sh
# 1. Bump the version in Cargo.toml and add the CHANGELOG entry, then:
cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test
cargo publish --dry-run          # proves the crate packages and builds standalone

# 2. Tag; the Release workflow builds the matrix and attaches SHA-256 sidecars
git commit -am "chore(release): v0.1.1" && git tag v0.1.1 && git push --tags

# 3. crates.io (needs a token in ~/.cargo/credentials.toml)
cargo publish

# 4. Homebrew: the release workflow does this for you once TAP_GITHUB_TOKEN is set.
#    Without the secret, run it by hand against a tap checkout:
sh packaging/homebrew/sync-tap.sh 0.1.1 /path/to/homebrew-tap
```

`sync-tap.sh` downloads each `*.sha256` sidecar, rewrites the formula and commits it, and exits
non-zero when a hard platform archive is missing — a formula never points at a URL that 404s.
The tap workflow is a no-op (not a failure) when `TAP_GITHUB_TOKEN` is unset.

`cargo publish` must run **after** the tag pushes cleanly, so the crate's `repository` and the
README images point at a release that exists.
