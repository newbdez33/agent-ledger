# agent-ledger

An append-only, SQLite-backed ledger that AI agents drive from the shell.

Agents that move money have wallets but no books. A wallet says what the balance is; it does not say why it changed, or whether what the agent thinks happened matches what the chain or exchange reports. `ledger` is the book: one binary, one file, JSON in and out, idempotent writes, and an audit trail that no caller can rewrite.

**Status:** v0.1, usable. `make install` builds the binary and links the companion skill. Design: [docs/specs/2026-09-06-agent-ledger-design.md](docs/specs/2026-09-06-agent-ledger-design.md).

## Install

Requires a Rust toolchain.

```sh
git clone https://github.com/newbdez33/agent-ledger && cd agent-ledger
make install          # cargo install + symlink skill/ledger into ~/.claude/skills/ledger
ledger --help
```

To pin a version without cloning (binary only, no skill symlink):

```sh
cargo install --git https://github.com/newbdez33/agent-ledger --tag v0.1.0
```

Every command takes `--json`. The database lives at `--db PATH`, else `$LEDGER_DB`, else `~/.agent-ledger/ledger.db`. Set `LEDGER_ACTOR` so the audit trail says who wrote each row.

## What it looks like

```sh
ledger account add poly-usdc --currency USDC --decimals 6

ledger add poly-usdc 100 --kind deposit --ref 0xabc… --json
ledger add poly-usdc -25.5 --kind trade --ref order-7f3 --group arb:btc-5m:0310 \
    --meta '{"market":"btc-5m-0310","side":"buy","price":"0.51","strategy":"arb"}'
ledger add kalshi-usd -24.0 --kind trade --ref k-991 --group arb:btc-5m:0310
ledger add poly-usdc 50 --kind settlement --ref settle:btc-5m-0310 --group arb:btc-5m:0310
ledger add poly-usdc -1.7 --kind fee --ref order-7f3-fee

ledger balance
ledger history poly-usdc --limit 20
ledger group arb:btc-5m:0310            # both legs, net per currency
ledger pnl poly-usdc --since 2026-09-01 --by meta:strategy
ledger pnl poly-usdc --by group --marks marks.json   # {"arb:btc-5m:0310": "48.00"}: adds open_value, mtm
ledger reconcile poly-usdc --observed 124.70 --source polymarket-onchain
ledger reverse 12 --memo "double counted"
cat backfill.jsonl | ledger import --dry-run
ledger export poly-usdc --format csv
```

## Principles

- **Append-only.** Entries and reconciliation snapshots can never be updated or deleted. Database triggers enforce this, so it holds even against a raw `sqlite3` session. Mistakes are fixed with a `reversal` entry that points back at the original.
- **Exact money.** Amounts are stored as integer minor units with per-account decimals. Inputs with too many decimals are rejected, never rounded. JSON carries amounts as strings.
- **Idempotent for retrying agents.** Pass `--ref` with an order id or tx hash; replaying the same entry returns the original with `"duplicate": true` and exit 0. A replay with a different amount is a hard error.
- **Reconciliation built in.** Tell the ledger the balance you actually observed; it stores a snapshot and posts an adjustment so the book matches reality, and the diff is on record.
- **Multiple accounts, multiple currencies.** One file can hold `poly-usdc`, `kalshi-usd`, `agent-x-eth`; transfers between same-currency accounts are atomic two-leg entries.
- **Positions, not just cash lines.** Tag every leg of a trade or a cross-venue arbitrage with one `--group`; `group <id>` shows the position's net per currency. Attach structured attributes (market, side, price, strategy) as `--meta` JSON and bucket realized PnL by them with `pnl --by meta:strategy`. Capital movements never count as PnL. Hand `pnl --marks` the venue's value of each open group and every row also shows `open_value` and `mtm`; the ledger itself never values a position.
- **Backfill in one shot.** `import` reads JSON Lines from stdin and applies them in one transaction, skipping duplicates and rolling back on the first conflict. `--dry-run` shows what would happen.
- **Machine and human output.** Aligned tables by default, `--json` for agents, exit codes 0 / 1 / 2 for success / usage / domain error.

## Companion skill

The repo ships a Claude Code skill in `skill/ledger/` that tells an agent when to record, which flags to always pass, the sign convention, and a worked Polymarket wallet flow. `make install` builds the binary and symlinks the skill into `~/.claude/skills/ledger`.

## Layout

```
src/lib.rs      ledger core over rusqlite
src/main.rs     clap CLI
skill/ledger/   companion skill (SKILL.md)
docs/specs/     design documents
docs/plans/     implementation plans
```

## Development

This project is spec-driven: a design document in `docs/specs/` is written and reviewed before code, then an implementation plan in `docs/plans/` breaks it into test-first steps.

Rust, stable toolchain. Runtime dependencies are `rusqlite` (bundled SQLite), `clap`, `rust_decimal`, `chrono`, `serde`, `uuid`.

## License

[MIT](LICENSE)
