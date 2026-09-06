# agent-ledger — Design

**Goal:** An append-only, SQLite-backed ledger that AI agents drive from the shell. It records balance movements (deposits, withdrawals, trades, fees, PnL) across multiple accounts and currencies, reconciles book balances against observed balances, and keeps a complete audit trail that no caller can rewrite.

**Non-goal:** Double-entry bookkeeping with a chart of accounts, multi-user authentication, a network service or HTTP API, valuation of non-cash positions, automatic ingestion from exchanges or chains (the caller fetches and posts), and any reporting beyond balance, history, and export.

## Context

Agents that move money — trading bots, agents paying for APIs, agents holding wallets — have wallets but no books. A wallet answers "what is the balance now"; it does not answer "why did it change" or "does what we think happened match what the chain says".

Existing options do not fit:

- TigerBeetle and Formance Ledger are rigorous double-entry engines, but each runs as a service the operator has to deploy and keep alive.
- Agent wallet toolkits (Coinbase AgentKit, Ledger Agent Stack, Skyfire) move money and enforce spend limits; they do not keep books.
- Plain-text accounting (hledger, beancount) is agent-readable, but it is double-entry and has no structured write path or idempotency for a retrying agent.

The immediate consumer is a Polymarket trading bot whose only wallet record today is a Redis snapshot of `{usdc, fetched_at}` plus strategy-level PnL. Deposits, withdrawals, redeems, and fees are not recorded anywhere.

## Users and interface

- **AI agents** (Claude Code and similar) shell out to the `ledger` binary with `--json` and parse the result. A companion skill teaches them when and how.
- **Automated processes** (e.g. a trader daemon) call the same binary to record fills.
- **Humans** read `ledger balance` and `ledger history` as aligned text tables.

## Architecture

One Rust crate, `agent-ledger`, producing one binary, `ledger`.

```
src/lib.rs        Ledger core over rusqlite: open/migrate, accounts, entries,
                  transfers, reversals, balances, history, reconcile, export.
                  No CLI concerns; every operation is one SQLite transaction.
src/main.rs       clap CLI: parse args, call the core, render a table or JSON,
                  map errors to exit codes.
skill/ledger/     SKILL.md companion skill for agents.
Makefile          install: cargo install --path . and symlink skill/ledger
                  into ~/.claude/skills/ledger.
docs/specs/       Design documents (this file).
docs/plans/       Implementation plans.
```

Dependencies: `rusqlite` (bundled SQLite), `clap` (derive), `rust_decimal`, `chrono`, `serde`/`serde_json`, `thiserror`, `anyhow`, `uuid` (transfer group ids), `dirs` (home directory). Dev: `assert_cmd`, `predicates`, `tempfile`.

### Storage

- One SQLite file. WAL journal mode, `busy_timeout` 5000 ms, `foreign_keys = ON` per connection. Multiple agents and processes may write concurrently; every write is a `BEGIN IMMEDIATE` transaction.
- Path resolution: `--db <path>`, else `LEDGER_DB` environment variable, else `~/.agent-ledger/ledger.db`. The directory is created if missing. There is no `init` command: opening the file creates the schema and applies migrations. `meta.schema_version` starts at 1.

### Money and time

- Amounts are stored as `INTEGER` minor units. Each account declares `decimals` (USDC 6, USD 2, BTC 8). Input is parsed with `rust_decimal`; an input with more fractional digits than the account allows is rejected, never rounded. Sums in SQL are exact integer sums. Output renders as a decimal string with exactly `decimals` fraction digits, and JSON carries amounts as strings, never numbers.
- Sign convention: positive is an inflow to the account, negative is an outflow. Zero is rejected.
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
                 ('deposit','withdrawal','trade','fee','pnl',
                  'transfer','adjustment','reversal','other')),
  amount       INTEGER NOT NULL CHECK (amount <> 0),
  ref          TEXT,
  memo         TEXT,
  actor        TEXT,
  reverses_id  INTEGER REFERENCES entries(id),
  group_id     TEXT
);
CREATE UNIQUE INDEX entries_account_ref ON entries(account_id, ref)   WHERE ref IS NOT NULL;
CREATE UNIQUE INDEX entries_reverses    ON entries(reverses_id)       WHERE reverses_id IS NOT NULL;
CREATE INDEX        entries_account_ts  ON entries(account_id, ts, id);

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

Append-only is enforced by the database itself, so it holds even when someone opens the file with `sqlite3` directly. Accounts have no delete or edit command; the foreign keys make deleting an account with entries impossible anyway.

### Rules

**Kinds and signs.** `deposit` must be positive. `withdrawal` and `fee` must be negative. `trade`, `pnl`, `adjustment`, `other` may carry either sign. `transfer` and `reversal` are written only by their own commands, never by `add`.

**Idempotent `add` with `--ref`.** When `--ref` is given and `(account, ref)` already exists:

- same `kind` and `amount` → return the existing entry with `"duplicate": true`, exit 0. A retrying agent gets a success.
- different `kind` or `amount` → error `ref_conflict`, exit 2. Nothing is written.

Entries without `--ref` are never deduplicated.

**Transfer.** `transfer <from> <to> <amount>` requires a positive amount and two accounts with the same `currency`. It writes two `transfer` entries in one transaction: `-amount` on `from`, `+amount` on `to`, sharing a fresh `group_id` (UUID v4). With `--ref`, both legs carry the ref. On replay, if both legs already exist with matching amounts the call returns them with `"duplicate": true`; if only one leg exists, or an amount differs, it is `ref_conflict` and nothing is written.

**Reversal.** `reverse <entry-id>` writes a `reversal` entry on the same account with `amount = -original.amount` and `reverses_id = original.id`, `ts = now`. If the original has a `group_id`, every entry in that group is reversed in one transaction and the reversals share a new `group_id`. Rejected when the target is itself a `reversal` (`cannot_reverse_reversal`) or already has a reversal (`already_reversed`, guaranteed by the unique index). To undo a reversal, add the entry again.

**Reconcile.** `reconcile <account> --observed <amount>` runs in one transaction: `book = SUM(amount)` over the account, `diff = observed - book`, insert a snapshot. If `diff != 0` and `--no-adjust` is absent, insert an `adjustment` entry with `amount = diff`, the same `ts` as the snapshot, memo `reconcile: observed <o>, book <b>`, and set `snapshot.adjustment_entry_id`. The book balance therefore equals the observed balance after every reconcile unless the caller opts out.

**Balance.** `balance` lists every account with its current balance. `balance <account> [--at <ts>]` sums entries with `ts <= at`. Both use `ts`, not `recorded_at`.

**History.** Entries of one account ordered by `(ts, id)` with a running balance computed by a window function (`SUM(amount) OVER (ORDER BY ts, id)`), never stored. `--since`/`--until` filter on `ts`, `--kind` filters on kind, `--limit N` (default 50) keeps the most recent N of the filtered set and prints them oldest first. The running balance is computed over the whole account before filtering, so it is always the true balance after that entry.

**Actor.** `--actor <name>` overrides the `LEDGER_ACTOR` environment variable; when neither is set, `actor` is null. It records who wrote the row (`claude`, `poly-trader`), not who owns the money.

### CLI

```
ledger [--db <path>] [--json] [--actor <name>] <command>

  account add <name> --currency <code> [--decimals <n>] [--note <text>]
  account list
  add <account> <amount> --kind <deposit|withdrawal|trade|fee|pnl|adjustment|other>
      [--ref <id>] [--memo <text>] [--ts <rfc3339>]
  transfer <from> <to> <amount> [--ref <id>] [--memo <text>] [--ts <rfc3339>]
  reverse <entry-id> [--memo <text>]
  balance [<account>] [--at <rfc3339>]
  history <account> [--limit <n>] [--since <rfc3339>] [--until <rfc3339>] [--kind <kind>]
  reconcile <account> --observed <amount> [--source <text>] [--no-adjust] [--ts <rfc3339>]
  show <entry-id>
  export <account> --format <csv|json>
```

- `<amount>` is a decimal with an optional leading `-`. An unsigned amount is positive. The positional accepts negative numbers (`clap` `allow_negative_numbers`).
- `--decimals` defaults to 2. Agents creating a stablecoin account pass `--decimals 6` explicitly; the skill says so.
- `account list` shows accounts (id, name, currency, decimals, note); `balance` shows money. They do not overlap.
- `export` writes every entry of the account with running balance to stdout; CSV columns are `id,ts,recorded_at,kind,amount,balance_after,ref,memo,actor,reverses_id,group_id`.

### Output

Default output is an aligned text table for humans. With `--json`, stdout carries exactly one JSON object per command. Amounts are decimal strings; timestamps are RFC 3339 strings; absent values are `null`.

`add` returns `{entry, balance, duplicate}`; `show` returns `{entry, balance}`; `transfer` returns `{entries, duplicate}` with the two legs; `reverse` returns `{entries}` with one entry per reversed leg. `reversed_by` is derived from the reversal index, not stored. The `add` shape:

```json
{
  "entry": {
    "id": 12, "account": "poly-usdc",
    "ts": "2026-09-06T03:12:45.000Z", "recorded_at": "2026-09-06T03:12:45.117Z",
    "kind": "deposit", "amount": "100.000000",
    "ref": "0xabc…", "memo": null, "actor": "claude",
    "reverses_id": null, "reversed_by": null, "group_id": null
  },
  "balance": "224.700000",
  "duplicate": false
}
```

`balance` (all accounts):

```json
{ "accounts": [
  { "account": "poly-usdc", "currency": "USDC", "balance": "124.700000",
    "entries": 37, "last_ts": "2026-09-06T03:12:45.000Z",
    "last_reconciled_at": "2026-09-05T22:00:00.000Z" }
] }
```

`balance <account>`:

```json
{ "account": "poly-usdc", "currency": "USDC", "balance": "124.700000", "at": null }
```

`history` and `export --format json`:

```json
{ "account": "poly-usdc", "currency": "USDC",
  "entries": [ { "id": 11, "…": "…", "balance_after": "124.700000" } ] }
```

`reconcile`:

```json
{ "snapshot": { "id": 3, "account": "poly-usdc", "ts": "…",
                "observed": "124.700000", "book": "126.200000",
                "diff": "-1.500000", "source": "polymarket-onchain" },
  "adjustment": { "id": 38, "kind": "adjustment", "amount": "-1.500000", "…": "…" } }
```

`adjustment` is `null` when `--no-adjust` was passed or `diff` was zero.

### Errors and exit codes

Errors go to stderr. In `--json` mode stderr carries one object and stdout stays empty:

```json
{ "error": { "code": "precision_exceeded",
             "message": "amount 1.2345678 has 7 decimals; account poly-usdc allows 6" } }
```

| exit | meaning |
|---|---|
| 0 | success, including idempotent duplicates |
| 1 | usage error, I/O error, database error |
| 2 | domain error, one of the codes below |

Domain error codes: `account_not_found`, `account_exists`, `entry_not_found`, `currency_mismatch`, `precision_exceeded`, `invalid_amount`, `zero_amount`, `invalid_sign`, `invalid_kind`, `invalid_timestamp`, `ref_conflict`, `already_reversed`, `cannot_reverse_reversal`.

### Companion skill

`skill/ledger/SKILL.md` is a Claude Code skill installed by `make install` as a symlink at `~/.claude/skills/ledger`. It contains:

- **When to use:** any time the agent moves money, records a fill, fee or PnL, checks a balance, or has just fetched a live balance from an exchange or chain.
- **Conventions:** always pass `--json`; always pass `--ref` when an external id exists (order id, tx hash); follow the sign convention; run `reconcile` right after fetching a live balance; never open the SQLite file directly; fix mistakes with `reverse`, never by editing.
- **Cheatsheet:** the command table above.
- **Worked flow:** a Polymarket wallet from `account add poly-usdc --currency USDC --decimals 6` through a deposit with its tx hash, a `trade` buy, a `pnl` on resolution, a `fee`, and a `reconcile` against the on-chain balance.

The skill is authored with the `writing-skills` skill during implementation so its frontmatter and structure are valid.

### Testing

Library tests run against a temporary database file:

- sums: balance equals the signed sum of entries; `--at` respects `ts`.
- idempotency: same ref, same kind and amount returns duplicate; different amount returns `ref_conflict` and writes nothing.
- reversal: negates and links; second reversal rejected; reversing a reversal rejected; reversing one transfer leg reverses the whole group.
- append-only: `UPDATE` and `DELETE` on `entries` and `snapshots` fail with the trigger message.
- transfer: atomic two-leg write; currency mismatch rejected; nothing written on failure.
- reconcile: snapshot stored, adjustment equals diff, `--no-adjust` writes no entry, zero diff writes no entry.
- precision: 7 decimals on a 6-decimal account rejected; 6 accepted exactly.
- history: running balance correct under `--limit` and `--since` filters.
- sign rules per kind; zero rejected.

CLI tests run the built binary with `assert_cmd`: JSON shapes for every command, exit codes 0/1/2, `--db` and `LEDGER_DB` path resolution, default path creation.

## Decisions

Recorded from the design conversation on 2026-09-06:

| question | decision |
|---|---|
| interface | CLI binary; `src/lib.rs` is internal structure, not a supported API; no MCP, no HTTP |
| accounts | multiple accounts, each with its own currency and decimals |
| bookkeeping | single-entry signed amounts per account, not double-entry |
| mutability | append-only, corrections only by reversal entries |
| reconciliation | snapshot plus automatic adjustment entry, opt-out with `--no-adjust` |
| idempotency | optional `--ref` unique per account; conflict on mismatched amount |
| amount storage | integer minor units with per-account decimals |
| repository | standalone public repo `agent-ledger`, Rust, binary `ledger` |
| companion | Claude Code skill shipped in `skill/ledger/`, symlinked by `make install` |
| docs layout | `docs/specs/` and `docs/plans/` |

Known caveat: the binary name `ledger` collides with ledger-cli if that is installed. Rename the installed binary in that case; the skill refers to the command by name, so update it too.
