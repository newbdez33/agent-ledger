# agent-ledger — TODO

Open work is tracked as GitHub issues; this file is the map. Shipped: **v0.1** (2026-09-06) — spec, CLI, tests, skill.

## Next

- [x] Release v0.1.0 tag — https://github.com/newbdez33/agent-ledger/issues/1
- [x] `account add` idempotent on identical re-run — https://github.com/newbdez33/agent-ledger/issues/2 (from the skill baseline test)
- [x] `reconcile` retries must not append duplicate snapshots — https://github.com/newbdez33/agent-ledger/issues/3 (from the skill baseline test)
- [x] `pnl --marks` values open groups and adds `open_value` / `mtm` — https://github.com/newbdez33/agent-ledger/issues/4 (from poly's first trading day)
- [ ] `group_id` → `group` in every output; export → import keeps groups — https://github.com/newbdez33/agent-ledger/issues/5
- [x] `reconcile` records the snapshot only; `--adjust` posts, `--dry-run` writes nothing — https://github.com/newbdez33/agent-ledger/issues/9 (from poly's live loop)

## Waiting on the poly integration

`newbdez33/poly` will shell out to `ledger` for fills, redeems, fees and balance reconciliation (see its `TODO.md`). Needs it discovers arrive here as issues; expect the first ones once its integration spec is written.

## Not planned

- Double-entry bookkeeping, FX between currencies, valuing open positions, an HTTP or MCP interface. See the Non-goal line in `docs/specs/2026-09-06-agent-ledger-design.md`.
