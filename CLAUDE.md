# agent-ledger — working notes for Claude

Append-only SQLite cash ledger for AI agents. One Rust crate, binary `ledger`, plus the companion
Claude Code skill in `skill/ledger/`. Read these first:

- `docs/specs/2026-09-06-agent-ledger-design.md` — the design; every rule the code enforces is stated there
- `docs/plans/2026-09-06-agent-ledger.md` — how v0.1 was built, task by task
- `skill/ledger/SKILL.md` — what agents are told; it must stay true to the CLI's behavior
- `README.md` — install and usage
- `TODO.md` — open work, mapped to GitHub issues (`gh issue list`)

## Conventions

- Spec first: a design change lands in `docs/specs/` before code; multi-task work gets a plan in `docs/plans/`.
  Never create `docs/superpowers/`.
- TDD: write the failing test, then the code. `make test` (fmt check, clippy `-D warnings`, all tests) must pass before every commit.
- Conventional commits (`feat:`, `fix:`, `docs:`, `chore:`).
- The **CLI is the public interface**. `src/lib.rs` is internal structure, not a supported API. Any change to a
  command, flag, JSON shape, error code or exit code updates the spec, the skill, and `tests/cli.rs` in the same commit.
- Money is `i64` minor units with per-account `decimals`; never round, reject excess precision. `entries` and
  `snapshots` are append-only by trigger; do not add UPDATE or DELETE paths, add reversal-style entries instead.
- Skill edits are tested like code: run a fresh subagent with only `SKILL.md` and `--help`, give it a realistic
  bookkeeping task, and fold its confusions back into the skill. The description stays "Use when ..." with
  triggers only, no workflow summary.

## Consumers and requests

- `newbdez33/poly` (Polymarket trader) will shell out to `ledger` for fills, redeems, fees and balance reconciliation;
  its conventions are listed in that repo's `TODO.md`. Requests for ledger changes arrive as GitHub issues here.
- Releasing: bump `Cargo.toml` version, tag `vX.Y.Z`, push; consumers run `make install`.

## Layout

```
src/lib.rs            module declarations, re-exports
src/error.rs          LedgerError, code(), exit_code()
src/money.rs          parse_amount / format_amount (exact)
src/time.rs           storage timestamps (RFC 3339 UTC millis)
src/model.rs          Kind + every serializable result type
src/db.rs             schema v1, migration, pragmas
src/ledger/           Ledger struct; accounts, entries, reports, pnl, reconcile, import
src/cli/              clap args, Output enum, table/CSV rendering
src/main.rs           dispatch, JSON vs table, exit codes
tests/cli.rs          end-to-end tests against the built binary
skill/ledger/         companion skill (symlinked by `make install`)
```
