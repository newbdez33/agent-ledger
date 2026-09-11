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
3. **Reconcile every fetched balance.** `reconcile` compares observed with the book **as of `--ts`**, the same number `balance --at <ts>` shows, and records a snapshot carrying the diff. It posts nothing else. Do not pre-compute book or diff to decide anything: run it and read `diff`. In the loop the first run is a real reconcile, so the read is on record even if you adjust later; `--dry-run` computes the same and writes nothing, for looking before you post. In a live loop a nonzero diff is usually fills you have not booked yet: `add` them with their refs, then run the reconcile again with the same `--ts` and `--observed`; the book has changed, so it writes a fresh snapshot, and `diff` `0` is your confirmation. A diff you can name is a missing entry too: book the amount the venue named under its kind, with a ref built from the read that revealed it and `--ts` at the venue's time if it gave one, else at the read; then reconcile again, and a diff still nonzero means the venue's number did not explain it. An on-chain fee with no activity row: `add poly-usdc -0.57 --kind fee --ref fee:polygon-rpc:2026-09-10T10:00:00Z --ts 2026-09-10T09:48:00Z --memo "venue support: gas fee on-chain at 09:48, no activity row; found by the 10:00 read"`. A fee with no order belongs to no position: no group or meta, and it lands in the `null` bucket by design. `--adjust` is for a diff you have confirmed is real but cannot name: it posts an `adjustment` at `--ts` with no ref, group or meta, which lands in the `null` bucket of every report; give it `--memo` with what the venue told you. Its snapshot records the diff it closed, not 0, and `adjustment_entry_id` on that snapshot is the resolved marker; a diff-0 row appears only if you run again. You are done; `--dry-run` shows the 0 without another row.
   `ledger reconcile poly-usdc --observed 123.6075 --source polygon-rpc --ts 2026-09-06T03:12:45Z --json`
   Pass the read time as `--ts` whenever you know it: the comparison is on entry `ts`, not `recorded_at`, so a correction dated before the read counts even if you wrote it afterwards, and the snapshot records when the venue was read. Retry safety: a run that repeats a recorded observation exactly (same `--ts`, observed, source **and book**; `--memo` and `--adjust` do not count) writes nothing and returns it with `"duplicate": true`. On a dry run `duplicate` means an identical snapshot exists, not that the read is resolved; read `diff` for that. `--adjust` with a nonzero diff always posts and is safe to retry: the retry finds diff 0, writes one zero-diff snapshot and posts nothing, and from then on identical retries are duplicates. `snapshots` therefore shows a diff being resolved, two or three rows per read, not one row per fetch. Without `--ts` it uses now, or the latest booked entry if that is later, so the whole book counts; each retry is then a new observation time and a new snapshot, unless the time is pinned to a future-dated entry.
   A historical statement (month-end, a past snapshot) gets its own `--ts` and never `--adjust`: an adjustment there lands in the past and shifts every later balance.
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
| fee found only by reconcile (no activity row) | `fee:<source>:<read ts>` |
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
| reconcile difference | `adjustment` (written by `reconcile --adjust`; `add --kind adjustment` only by hand, with a memo) | ± |
| undo of an entry | `reversal` (written by `reverse`) | opposite of the original |

A leg that expires worthless moves no cash: record nothing. The group net already shows the loss.
Every leg of a position (fill, fee, settlement) carries the same `--group` and the same `strategy` and `market` `--meta`; a fee also takes its fill's `--ts`. A fee with no order takes the venue's time, else the time of the read that found it (step 3), and no group. A leg without them lands in the `null` bucket and the strategy's PnL is wrong.
Round to the account's decimals before posting; extra digits are rejected (`precision_exceeded`), never rounded.
Pass `--ts` with the venue's event time (RFC 3339 or `YYYY-MM-DD`); omit it only for something happening now. Future timestamps are accepted without warning.
Negative balances are allowed; the ledger does not know your funding.

## Positions and PnL

- `ledger group <id> --json` lists every leg across accounts with `net` per currency. There is no FX: USDC and USD stay separate; add them yourself if you treat them at par.
- `ledger pnl --by meta:strategy` buckets by a meta key; entries without the key land in the `null` bucket, printed as `null` in the table. Conventional keys, all strings: `market`, `side`, `price`, `shares`, `strategy`, `venue`; record the ones you have, missing keys are fine. Extra keys (`outcome`, `fee_type`, ...) are fine.
- `pnl` excludes `deposit`, `withdrawal`, `transfer` automatically; `trade`, `settlement`, `fee`, `adjustment`, `other` count. A `reversal` counts under the kind it reverses, so a reversed adjustment shows as zero in `adjustments`.
- `pnl` is cash only, so an open position reads as a loss equal to its cost until you mark it. Write the venue's current value of each open group to a JSON file, amounts as strings with up to the account's decimals, `{"farm:whistler-crompton": "21.60", "dir:laprairie": "4.20"}`, and pass `--marks marks.json`. Every row gains `open_value` (the marks of the groups in that row summed, `null` when none) and `mtm` (`net` + `open_value`); JSON always carries both, the table shows them only with `--marks`. Use it with `--by total`, `group` or `meta:<key>`. A group that exists nowhere is `group_not_found`, a typo. A group outside the report (another account, before `--since`) is skipped silently. Marks are not stored; keep the file next to the balance snapshot it came from.
- One marks file per account. A two-venue arbitrage group has one value per venue in that venue's currency, and a marked group that lands in two rows is `mark_ambiguous`, so write `poly-marks.json` and `kalshi-marks.json` and run `pnl poly-usdc --marks poly-marks.json`, then `pnl kalshi-usd --marks kalshi-marks.json`. The tool never adds USDC `mtm` to USD `mtm`; you do, at par if that is your policy. A position split across `--by day` rows cannot be marked either. The `adjustment` a reconcile posts has no group or meta, so it sits in the `null` bucket: inside `--by total` `mtm`, outside every strategy and group row.
- USDC on Polygon to USD at Kalshi is a `withdrawal` plus a `deposit`, optionally sharing a `--group`. `transfer` is only for same-currency accounts.

## Fixing mistakes

`ledger reverse <entry-id> --memo "<why>"`, or `ledger reverse --group <id> --memo "<why>"` when the whole position was wrong (every entry in the group not yet reversed). Find the id with `history <account> --json` and match `ref`. A reversal copies the original's `ts`, `group` and `meta`, so the mistake nets to zero in every view (`balance --at`, `--by day`, `--by group`, `--by meta:<key>`); `recorded_at` (JSON only, the table has no column for it) says when you corrected it, and `--memo` is the only place the table shows why.
A corrected re-entry keeps the original fill's `--ts`, takes a new ref `<original ref>:v2`, and carries the corrected `--group` and `--meta`; when the strategy changes, the group name changes with it (`arb:btc-close` → `dir:btc-close`). The old group and the old strategy stay as zero rows: that is the audit trail, not a break-even.
```
ledger reverse 3 --memo "desk: p42 was dir, not arb" --json
ledger add poly-usdc -10 --kind trade --ref p42:v2 --group dir:btc-close --meta '{"strategy":"dir","market":"btc-close","side":"buy","price":"0.40","shares":"25","venue":"polymarket"}' --ts 2026-09-06T09:30:00Z --json
```
Dating the re-entry at the correction time instead leaves `balance --at` short by the amount between fill and correction, and a `reconcile --adjust --ts` in that window posts a phantom adjustment. After a re-entry, re-key any marks to the new group: the old group still exists and still accepts a mark, which would show as phantom `open_value`. A reversal cannot be reversed (`cannot_reverse_reversal`); to undo one, add the original again under a new ref. Never open the SQLite file directly; it refuses updates and deletes.

## Reading results

Exit 0 success (including duplicates), 1 usage or IO, 2 domain error. Errors go to stderr as `{"error":{"code","message"}}` (`line` added for `import`). JSON amounts are strings.
Shapes: `add` → `{entry, balance, duplicate}` where `balance` is the account's current balance (on a duplicate too); `show` → `{entry, balance}`, again the current balance, not the entry's running balance; `account add` → `{account, duplicate}`; `reverse` → `{entries:[reversal, …]}`; `balance` → `{accounts:[{account, currency, balance, entries, last_ts, last_reconciled_at}]}` where `last_reconciled_at` is the latest snapshot's as-of `ts`, not when it ran, and `balance <account> [--at]` → `{account, currency, balance, at}`; `history` → `{entries:[... balance_after]}`; `group` → `{entries, net}`; `pnl` → `{accounts:[{rows:[{bucket, trades, settlements, fees, adjustments, other, net, open_value, mtm}]}]}` where `bucket` is `null` both for the no-value bucket and for the single `--by total` row (the table prints `null` and `total`), and `open_value` is `null` (a blank cell in the table) with `mtm` equal to `net` unless `--marks` valued a group in that row; `reconcile` → `{snapshot, adjustment|null, duplicate, dry_run}` where `snapshot.id` is `null` on a dry run; `snapshots` → `{account, snapshots:[{id, account, ts, observed, book, diff, adjustment_entry_id, source}]}` oldest first, last row is the latest, `ts` being the as-of time; nothing records when a reconcile ran.
After a replay, compare `entries` counts from `balance` before and after: unchanged means nothing double-counted.

## Common mistakes

- Recording a zero-amount "settlement" for a leg that expired worthless → `zero_amount`. Record nothing.
- Reusing the fill's order id as the fee's `--ref` → `ref_conflict`. Use `fee:<order-id>`.
- Leaving `--decimals` at its default of 2 for a stablecoin account, then failing on `0.8925`. Pass `--decimals 6`.
- Re-running `account add` with a different currency or decimals. Same name, currency and decimals is a duplicate success; a mismatch is `account_exists`.
- Adjusting away a diff in a live loop. A nonzero diff is usually unbooked fills: `add` them with their refs and reconcile again. A fee you can name is an `add --kind fee`; `--adjust` is for a confirmed diff nobody can name.
- Skipping the re-run after booking late fills because "it would be a duplicate". It is not: the book changed, and the fresh snapshot with `diff` `0` is the record that the read was resolved.
- Retrying a `reconcile` without `--ts`. Each run is normally a new observation time and adds a snapshot; with the read time as `--ts` an unchanged retry is a duplicate.
- Passing a stale `--ts` for a balance you fetched just now. The book as of that time excludes later entries, and an `--adjust` there lands in the past. Use the read time; for a real historical statement use its time without `--adjust`.
- Summing `net` across currencies inside the tool. It never does; you do, explicitly.
- Passing today's date as `--until` for today's PnL. A bare date is 00:00 UTC, so `--until 2026-09-06` excludes the whole day. Today is `--since 2026-09-06` alone; a closed day is `--since 2026-09-05 --until 2026-09-05T23:59:59.999Z`.
- Reading a strategy's `net` as how it is doing while its positions are open. That is cash flow; pass `--marks` and read `mtm`.
