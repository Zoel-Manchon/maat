# Contributing to Maat

Maat is intentionally small. Contributions should preserve its three design
principles:

1. Modal editing must remain predictable.
2. File-state and integrity behavior must remain explicit.
3. The TUI should stay fast, legible and dependency-conscious.

## Before opening a pull request

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

Add tests for editing commands, cursor invariants, search behavior and file I/O
whenever the change touches those areas.

## Scope

Small, focused pull requests are preferred. For large features such as syntax
highlighting, a rope buffer, plugins or multiple windows, open an issue first
and describe:

- the user workflow;
- the proposed key bindings or commands;
- the effect on the core/UI boundary;
- new dependencies and their cost;
- the tests needed to prevent regressions.

## Commit style

Use clear imperative commits, for example:

```text
feat: add wrapped search navigation
fix: preserve path after failed save-as
refactor: isolate history snapshots
```
