---
name: ledger
description: Use when an agent moves money, records a trade fill, fee, redemption or settlement, checks a wallet or venue balance, needs realized PnL for a period, strategy or arbitrage position, backfills trading history, or has just fetched a live balance from an exchange or chain and the `ledger` CLI is installed
---

# Ledger

`ledger` is an append-only SQLite cash ledger: one binary, one file, exact money, JSON output.
It records cash movements per account and reconciles them against what the venue or chain reports.
It does not value open positions; equity = `ledger balance` + venue-reported position value.
Every command takes `--json` and `--help`; the file is `--db PATH`, else `$LEDGER_DB`, else `~/.agent-ledger/ledger.db`.

## When to use

- Money is about to move or just moved: deposit, withdrawal, fill, redeem, fee. Record it immediately.
- You fetched a live balance from a venue or chain. Reconcile right away.
- You need a balance, what happened, or realized PnL by period, strategy, or position.
- You are backfilling history from venue exports (`import`, JSON Lines, `--dry-run` first).

## Recipe

1. **One account per real venue balance**, decimals explicit. Retry-safe: `account_exists` (exit 2) means it is already there.
   `ledger account add poly-usdc --currency USDC --decimals 6 --json`
2. **Record each movement** with `--json`, a `--ref`, and for positions the same `--group` on every leg plus `--meta`. Every position gets a group, even a one-venue buy and redeem; name it `<strategy>:<market>`.
   `ledger add poly-usdc -25.5 --kind trade --ref ord-7f3 --group arb:btc-0310 --meta '{"strategy":"arb","market":"btc-5m-0310","side":"buy","price":"0.51","shares":"50","venue":"polymarket"}' --ts 2026-09-06T03:10:02Z --json`
3. **Reconcile once per fetched balance.** `reconcile` compares observed with the book **as of `--ts`**, the same number `balance --at <ts>` shows, and posts the difference as an `adjustment` at that time. Without `--ts` it uses now or the latest booked entry, whichever is later, so the whole book counts. That is the right call for a balance you just fetched.
   `ledger reconcile poly-usdc --observed 123.6075 --source polygon-rpc --json`
   A historical statement (month-end, a past snapshot) gets its own `--ts`; a nonzero diff there lands in the past and shifts every later balance, so pass `--no-adjust` to record the check without changing the books.
4. **Read**: `balance [account]`, `history <account>`, `group <id>`, `pnl [account] --by total|day|week|month|group|meta:<key>`, `snapshots <account>`, `export <account> --format csv|json`.

Set `LEDGER_ACTOR=<your agent name>` so the audit trail says who wrote each row.

## Refs make writes idempotent

`--ref` is unique per account. Replaying the same ref with the same amount returns the original with `"duplicate": true`, exit 0. Same ref, different amount is `ref_conflict`, exit 2, nothing written.

| movement | ref |
|---|---|
| deposit, withdrawal, redeem | the tx hash |
| fill | the order or fill id |
| fee on an order | `fee:<order-id>` (the order id itself is taken by the fill) |
| resolution payout | the venue's settlement id or redeem tx; `settle:<market>` when it has none |

## Kinds and signs

Positive is money into the account, negative is money out. Zero is rejected.

| event | kind | sign |
|---|---|---|
| deposit in | `deposit` | + |
| withdrawal, bridge out | `withdrawal` | − |
| buy fill / sell fill | `trade` | − / + |
| redeem, resolution payout, expiry cash, funding | `settlement` | ± |
| venue fee | `fee` | − |
| rebate, airdrop | `other` | ± |
| reconcile difference | `adjustment` (written by `reconcile`) | ± |
| undo of an entry | `reversal` (written by `reverse`) | opposite of the original |

A leg that expires worthless moves no cash: record nothing. The group net already shows the loss.
A fee on a fill takes the fill's `--group`, its strategy `--meta`, and its `--ts`; otherwise strategy PnL overstates and the fee lands in the `null` bucket.
Round to the account's decimals before posting; extra digits are rejected (`precision_exceeded`), never rounded.
Pass `--ts` with the venue's event time (RFC 3339 or `YYYY-MM-DD`); omit it only for something happening now. Future timestamps are accepted without warning.
Negative balances are allowed; the ledger does not know your funding.

## Positions and PnL

- `ledger group <id> --json` lists every leg across accounts with `net` per currency. There is no FX: USDC and USD stay separate; add them yourself if you treat them at par.
- `ledger pnl --by meta:strategy` buckets by a meta key; entries without the key land in the `null` bucket. Conventional keys, all strings: `market`, `side`, `price`, `shares`, `strategy`, `venue`. Extra keys (`outcome`, `fee_type`, ...) are fine.
- `pnl` excludes `deposit`, `withdrawal`, `transfer` automatically; `trade`, `settlement`, `fee`, `adjustment`, `other` count. A `reversal` counts under the kind it reverses, so a reversed adjustment shows as zero in `adjustments`.
- USDC on Polygon to USD at Kalshi is a `withdrawal` plus a `deposit`, optionally sharing a `--group`. `transfer` is only for same-currency accounts.

## Fixing mistakes

`ledger reverse <entry-id>` or `ledger reverse --group <id>` (every entry not yet reversed). Reversals inherit the group, so a reversed position nets to zero. Never open the SQLite file directly; it refuses updates and deletes.

## Reading results

Exit 0 success (including duplicates), 1 usage or IO, 2 domain error. Errors go to stderr as `{"error":{"code","message"}}` (`line` added for `import`). JSON amounts are strings.
Shapes: `add` → `{entry, balance, duplicate}` where `balance` is the account's current balance (on a duplicate too); `balance` → `{accounts:[{account, currency, balance, entries, last_ts, last_reconciled_at}]}` and `balance <account> [--at]` → `{account, currency, balance, at}`; `history` → `{entries:[... balance_after]}`; `group` → `{entries, net}`; `pnl` → `{accounts:[{rows:[{bucket, trades, settlements, fees, adjustments, other, net}]}]}`; `reconcile` → `{snapshot, adjustment|null}`; `snapshots` → oldest first, last row is the latest.
After a replay, compare `entries` counts from `balance` before and after: unchanged means nothing double-counted.

## Common mistakes

- Recording a zero-amount "settlement" for a leg that expired worthless → `zero_amount`. Record nothing.
- Reusing the fill's order id as the fee's `--ref` → `ref_conflict`. Use `fee:<order-id>`.
- Leaving `--decimals` at its default of 2 for a stablecoin account, then failing on `0.8925`. Pass `--decimals 6`.
- Treating `account_exists` on a retry as a failure. It is the account being there.
- Retrying a successful `reconcile`. Each run adds a snapshot row; the second one changes nothing.
- Passing a stale `--ts` for a balance you fetched just now. The book as of that time excludes later entries and the adjustment lands in the past. For a live balance omit `--ts`; for a real historical statement use its time with `--no-adjust`.
- Summing `net` across currencies inside the tool. It never does; you do, explicitly.
