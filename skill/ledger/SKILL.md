---
name: ledger
description: Use when an agent moves money, records a trade fill, fee, redemption or settlement, checks a wallet or venue balance, needs realized or mark-to-market PnL for a period, strategy or arbitrage position, backfills trading history, or has just fetched a live balance from an exchange or chain and the `ledger` CLI is installed
---

# Ledger

`ledger` is an append-only SQLite cash ledger: one binary, one file, exact money, JSON output.
It records cash movements per account and reconciles them against what the venue or chain reports.
It does not value open positions: fetch each open position's value from the venue yourself, then hand them to `pnl --marks` for mark-to-market; equity is the `ledger balance` figure plus those values, added by you.
Every command takes `--json` and `--help`; the file is `--db PATH`, else `$LEDGER_DB`, else `~/.agent-ledger/ledger.db`.

## When to use

- Money is about to move or just moved: deposit, withdrawal, fill, redeem, fee. Record it immediately.
- You fetched a live balance from a venue or chain. Reconcile right away.
- You need a balance, what happened, or realized PnL by period, strategy, or position.
- You are backfilling history from venue exports (`import`, JSON Lines, `--dry-run` first).

## Recipe

1. **One account per real venue balance**, decimals explicit. Re-running with the same currency and decimals returns `"duplicate": true`, exit 0. A different currency or decimals is `account_exists`, exit 2.
   `ledger account add poly-usdc --currency USDC --decimals 6 --json`
2. **Record each movement** with `--json`, a `--ref`, and for positions the same `--group` on every leg plus `--meta`. Every position gets a group, even a one-venue buy and redeem; name it `<strategy>:<market>`.
   `ledger add poly-usdc -25.5 --kind trade --ref ord-7f3 --group arb:btc-5m-0310 --meta '{"strategy":"arb","market":"btc-5m-0310","side":"buy","price":"0.51","shares":"50","venue":"polymarket"}' --ts 2026-09-06T03:10:02Z --json`
3. **Reconcile once per fetched balance.** `reconcile` compares observed with the book **as of `--ts`**, the same number `balance --at <ts>` shows, and posts the difference as an `adjustment` at that time. Without `--ts` it uses now or the latest booked entry, whichever is later, so the whole book counts. That is the right call for a balance you just fetched. A balance read earlier, at a known time, gets that time as `--ts`; it is the only correct comparison once anything was booked after the read, and the snapshot then records when the venue was read.
   `ledger reconcile poly-usdc --observed 123.6075 --source polygon-rpc --json`
   A historical statement (month-end, a past snapshot) gets its own `--ts`; a nonzero diff there lands in the past and shifts every later balance, so pass `--no-adjust` to record the check without changing the books.
4. **Read**: `balance [account]`, `history <account>`, `group <id>`, `pnl [account] --by total|day|week|month|group|meta:<key> [--marks marks.json]`, `snapshots <account>`, `export <account> --format csv|json`.

Set `LEDGER_ACTOR=<your agent name>` so the audit trail says who wrote each row.

## Refs make writes idempotent

`--ref` is unique per account. Replaying the same ref with the same amount returns the original with `"duplicate": true`, exit 0. Same ref, different amount is `ref_conflict`, exit 2, nothing written. Only kind and amount are compared: a replay with a different `--meta`, `--group` or `--ts` is still a duplicate and changes nothing. Fix wrong metadata with `reverse` and a new entry under a new ref. A reversed entry still owns its ref: replaying it returns that entry with `"duplicate": true` and `reversed_by` set, and writes nothing.

| movement | ref |
|---|---|
| deposit, withdrawal, redeem | the tx hash |
| fill | the order or fill id |
| fee on an order | `fee:<order-id>` (the order id itself is taken by the fill) |
| resolution payout | the venue's settlement id or redeem tx; `settle:<market>` when it has none |
| corrected re-entry of a reversed fill | `<original ref>:v2` (the original ref stays with the reversed entry) |

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
Every leg of a position (fill, fee, settlement) carries the same `--group` and the same `strategy` and `market` `--meta`; a fee also takes its fill's `--ts`. A leg without them lands in the `null` bucket and the strategy's PnL is wrong.
Round to the account's decimals before posting; extra digits are rejected (`precision_exceeded`), never rounded.
Pass `--ts` with the venue's event time (RFC 3339 or `YYYY-MM-DD`); omit it only for something happening now. Future timestamps are accepted without warning.
Negative balances are allowed; the ledger does not know your funding.

## Positions and PnL

- `ledger group <id> --json` lists every leg across accounts with `net` per currency. There is no FX: USDC and USD stay separate; add them yourself if you treat them at par.
- `ledger pnl --by meta:strategy` buckets by a meta key; entries without the key land in the `null` bucket, printed as `null` in the table. Conventional keys, all strings: `market`, `side`, `price`, `shares`, `strategy`, `venue`. Extra keys (`outcome`, `fee_type`, ...) are fine.
- `pnl` excludes `deposit`, `withdrawal`, `transfer` automatically; `trade`, `settlement`, `fee`, `adjustment`, `other` count. A `reversal` counts under the kind it reverses, so a reversed adjustment shows as zero in `adjustments`.
- `pnl` is cash only, so an open position reads as a loss equal to its cost until you mark it. Write the venue's current value of each open group to a JSON file, amounts as strings with up to the account's decimals, `{"farm:whistler-crompton": "21.60", "dir:laprairie": "4.20"}`, and pass `--marks marks.json`. Every row gains `open_value` (the marks of the groups in that row summed, `null` when none) and `mtm` (`net` + `open_value`); JSON always carries both, the table shows them only with `--marks`. Use it with `--by total`, `group` or `meta:<key>`. A group that exists nowhere is `group_not_found`, a typo. A group outside the report (another account, before `--since`) is skipped silently. Marks are not stored; keep the file next to the balance snapshot it came from.
- One marks file per account. A two-venue arbitrage group has one value per venue in that venue's currency, and a marked group that lands in two rows is `mark_ambiguous`, so write `poly-marks.json` and `kalshi-marks.json` and run `pnl poly-usdc --marks poly-marks.json`, then `pnl kalshi-usd --marks kalshi-marks.json`. The tool never adds USDC `mtm` to USD `mtm`; you do, at par if that is your policy. A position split across `--by day` rows cannot be marked either. The `adjustment` a reconcile posts has no group or meta, so it sits in the `null` bucket: inside `--by total` `mtm`, outside every strategy and group row.
- USDC on Polygon to USD at Kalshi is a `withdrawal` plus a `deposit`, optionally sharing a `--group`. `transfer` is only for same-currency accounts.

## Fixing mistakes

`ledger reverse <entry-id>` or `ledger reverse --group <id>` (every entry not yet reversed). A reversal copies the original's `ts`, `group` and `meta`, so the mistake nets to zero in every view (`balance --at`, `--by day`, `--by group`, `--by meta:<key>`); `recorded_at` says when you corrected it. A reversal cannot be reversed (`cannot_reverse_reversal`); to undo one, add the original again under a new ref. Never open the SQLite file directly; it refuses updates and deletes.

## Reading results

Exit 0 success (including duplicates), 1 usage or IO, 2 domain error. Errors go to stderr as `{"error":{"code","message"}}` (`line` added for `import`). JSON amounts are strings.
Shapes: `add` → `{entry, balance, duplicate}` where `balance` is the account's current balance (on a duplicate too); `account add` → `{account, duplicate}`; `balance` → `{accounts:[{account, currency, balance, entries, last_ts, last_reconciled_at}]}` and `balance <account> [--at]` → `{account, currency, balance, at}`; `history` → `{entries:[... balance_after]}`; `group` → `{entries, net}`; `pnl` → `{accounts:[{rows:[{bucket, trades, settlements, fees, adjustments, other, net, open_value, mtm}]}]}` where `open_value` is `null` and `mtm` equals `net` unless `--marks` valued a group in that row; `reconcile` → `{snapshot, adjustment|null}`; `snapshots` → oldest first, last row is the latest.
After a replay, compare `entries` counts from `balance` before and after: unchanged means nothing double-counted.

## Common mistakes

- Recording a zero-amount "settlement" for a leg that expired worthless → `zero_amount`. Record nothing.
- Reusing the fill's order id as the fee's `--ref` → `ref_conflict`. Use `fee:<order-id>`.
- Leaving `--decimals` at its default of 2 for a stablecoin account, then failing on `0.8925`. Pass `--decimals 6`.
- Re-running `account add` with a different currency or decimals. Same name, currency and decimals is a duplicate success; a mismatch is `account_exists`.
- Retrying a successful `reconcile`. Each run adds a snapshot row; the second one changes nothing.
- Passing a stale `--ts` for a balance you fetched just now. The book as of that time excludes later entries and the adjustment lands in the past. For a live balance omit `--ts`; for a real historical statement use its time with `--no-adjust`.
- Summing `net` across currencies inside the tool. It never does; you do, explicitly.
- Passing today's date as `--until` for today's PnL. A bare date is 00:00 UTC, so `--until 2026-09-06` excludes the whole day. Today is `--since 2026-09-06` alone; a closed day is `--since 2026-09-05 --until 2026-09-05T23:59:59.999Z`.
- Reading a strategy's `net` as how it is doing while its positions are open. That is cash flow; pass `--marks` and read `mtm`.
