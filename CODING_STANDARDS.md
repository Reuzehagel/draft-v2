# Coding standards

Read at review, not implementation. Every rule here is a judgement call: `cargo fmt --check` and `cargo clippy --all-targets -D warnings` already run in `.githooks/pre-commit`, so anything they enforce is out of scope. `CLAUDE.md`'s "Facts that bite" and `src/pill/CLAUDE.md` are binding too; this file holds what only a reviewer reading the diff can judge.

## Prefactors

A change described as "no behaviour change" preserves everything observable, the log included: each `tracing` line keeps its fields, timings above all (`elapsed_ms` is how latency gets diagnosed from a user's log). A line may be reworded or merged; a field may not quietly drop out.

## Tests

- A test names the source's constant (`llm::MODEL`), never a retyped literal, so a model retirement or a rename breaks only the tests about it.
- A test drives its module through that module's own seams — the fakes pattern of `pill::adapter`'s `PillPort` and `settings_ui::state`'s `Store`. Assertions about another module's wire format (a request body's shape) belong in that module's tests.
- A test never touches the user's real files under `%APPDATA%\Draft` or `%LOCALAPPDATA%\Draft`.

## Comments

An ordering invariant ("history before paste, do not reorder") is stated once, at the code that enforces it. Code that moves takes its invariant comment along rather than leaving a copy behind.

## Naming

Types, tests and log messages use `CONTEXT.md`'s terms. A new concept a diff introduces either matches a glossary entry or comes with one.
