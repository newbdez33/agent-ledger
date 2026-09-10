# agent-ledger — Design

**Goal:** An append-only, SQLite-backed ledger that AI agents drive from the shell. It records balance movements (deposits, withdrawals, trades, fees, settlements) across multiple accounts and currencies, links the legs of a position so realized PnL is one query, reconciles book balances against observed balances, and keeps a complete audit trail that no caller can rewrite.

**Non-goal:** Double-entry bookkeeping with a chart of accounts, multi-user authentication, a network service or HTTP API, valuing open positions (the caller may hand `pnl` its own marks; the ledger never computes one), FX conversion between currencies, automatic ingestion from exchanges or chains (the caller fetches and posts), and reporting beyond balance, history, realized PnL, and export.

## Context

Agents that move money — trading bots, agents paying for APIs, agents holding wallets — have wallets but no books. A wallet answers "what is the balance now"; it does not answer "why did it change" or "does what we think happened match what the chain says".

Existing options do not fit:

- TigerBeetle and Formance Ledger are rigorous double-entry engines, but each runs as a service the operator has to deploy and keep alive.
- Agent wallet toolkits (Coinbase AgentKit, Ledger Agent Stack, Skyfire) move money and enforce spend limits; they do not keep books.
- Plain-text accounting (hledger, beancount) is agent-readable, but it is double-entry and has no structured write path or idempotency for a retrying agent.

The immediate consumers are a Polymarket short-window trading bot, whose only wallet record today is a Redis snapshot of `{usdc, fetched_at}` plus strategy-level PnL, and a Polymarket↔Kalshi arbitrage monitor. Deposits, withdrawals, redeems, and fees are not recorded anywhere.

### What quant and arbitrage use demands

- **A position is several cash movements over time.** A buy fill, maybe a fee, a settlement. An arbitrage position spans two venues, two accounts, and two currencies (USDC on Polymarket, USD on Kalshi). The ledger must link these legs so the realized result of one position is one query. Hence a caller-supplied `group`.
- **Entries need structured attributes.** Market, side, price, shares, strategy. An analyst must be able to filter on them later without parsing memos. Hence a `meta` JSON column.
- **Realized PnL is the primary question.** Over a period, per strategy, per position. Capital movements (deposits, withdrawals, transfers) must be excluded from it automatically. Hence a `pnl` report.
- **History has to be backfilled** from on-chain and exchange data, atomically and idempotently. Hence `import`.
- **Losing positions settle to zero cash** and therefore produce no entry. The group's net is the loss; nothing extra is recorded.
- **The ledger tracks cash only.** Open-position value comes from the venue. Equity is ledger balance plus venue-reported position value, computed by the caller. The caller can hand those values to `pnl --marks`, so mark-to-market per position or strategy is one command instead of a join outside the tool.

## Users and interface

- **AI agents** (Claude Code and similar) shell out to the `ledger` binary with `--json` and parse the result. A companion skill teaches them when and how.
- **Automated processes** (e.g. a trader daemon) call the same binary to record fills.
- **Humans** read `ledger balance`, `ledger history`, and `ledger pnl` as aligned text tables.

## Architecture

One Rust crate, `agent-ledger`, producing one binary, `ledger`.

```
src/lib.rs        Ledger core over rusqlite: open/migrate, accounts, entries,
                  transfers, reversals, balances, history, groups, pnl,
                  reconcile, snapshots, import, export. No CLI concerns;
                  every operation is one SQLite transaction.
src/main.rs       clap CLI: parse args, call the core, render a table or JSON,
                  map errors to exit codes.
skill/ledger/     SKILL.md companion skill for agents.
Makefile          install: cargo install --path . and symlink skill/ledger
                  into ~/.claude/skills/ledger.
docs/specs/       Design documents (this file).
docs/plans/       Implementation plans.
```

Dependencies: `rusqlite` (bundled SQLite), `clap` (derive, env), `rust_decimal`, `chrono`, `serde`/`serde_json`, `thiserror`, `uuid` (transfer group ids), `dirs` (home directory). Dev: `assert_cmd`, `predicates`, `tempfile`.

### Storage

- One SQLite file. WAL journal mode, `busy_timeout` 5000 ms, `foreign_keys = ON` per connection. Multiple agents and processes may write concurrently; every write is a `BEGIN IMMEDIATE` transaction.
- Path resolution: `--db <path>`, else `LEDGER_DB` environment variable, else `~/.agent-ledger/ledger.db`. The directory is created if missing. There is no `init` command: opening the file creates the schema and applies migrations. `meta.schema_version` starts at 1.

### Money and time

- Amounts are stored as `INTEGER` minor units. Each account declares `decimals` (USDC 6, USD 2, BTC 8). Input is parsed with `rust_decimal`; an input with more fractional digits than the account allows is rejected, never rounded. Sums in SQL are exact integer sums. Output renders as a decimal string with exactly `decimals` fraction digits, and JSON carries amounts as strings, never numbers.
- Sign convention: positive is an inflow to the account, negative is an outflow. Zero is rejected. Negative balances are allowed; nothing enforces a floor, because margin venues legitimately go negative.
- Currency codes are stored uppercase. Account names are unique case-insensitively and looked up case-insensitively.
- Timestamps are `TEXT` in RFC 3339 UTC with millisecond precision, e.g. `2026-09-06T03:12:45.000Z`. Fixed width keeps lexical order equal to chronological order. `ts` is when the movement happened (caller-supplied or now); `recorded_at` is when the row was written (always now). A caller-supplied `--ts` may carry any offset; it is normalized to UTC milliseconds before storage.

### Schema

```sql
CREATE TABLE meta (
  key    TEXT PRIMARY KEY,
  value  TEXT NOT NULL
);

CREATE TABLE accounts (
  id          INTEGER PRIMARY KEY,
  name        TEXT NOT NULL UNIQUE COLLATE NOCASE,
  currency    TEXT NOT NULL,
  decimals    INTEGER NOT NULL CHECK (decimals BETWEEN 0 AND 18),
  note        TEXT,
  created_at  TEXT NOT NULL
);

CREATE TABLE entries (
  id           INTEGER PRIMARY KEY,
  account_id   INTEGER NOT NULL REFERENCES accounts(id),
  ts           TEXT NOT NULL,
  recorded_at  TEXT NOT NULL,
  kind         TEXT NOT NULL CHECK (kind IN
                 ('deposit','withdrawal','trade','settlement','fee',
                  'transfer','adjustment','reversal','other')),
  amount       INTEGER NOT NULL CHECK (amount <> 0),
  ref          TEXT,
  memo         TEXT,
  actor        TEXT,
  group_id     TEXT,
  meta         TEXT CHECK (meta IS NULL OR json_type(meta) = 'object'),
  reverses_id  INTEGER REFERENCES entries(id)
);
CREATE UNIQUE INDEX entries_account_ref ON entries(account_id, ref)   WHERE ref IS NOT NULL;
CREATE UNIQUE INDEX entries_reverses    ON entries(reverses_id)       WHERE reverses_id IS NOT NULL;
CREATE INDEX        entries_account_ts  ON entries(account_id, ts, id);
CREATE INDEX        entries_group       ON entries(group_id)          WHERE group_id IS NOT NULL;

CREATE TRIGGER entries_no_update BEFORE UPDATE ON entries
  BEGIN SELECT RAISE(ABORT, 'ledger entries are append-only'); END;
CREATE TRIGGER entries_no_delete BEFORE DELETE ON entries
  BEGIN SELECT RAISE(ABORT, 'ledger entries are append-only'); END;

CREATE TABLE snapshots (
  id                   INTEGER PRIMARY KEY,
  account_id           INTEGER NOT NULL REFERENCES accounts(id),
  ts                   TEXT NOT NULL,
  observed             INTEGER NOT NULL,
  book                 INTEGER NOT NULL,
  diff                 INTEGER NOT NULL,          -- observed - book
  adjustment_entry_id  INTEGER REFERENCES entries(id),
  source               TEXT
);
CREATE TRIGGER snapshots_no_update BEFORE UPDATE ON snapshots
  BEGIN SELECT RAISE(ABORT, 'ledger snapshots are append-only'); END;
CREATE TRIGGER snapshots_no_delete BEFORE DELETE ON snapshots
  BEGIN SELECT RAISE(ABORT, 'ledger snapshots are append-only'); END;
```

Append-only is enforced by the database itself, so it holds even when someone opens the file with `sqlite3` directly. Accounts have no delete or edit command; the foreign keys make deleting an account with entries impossible anyway. `meta` is validated as a JSON object by the CHECK constraint, so `json_extract(meta, '$.strategy')` always works in ad-hoc queries.

### Rules

**Kinds and signs.** `deposit` must be positive. `withdrawal` and `fee` must be negative. `trade` (fills: buy negative, sell positive), `settlement` (cash posted when a position resolves, expires, is redeemed, or receives funding), `adjustment`, and `other` (rebates, airdrops, anything else) may carry either sign. `transfer` and `reversal` are written only by their own commands, never by `add` or `import`.

**Idempotent `add` with `--ref`.** When `--ref` is given and `(account, ref)` already exists:

- same `kind` and `amount` → return the existing entry with `"duplicate": true`, exit 0. A retrying agent gets a success.
- different `kind` or `amount` → error `ref_conflict`, exit 2. Nothing is written.

Entries without `--ref` are never deduplicated.

**Idempotent `account add`.** When an account of that name already exists (case-insensitive):

- same `currency` and `decimals` → return the existing account with `"duplicate": true`, exit 0. `--note` is ignored; the stored row is not changed.
- different `currency` or `decimals` → `account_exists`, exit 2.

**Group.** `--group <id>` on `add`, `transfer`, and `import` is a caller-chosen, non-empty string that links the legs of one position across accounts, e.g. `arb:btc-5m:2026-09-06T03:10Z`. `transfer` without `--group` generates a UUID v4 so its two legs are always linked. A reversal inherits the group of the entry it reverses, so a fully reversed group nets to zero. `group <id>` shows every entry in the group across all accounts with a per-currency net; there is no FX, so nets are never summed across currencies.

**Meta.** `--meta <json>` must be a JSON object; anything else is `invalid_meta`. It is stored verbatim and returned parsed in JSON output. The skill fixes the conventional keys for trading: `market`, `side`, `price`, `shares`, `strategy`, `venue`.

**Transfer.** `transfer <from> <to> <amount>` requires a positive amount and two accounts with the same `currency`. It writes two `transfer` entries in one transaction: `-amount` on `from`, `+amount` on `to`, sharing a `group_id`. With `--ref`, both legs carry the ref. On replay, if both legs already exist with matching amounts the call returns them with `"duplicate": true`; if only one leg exists, or an amount differs, it is `ref_conflict` and nothing is written. Moving value between currencies (USDC on Polygon to USD at Kalshi) is not a transfer; it is a `withdrawal` on one account and a `deposit` on the other, optionally sharing a `--group`.

**Reversal.** `reverse <entry-id>` writes a `reversal` entry on the same account with `amount = -original.amount`, `reverses_id = original.id`, and the original's `ts`, `group_id` and `meta`; `recorded_at` is the moment of the reversal. The mistake therefore nets to zero in every view (`balance --at`, `history`, `pnl` by day, group or meta key) while the audit trail keeps when it was corrected. If the original is a `transfer` leg, its sibling leg is reversed in the same transaction, because a transfer is one movement. `reverse --group <id>` reverses every entry in the group that is neither a reversal nor already reversed, in one transaction; `nothing_to_reverse` if there are none. Rejected when a target is itself a `reversal` (`cannot_reverse_reversal`) or already has a reversal (`already_reversed`, guaranteed by the unique index). To undo a reversal, add the entry again.

**Reconcile.** `reconcile <account> --observed <amount> [--source <s>] [--ts <t>] [--adjust] [--dry-run]` runs in one transaction. `t` defaults to now or the account's latest entry `ts`, whichever is later, so an implicit reconcile always compares against the whole book even when venue timestamps run ahead of this machine's clock; an explicit `--ts` is the only way to reconcile historically. Then: `book = SUM(amount) WHERE ts <= t`, `diff = observed - book`, insert a snapshot at `t` carrying `diff`. Nothing else is written by default: in a live loop a nonzero diff is usually activity the caller has not booked yet, and an automatic adjustment would absorb it and then compound once the real entries arrive. With `--adjust` and `diff != 0`, insert an `adjustment` entry with `amount = diff`, `ts = t`, memo `reconcile: observed <o>, book <b>`, and store its id in `snapshot.adjustment_entry_id`; the adjustment is inserted before the snapshot row because snapshots are append-only, and the book as of `t` then equals the observed balance. With `--dry-run` nothing is written: the result carries the computed `ts`, `observed`, `book` and `diff` with `snapshot.id = null` and `dry_run: true`; `--dry-run` and `--adjust` together are a usage error.

A snapshot is the fact "source `s` observed `o` at `t` against book `b`", and the same fact is never stored twice: a call that would post no adjustment and whose `(ts, observed, book, source)` equals an existing snapshot of the account writes nothing and returns that snapshot with `duplicate: true` (its adjustment, if it has one). A call with `--adjust` and a nonzero diff is never a duplicate, so observing first and adjusting later at the same `--ts` works. Because `book` is part of the identity, a replay after an adjustment, or after a back-dated entry, records a new snapshot. A dry run reports `duplicate` the same way. `snapshots <account>` lists past snapshots.

**Balance.** `balance` lists every account with its current balance. `balance <account> [--at <ts>]` sums entries with `ts <= at`. Both use `ts`, not `recorded_at`.

**History.** Entries of one account ordered by `(ts, id)` with a running balance computed by a window function (`SUM(amount) OVER (ORDER BY ts, id)`), never stored. `--since`/`--until` filter on `ts`, `--kind` filters on kind, `--group` filters on group, `--limit N` (default 50, `0` for unlimited) keeps the most recent N of the filtered set and prints them oldest first. The running balance is computed over the whole account before filtering, so it is always the true balance after that entry.

**PnL.** `pnl [<account>] [--since] [--until] [--by total|day|week|month|group|meta:<key>]` reports realized cash PnL. Capital movements are excluded: `deposit`, `withdrawal`, `transfer`, and reversals of those. Every other entry counts, and a `reversal` counts under the kind of the entry it reverses. Columns: `trades`, `settlements`, `fees`, `adjustments`, `other`, `net`. Without `--by` there is one row per account; with `--by`, one row per bucket, where `day`/`week`/`month` bucket on `ts` (`%Y-%m-%d`, `%Y-W%W`, `%Y-%m`), `group` buckets on `group_id`, and `meta:<key>` buckets on `json_extract(meta, '$.<key>')`. Entries without the bucket value fall in a `null` bucket, which the table prints as `null`; under `--by total` the single row prints as `total`. Without `<account>` every account is reported, each in its own currency.

With `--marks <file>`, a JSON object of group id to amount string (`{"farm:whistler": "21.60"}`), every row also carries `open_value`, the sum of the marks of the groups whose counted entries fall in that row, and `mtm`, `net` plus `open_value`. Marks are parsed with the account's decimals, never rounded; zero and negative values are allowed. A row with no marked group reports `open_value: null` and `mtm` equal to `net`, and without `--marks` every row does, so the JSON shape is fixed. A mark whose group has no entries anywhere is `group_not_found`, a typo. A mark whose group exists but has no counted entry in the report, because it sits on another account or outside `--since`/`--until`, is ignored, so a file may also hold marks for groups on other accounts. A marked group whose counted entries fall in more than one row (two accounts, two `meta` values, or split across `day`/`week`/`month` buckets) is `mark_ambiguous` and nothing is reported. Marks are never stored.

**Import.** `import [--dry-run]` reads JSON Lines from stdin, one entry per line:

```json
{"account":"poly-usdc","amount":"-25.500000","kind":"trade","ref":"order-7f3",
 "ts":"2026-09-06T03:10:02Z","group":"arb:btc-5m:03:10","memo":"BTC 5m up",
 "meta":{"market":"btc-5m-0310","side":"buy","price":"0.51","shares":"50"}}
```

`account`, `amount`, `kind` are required; `amount` must be a JSON string (JSON numbers are floats and are rejected as `invalid_amount`). `ref`, `ts`, `group`, `memo`, `meta`, `actor` are optional and mean what the `add` flags mean. Every line is validated, then all lines are applied in one transaction with the same rules as `add`, including ref idempotency. A duplicate is skipped and counted; any error rolls back the whole batch and reports the line number. `--dry-run` runs the transaction and rolls it back, reporting what would have happened. Only `add` kinds are importable; transfers are not.

**Actor.** `--actor <name>` overrides the `LEDGER_ACTOR` environment variable; when neither is set, `actor` is null. It records who wrote the row (`claude`, `poly-trader`, `backfill`), not who owns the money.

### CLI

```
ledger [--db <path>] [--json] [--actor <name>] <command>

  account add <name> --currency <code> [--decimals <n>] [--note <text>]
  account list
  add <account> <amount> --kind <deposit|withdrawal|trade|settlement|fee|adjustment|other>
      [--ref <id>] [--memo <text>] [--ts <rfc3339>] [--group <id>] [--meta <json>]
  transfer <from> <to> <amount> [--ref <id>] [--memo <text>] [--ts <rfc3339>]
      [--group <id>] [--meta <json>]
  reverse (<entry-id> | --group <id>) [--memo <text>]
  balance [<account>] [--at <rfc3339>]
  history <account> [--limit <n>] [--since <rfc3339>] [--until <rfc3339>]
      [--kind <kind>] [--group <id>]
  group <id>
  pnl [<account>] [--since <rfc3339>] [--until <rfc3339>] [--by total|day|week|month|group|meta:<key>]
      [--marks <file>]
  reconcile <account> --observed <amount> [--source <text>] [--ts <rfc3339>] [--adjust] [--dry-run]
  snapshots <account> [--limit <n>]
  import [--dry-run]
  show <entry-id>
  export <account> --format <csv|json>
```

- `<amount>` is a decimal with an optional leading `-`. An unsigned amount is positive. The positional accepts negative numbers (`clap` `allow_negative_numbers`).
- `--decimals` defaults to 2. Agents creating a stablecoin account pass `--decimals 6` explicitly; the skill says so.
- `account list` shows accounts (id, name, currency, decimals, note); `balance` shows money. They do not overlap.
- `export` writes every entry of the account with running balance to stdout; CSV columns are `id,ts,recorded_at,kind,amount,balance_after,ref,memo,actor,group_id,meta,reverses_id,reversed_by`, with `meta` as the raw JSON string.

### Output

Default output is an aligned text table for humans. With `--json`, stdout carries exactly one JSON object per command. Amounts are decimal strings; timestamps are RFC 3339 strings; absent values are `null`; `meta` is a parsed object or `null`.

The entry object, used everywhere an entry appears:

```json
{
  "id": 12, "account": "poly-usdc", "currency": "USDC",
  "ts": "2026-09-06T03:12:45.000Z", "recorded_at": "2026-09-06T03:12:45.117Z",
  "kind": "trade", "amount": "-25.500000",
  "ref": "order-7f3", "memo": "BTC 5m up", "actor": "claude",
  "group_id": "arb:btc-5m:03:10",
  "meta": {"market": "btc-5m-0310", "side": "buy", "price": "0.51", "shares": "50"},
  "reverses_id": null, "reversed_by": null
}
```

`reversed_by` is derived from the reversal index, not stored.

| command | shape |
|---|---|
| `add` | `{entry, balance, duplicate}` |
| `show` | `{entry, balance}` |
| `transfer` | `{entries: [from_leg, to_leg], duplicate}` |
| `reverse` | `{entries: [reversal, …]}` |
| `account add` | `{account: {id, name, currency, decimals, note, created_at}, duplicate}` |
| `account list` | `{accounts: [account, …]}` |
| `balance` | `{accounts: [{account, currency, balance, entries, last_ts, last_reconciled_at}, …]}` |
| `balance <a>` | `{account, currency, balance, at}` |
| `history`, `export --format json` | `{account, currency, entries: [entry + balance_after, …]}` |
| `group` | `{group, entries: [entry, …], net: {"USDC": "3.000000", "USD": "-52.00"}}` |
| `pnl` | `{accounts: [{account, currency, rows: [{bucket, trades, settlements, fees, adjustments, other, net, open_value, mtm}, …]}, …]}`; `open_value` is `null` and `mtm` equals `net` unless `--marks` valued a group in the row |
| `reconcile` | `{snapshot, adjustment, duplicate, dry_run}` with `adjustment` an entry or `null`; `snapshot.id` is `null` on a dry run |
| `snapshots` | `{account, snapshots: [{id, ts, observed, book, diff, adjustment_entry_id, source}, …]}` |
| `import` | `{imported, duplicates, dry_run, entries: [entry, …]}` |

### Errors and exit codes

Errors go to stderr. In `--json` mode stderr carries one object and stdout stays empty:

```json
{ "error": { "code": "precision_exceeded",
             "message": "amount 1.2345678 has 7 decimals; account poly-usdc allows 6" } }
```

Import errors add `"line": <n>` to the object.

| exit | meaning |
|---|---|
| 0 | success, including idempotent duplicates |
| 1 | usage error, I/O error, database error |
| 2 | domain error, one of the codes below |

Domain error codes (exit 2): `account_not_found`, `account_exists`, `invalid_account_name`, `invalid_currency`, `same_account`, `entry_not_found`, `group_not_found`, `currency_mismatch`, `precision_exceeded`, `invalid_amount`, `zero_amount`, `invalid_sign`, `invalid_kind`, `invalid_timestamp`, `invalid_group`, `invalid_meta`, `invalid_bucket`, `mark_ambiguous`, `invalid_json`, `ref_conflict`, `already_reversed`, `cannot_reverse_reversal`, `nothing_to_reverse`. Infrastructure codes (exit 1): `database_error`, `io_error`.

### Companion skill

`skill/ledger/SKILL.md` is a Claude Code skill installed by `make install` as a symlink at `~/.claude/skills/ledger`. It contains:

- **When to use:** any time the agent moves money, records a fill, fee or settlement, checks a balance, wants realized PnL, or has just fetched a live balance from an exchange or chain.
- **Conventions:** always pass `--json`; always pass `--ref` when an external id exists (order id, tx hash) and build a deterministic synthetic ref (`settle:<market>`) when the venue gives none; follow the sign convention; round fees to the account's decimals before posting because the ledger rejects extra precision; one ledger account per real venue balance so reconcile stays meaningful, with strategy attribution in `meta.strategy`; tag every leg of a position with the same `--group`; USDC↔USD moves are withdrawal plus deposit, not transfer; run `reconcile` right after fetching a live balance; never open the SQLite file directly; fix mistakes with `reverse`, never by editing.
- **Meta keys:** `market`, `side`, `price`, `shares`, `strategy`, `venue`, all as strings.
- **Cheatsheet:** the command table above.
- **Worked flows:** a Polymarket wallet from `account add poly-usdc --currency USDC --decimals 6` through a deposit with its tx hash, a `trade` buy, a `settlement` on resolution, a `fee`, and a `reconcile` against the on-chain balance; and a two-venue arbitrage position with both legs under one `--group`, followed by `group <id>` to read the result.

The skill is authored with the `writing-skills` skill during implementation so its frontmatter and structure are valid.

### Testing

Library tests run against a temporary database file:

- sums: balance equals the signed sum of entries; `--at` respects `ts`.
- idempotency: same ref, same kind and amount returns duplicate; different amount returns `ref_conflict` and writes nothing. `account add` with the same name, currency and decimals returns duplicate; a different currency or decimals is `account_exists`.
- reversal: negates, links, inherits ts, group and meta so day and meta buckets net to zero; second reversal rejected; reversing a reversal rejected; reversing one transfer leg reverses the sibling; `--group` reverses only unreversed non-reversal entries and errors when nothing is left.
- append-only: `UPDATE` and `DELETE` on `entries` and `snapshots` fail with the trigger message.
- transfer: atomic two-leg write; currency mismatch rejected; nothing written on failure; generated group id when none given.
- reconcile: snapshot stored with the diff and no entry by default; `--adjust` posts an adjustment equal to diff, zero diff posts none; `--dry-run` reports the diff and writes nothing; a repeat of the same ts, observed, book and source that posts nothing is a duplicate, observe then `--adjust` at the same ts is not, a changed book is not; `--ts` computes book as of that time.
- precision: 7 decimals on a 6-decimal account rejected; 6 accepted exactly.
- history: running balance correct under `--limit`, `--since`, `--group` filters; `--limit 0` unlimited.
- group: cross-account entries, per-currency nets, fully reversed group nets to zero.
- pnl: deposits/withdrawals/transfers excluded; reversal folds into original kind; bucketing by day, group, and meta key; null bucket for missing values; marks join into group, total and meta rows; unmarked rows carry `null` and `mtm` = `net`; unknown group rejected; out-of-report group ignored; group spanning rows rejected; mark precision follows the account.
- import: all-or-nothing on error with line number; duplicates skipped and counted; numeric amount rejected; `--dry-run` writes nothing.
- meta: non-object rejected; stored and returned verbatim.
- sign rules per kind; zero rejected; uppercase currency normalization; case-insensitive account lookup.

CLI tests run the built binary with `assert_cmd`: JSON shapes for every command, exit codes 0/1/2, `--db` and `LEDGER_DB` path resolution, default path creation, stdin import.

## Decisions

Recorded from the design conversation on 2026-09-06:

| question | decision |
|---|---|
| interface | CLI binary; `src/lib.rs` is internal structure, not a supported API; no MCP, no HTTP |
| accounts | multiple accounts, each with its own currency and decimals |
| bookkeeping | single-entry signed amounts per account, not double-entry |
| mutability | append-only, corrections only by reversal entries |
| reconciliation | snapshot always; adjustment entry only with `--adjust` (automatic until 0.2, see the live-loop row below) |
| idempotency | optional `--ref` unique per account; conflict on mismatched amount |
| amount storage | integer minor units with per-account decimals |
| repository | standalone public repo `agent-ledger`, Rust, binary `ledger`, MIT |
| companion | Claude Code skill shipped in `skill/ledger/`, symlinked by `make install` |
| docs layout | `docs/specs/` and `docs/plans/` |

Added by the quant/arbitrage review on 2026-09-06:

| gap | decision |
|---|---|
| multi-leg positions across venues and currencies | caller-supplied `--group`; `group <id>` view with per-currency nets; reversals inherit group |
| structured trade attributes | `meta` JSON object column, `--meta`, conventional keys in the skill |
| realized PnL per period and strategy | `pnl` report excluding capital movements, bucketed by day/week/month/group/meta key |
| backfilling history | `import` from JSON Lines, one transaction, duplicates skipped, `--dry-run` |
| `pnl` kind ambiguous in a cash ledger | renamed to `settlement` |
| snapshots unreadable | `snapshots <account>` command; `reconcile --ts` computes book as of that time |
| reversing one leg of a caller group reversed everything | `reverse <id>` is single-entry (transfer sibling follows); `reverse --group` is explicit |
| open-position value, FX | stay out of scope; caller combines ledger cash with venue positions (revisited below: values may be handed to `pnl --marks`) |

Added after the first full trading day, 2026-09-06 (issue #4):

| gap | decision |
|---|---|
| open groups read as losses in `pnl`; agents joined venue values by hand | `pnl --marks <file>` takes caller-supplied values per group and adds `open_value` and `mtm` to every row; marks are never stored and the ledger still computes no valuation |
| stored marks (`ledger mark`) and group open/closed state | deferred; the read-time join covers the reported case, and a stored-mark table could feed the same output shape later |
| a reversal dated `now` without meta left `balance --at`, `--by day` and `--by meta:<key>` off by the reversed amount between mistake and correction, and `reconcile --ts` at a time in that window posted a phantom adjustment (skill re-test) | a reversal copies the original's `ts`, `group` and `meta`; `recorded_at` keeps the correction time |

Added from poly's live loop, 2026-09-10 (issues #9 and #3):

| gap | decision |
|---|---|
| the automatic adjustment absorbed fills that were not booked yet, and the next reconcile compounded the error (#9) | `reconcile` records the snapshot only; `--adjust` posts the adjustment; `--dry-run` writes nothing; `--no-adjust` removed |
| retries of a successful reconcile appended identical snapshots (#3) | a snapshot is one fact `(ts, observed, book, source)`; recording it again returns the existing row with `duplicate: true` unless the call posts an adjustment |

Known caveat: the binary name `ledger` collides with ledger-cli if that is installed. Rename the installed binary in that case; the skill refers to the command by name, so update it too.
