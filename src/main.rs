mod cli;

use std::path::PathBuf;

use clap::Parser;

use agent_ledger::ledger::{
    AddRequest, HistoryFilter, PnlBucket, PnlFilter, ReconcileRequest, TransferRequest,
};
use agent_ledger::model::Kind;
use agent_ledger::{Ledger, LedgerError};

use cli::output::Output;
use cli::{AccountCommand, Cli, Command, ExportFormat};

fn main() {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            let code = if e.use_stderr() { 1 } else { 0 };
            let _ = e.print();
            std::process::exit(code);
        }
    };
    let json =
        cli.json || matches!(&cli.command, Command::Export(e) if e.format == ExportFormat::Json);
    match run(cli) {
        Ok(Output::Raw(text)) => print!("{text}"),
        Ok(out) if json => println!(
            "{}",
            serde_json::to_string_pretty(&out).expect("output is serializable")
        ),
        Ok(out) => print!("{}", cli::render::render(&out)),
        Err(e) => {
            if json {
                let mut obj = serde_json::json!({ "code": e.code(), "message": e.to_string() });
                if let Some(line) = e.line() {
                    obj["line"] = serde_json::json!(line);
                }
                eprintln!("{}", serde_json::json!({ "error": obj }));
            } else {
                eprintln!("error: {e}");
            }
            std::process::exit(e.exit_code());
        }
    }
}

fn db_path(cli: &Cli) -> PathBuf {
    cli.db.clone().unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".agent-ledger")
            .join("ledger.db")
    })
}

fn run(cli: Cli) -> Result<Output, LedgerError> {
    let path = db_path(&cli);
    let actor = cli.actor.clone();
    let mut ledger = Ledger::open(&path)?;
    Ok(match cli.command {
        Command::Account {
            command:
                AccountCommand::Add {
                    name,
                    currency,
                    decimals,
                    note,
                },
        } => Output::Account {
            account: ledger.add_account(&name, &currency, decimals, note.as_deref())?,
        },
        Command::Account {
            command: AccountCommand::List,
        } => Output::Accounts {
            accounts: ledger.list_accounts()?,
        },
        Command::Add(a) => Output::Add(ledger.add(&AddRequest {
            account: a.account,
            amount: a.amount,
            kind: Kind::parse_addable(&a.kind)?,
            reference: a.reference,
            memo: a.memo,
            ts: a.ts,
            group: a.group,
            meta: a.meta,
            actor,
        })?),
        Command::Transfer(t) => Output::Transfer(ledger.transfer(&TransferRequest {
            from: t.from,
            to: t.to,
            amount: t.amount,
            reference: t.reference,
            memo: t.memo,
            ts: t.ts,
            group: t.group,
            meta: t.meta,
            actor,
        })?),
        Command::Reverse(r) => Output::Reverse(match (r.entry_id, r.group) {
            (Some(id), _) => ledger.reverse_entry(id, r.memo, actor)?,
            (None, Some(group)) => ledger.reverse_group(&group, r.memo, actor)?,
            (None, None) => unreachable!("clap requires entry_id or --group"),
        }),
        Command::Balance(b) => match b.account {
            Some(account) => Output::Balance(ledger.balance(&account, b.at.as_deref())?),
            None => Output::Balances {
                accounts: ledger.balances()?,
            },
        },
        Command::History(h) => {
            let kind = match h.kind {
                None => None,
                Some(k) => Some(Kind::parse(&k).ok_or(LedgerError::InvalidKind(k))?),
            };
            Output::History(ledger.history(
                &h.account,
                &HistoryFilter {
                    since: h.since,
                    until: h.until,
                    kind,
                    group: h.group,
                    limit: h.limit,
                },
            )?)
        }
        Command::Group { id } => Output::Group(ledger.group(&id)?),
        Command::Pnl(p) => Output::Pnl {
            accounts: ledger.pnl(
                p.account.as_deref(),
                &PnlFilter {
                    since: p.since,
                    until: p.until,
                    by: PnlBucket::parse(&p.by)?,
                },
            )?,
        },
        Command::Reconcile(r) => Output::Reconcile(ledger.reconcile(&ReconcileRequest {
            account: r.account,
            observed: r.observed,
            source: r.source,
            ts: r.ts,
            adjust: !r.no_adjust,
            actor,
        })?),
        Command::Snapshots(s) => Output::Snapshots(ledger.snapshots(&s.account, s.limit)?),
        Command::Import(i) => {
            let stdin = std::io::stdin();
            Output::Import(ledger.import(stdin.lock(), i.dry_run, actor.as_deref())?)
        }
        Command::Show { entry_id } => Output::Show(ledger.show(entry_id)?),
        Command::Export(e) => {
            let history = ledger.export(&e.account)?;
            match e.format {
                ExportFormat::Json => Output::History(history),
                ExportFormat::Csv => Output::Raw(cli::render::csv(&history)),
            }
        }
    })
}
