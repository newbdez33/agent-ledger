use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;

fn ledger(db: &Path) -> Command {
    let mut c = Command::cargo_bin("ledger").unwrap();
    c.arg("--db")
        .arg(db)
        .env_remove("LEDGER_ACTOR")
        .env_remove("LEDGER_DB");
    c
}

fn json(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes)
        .unwrap_or_else(|e| panic!("not json: {e}\n{}", String::from_utf8_lossy(bytes)))
}

fn with_account(db: &Path) {
    ledger(db)
        .args([
            "account",
            "add",
            "poly-usdc",
            "--currency",
            "usdc",
            "--decimals",
            "6",
        ])
        .assert()
        .success();
}

#[test]
fn add_balance_history_json_shapes() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("l.db");
    with_account(&db);

    let out = ledger(&db)
        .args([
            "--json",
            "--actor",
            "claude",
            "add",
            "poly-usdc",
            "100",
            "--kind",
            "deposit",
            "--ref",
            "0xabc",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json(&out.stdout);
    assert_eq!(v["entry"]["amount"], "100.000000");
    assert_eq!(v["entry"]["currency"], "USDC");
    assert_eq!(v["entry"]["ref"], "0xabc");
    assert_eq!(v["entry"]["actor"], "claude");
    assert_eq!(v["balance"], "100.000000");
    assert_eq!(v["duplicate"], false);

    ledger(&db)
        .args([
            "add",
            "poly-usdc",
            "-25.5",
            "--kind",
            "trade",
            "--group",
            "g1",
            "--meta",
            r#"{"side":"buy"}"#,
        ])
        .assert()
        .success();

    let v = json(
        &ledger(&db)
            .args(["--json", "balance"])
            .output()
            .unwrap()
            .stdout,
    );
    assert_eq!(v["accounts"][0]["account"], "poly-usdc");
    assert_eq!(v["accounts"][0]["balance"], "74.500000");
    assert_eq!(v["accounts"][0]["entries"], 2);

    let v = json(
        &ledger(&db)
            .args(["--json", "balance", "poly-usdc"])
            .output()
            .unwrap()
            .stdout,
    );
    assert_eq!(v["balance"], "74.500000");
    assert!(v["at"].is_null());

    let v = json(
        &ledger(&db)
            .args(["--json", "history", "poly-usdc", "--limit", "1"])
            .output()
            .unwrap()
            .stdout,
    );
    assert_eq!(v["entries"].as_array().unwrap().len(), 1);
    assert_eq!(v["entries"][0]["balance_after"], "74.500000");
    assert_eq!(v["entries"][0]["meta"]["side"], "buy");
    assert_eq!(v["entries"][0]["group_id"], "g1");
}

#[test]
fn account_add_identical_rerun_is_duplicate_mismatch_is_exists() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("l.db");
    let add = [
        "--json",
        "account",
        "add",
        "poly-usdc",
        "--currency",
        "USDC",
        "--decimals",
        "6",
    ];
    let first = json(&ledger(&db).args(add).output().unwrap().stdout);
    assert_eq!(first["duplicate"], false);
    let id = first["account"]["id"].clone();

    let out = ledger(&db).args(add).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let again = json(&out.stdout);
    assert_eq!(again["duplicate"], true);
    assert_eq!(again["account"]["id"], id);
    assert_eq!(again["account"]["currency"], "USDC");
    assert_eq!(again["account"]["decimals"], 6);

    let mismatch = ledger(&db)
        .args([
            "--json",
            "account",
            "add",
            "poly-usdc",
            "--currency",
            "USD",
            "--decimals",
            "6",
        ])
        .output()
        .unwrap();
    assert_eq!(mismatch.status.code(), Some(2));
    assert!(mismatch.stdout.is_empty());
    assert_eq!(json(&mismatch.stderr)["error"]["code"], "account_exists");
}

#[test]
fn duplicate_ref_exits_0_and_conflict_exits_2_with_json_error() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("l.db");
    with_account(&db);
    let args = [
        "--json",
        "add",
        "poly-usdc",
        "10",
        "--kind",
        "deposit",
        "--ref",
        "tx1",
    ];
    ledger(&db).args(args).assert().success();
    let v = json(&ledger(&db).args(args).output().unwrap().stdout);
    assert_eq!(v["duplicate"], true);

    let out = ledger(&db)
        .args([
            "--json",
            "add",
            "poly-usdc",
            "11",
            "--kind",
            "deposit",
            "--ref",
            "tx1",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let e = json(&out.stderr);
    assert_eq!(e["error"]["code"], "ref_conflict");
    assert!(e["error"]["message"].as_str().unwrap().contains("tx1"));
}

#[test]
fn exit_codes_for_domain_and_usage_errors() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("l.db");
    with_account(&db);
    ledger(&db)
        .args(["add", "nope", "1", "--kind", "deposit"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("not found"));
    ledger(&db)
        .args(["add", "poly-usdc", "5", "--kind", "fee"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("negative"));
    ledger(&db)
        .args(["add", "poly-usdc", "1", "--kind", "transfer"])
        .assert()
        .code(2);
    ledger(&db).args(["frobnicate"]).assert().code(1);
    ledger(&db).args(["reverse"]).assert().code(1);
    ledger(&db)
        .args([
            "account",
            "add",
            "x",
            "--currency",
            "USD",
            "--decimals",
            "19",
        ])
        .assert()
        .code(1);
    ledger(&db).args(["--help"]).assert().code(0);
}

#[test]
fn import_from_stdin_with_dry_run_then_real() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("l.db");
    with_account(&db);
    let lines = "{\"account\":\"poly-usdc\",\"amount\":\"100\",\"kind\":\"deposit\",\"ref\":\"d1\"}\n\
                 {\"account\":\"poly-usdc\",\"amount\":\"-40\",\"kind\":\"trade\",\"ref\":\"t1\",\"group\":\"g\"}\n";
    let v = json(
        &ledger(&db)
            .args(["--json", "import", "--dry-run"])
            .write_stdin(lines)
            .output()
            .unwrap()
            .stdout,
    );
    assert_eq!(v["imported"], 2);
    assert_eq!(v["dry_run"], true);
    let v = json(
        &ledger(&db)
            .args(["--json", "balance", "poly-usdc"])
            .output()
            .unwrap()
            .stdout,
    );
    assert_eq!(v["balance"], "0.000000");

    let v = json(
        &ledger(&db)
            .args(["--json", "import"])
            .write_stdin(lines)
            .output()
            .unwrap()
            .stdout,
    );
    assert_eq!(v["imported"], 2);
    let v = json(
        &ledger(&db)
            .args(["--json", "import"])
            .write_stdin(lines)
            .output()
            .unwrap()
            .stdout,
    );
    assert_eq!(v["duplicates"], 2);

    let bad = "{\"account\":\"poly-usdc\",\"amount\":\"1\",\"kind\":\"deposit\"}\n{\"account\":\"poly-usdc\",\"amount\":1,\"kind\":\"deposit\"}\n";
    let out = ledger(&db)
        .args(["--json", "import"])
        .write_stdin(bad)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let e = json(&out.stderr);
    assert_eq!(e["error"]["code"], "invalid_amount");
    assert_eq!(e["error"]["line"], 2);
}

#[test]
fn group_pnl_reconcile_and_snapshots_flow() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("l.db");
    with_account(&db);
    ledger(&db)
        .args(["account", "add", "kalshi-usd", "--currency", "USD"])
        .assert()
        .success();
    ledger(&db)
        .args([
            "add",
            "poly-usdc",
            "-45",
            "--kind",
            "trade",
            "--group",
            "arb:1",
            "--meta",
            r#"{"strategy":"arb"}"#,
        ])
        .assert()
        .success();
    ledger(&db)
        .args([
            "add",
            "kalshi-usd",
            "-52",
            "--kind",
            "trade",
            "--group",
            "arb:1",
            "--meta",
            r#"{"strategy":"arb"}"#,
        ])
        .assert()
        .success();
    ledger(&db)
        .args([
            "add",
            "poly-usdc",
            "100",
            "--kind",
            "settlement",
            "--group",
            "arb:1",
            "--ref",
            "settle:m1",
        ])
        .assert()
        .success();

    let v = json(
        &ledger(&db)
            .args(["--json", "group", "arb:1"])
            .output()
            .unwrap()
            .stdout,
    );
    assert_eq!(v["entries"].as_array().unwrap().len(), 3);
    assert_eq!(v["net"]["USDC"], "55.000000");
    assert_eq!(v["net"]["USD"], "-52.00");

    let v = json(
        &ledger(&db)
            .args(["--json", "pnl", "--by", "meta:strategy"])
            .output()
            .unwrap()
            .stdout,
    );
    assert_eq!(v["accounts"][0]["account"], "kalshi-usd");
    assert_eq!(v["accounts"][0]["rows"][0]["bucket"], "arb");
    assert_eq!(v["accounts"][0]["rows"][0]["net"], "-52.00");
    // poly-usdc: the settlement has no meta, so it lands in the null bucket, which sorts first.
    let poly_rows = v["accounts"][1]["rows"].as_array().unwrap();
    assert_eq!(poly_rows[0]["bucket"], Value::Null);
    assert_eq!(poly_rows[0]["net"], "100.000000");
    assert_eq!(poly_rows[1]["bucket"], "arb");
    assert_eq!(poly_rows[1]["net"], "-45.000000");

    let v = json(
        &ledger(&db)
            .args([
                "--json",
                "reconcile",
                "poly-usdc",
                "--observed",
                "54.5",
                "--source",
                "chain",
            ])
            .output()
            .unwrap()
            .stdout,
    );
    assert_eq!(v["snapshot"]["diff"], "-0.500000");
    assert_eq!(v["adjustment"]["kind"], "adjustment");
    let v = json(
        &ledger(&db)
            .args(["--json", "snapshots", "poly-usdc"])
            .output()
            .unwrap()
            .stdout,
    );
    assert_eq!(v["snapshots"].as_array().unwrap().len(), 1);
    assert_eq!(v["snapshots"][0]["source"], "chain");

    let v = json(
        &ledger(&db)
            .args(["--json", "reverse", "--group", "arb:1"])
            .output()
            .unwrap()
            .stdout,
    );
    assert_eq!(v["entries"].as_array().unwrap().len(), 3);
    let v = json(
        &ledger(&db)
            .args(["--json", "show", "1"])
            .output()
            .unwrap()
            .stdout,
    );
    assert!(v["entry"]["reversed_by"].is_number());

    let v = json(
        &ledger(&db)
            .args(["--json", "transfer", "poly-usdc", "kalshi-usd", "1"])
            .output()
            .unwrap()
            .stderr,
    );
    assert_eq!(v["error"]["code"], "currency_mismatch");
}

#[test]
fn ledger_db_env_and_default_dir_creation() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("deep").join("l.db");
    let mut c = Command::cargo_bin("ledger").unwrap();
    c.env("LEDGER_DB", &db).env_remove("LEDGER_ACTOR");
    c.args(["account", "add", "w", "--currency", "USD"])
        .assert()
        .success();
    assert!(db.exists());
}

#[test]
fn export_csv_header_and_json_shape() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("l.db");
    with_account(&db);
    ledger(&db)
        .args([
            "add",
            "poly-usdc",
            "1",
            "--kind",
            "deposit",
            "--memo",
            "a, \"quoted\"",
        ])
        .assert()
        .success();
    let out = ledger(&db)
        .args(["export", "poly-usdc", "--format", "csv"])
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    let mut lines = text.lines();
    assert_eq!(
        lines.next().unwrap(),
        "id,ts,recorded_at,kind,amount,balance_after,ref,memo,actor,group_id,meta,reverses_id,reversed_by"
    );
    assert!(lines.next().unwrap().contains("\"a, \"\"quoted\"\"\""));
    let v = json(
        &ledger(&db)
            .args(["export", "poly-usdc", "--format", "json"])
            .output()
            .unwrap()
            .stdout,
    );
    assert_eq!(v["entries"][0]["balance_after"], "1.000000");
}

#[test]
fn pnl_marks_add_open_value_and_mtm() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("l.db");
    with_account(&db);
    ledger(&db)
        .args(["account", "add", "kalshi-usd", "--currency", "USD"])
        .assert()
        .success();
    let trade = |account: &str, amount: &str, group: &str, strategy: &str| {
        ledger(&db)
            .args([
                "add",
                account,
                amount,
                "--kind",
                "trade",
                "--group",
                group,
                "--meta",
                &format!(r#"{{"strategy":"{strategy}"}}"#),
            ])
            .assert()
            .success();
    };
    trade("poly-usdc", "-23.2", "farm:a", "farm");
    trade("poly-usdc", "-7.44", "farm:b", "farm");
    trade("poly-usdc", "-5", "dir:c", "dir");
    trade("poly-usdc", "-45", "arb:1", "arb");
    trade("kalshi-usd", "-52", "arb:1", "arb");

    let marks = dir.path().join("marks.json");
    let marks_arg = marks.to_str().unwrap().to_string();
    std::fs::write(&marks, r#"{"farm:a":"21.60","farm:b":"5.5","dir:c":"0"}"#).unwrap();

    let v = json(
        &ledger(&db)
            .args([
                "--json",
                "pnl",
                "poly-usdc",
                "--by",
                "group",
                "--marks",
                &marks_arg,
            ])
            .output()
            .unwrap()
            .stdout,
    );
    let rows = v["accounts"][0]["rows"].as_array().unwrap();
    let row = |bucket: &str| rows.iter().find(|r| r["bucket"] == bucket).unwrap();
    assert_eq!(row("farm:a")["net"], "-23.200000");
    assert_eq!(row("farm:a")["open_value"], "21.600000");
    assert_eq!(row("farm:a")["mtm"], "-1.600000");
    assert_eq!(row("dir:c")["open_value"], "0.000000");
    assert_eq!(row("dir:c")["mtm"], "-5.000000");
    // arb:1 is not in the marks file: open_value stays null and mtm equals net.
    assert_eq!(row("arb:1")["open_value"], Value::Null);
    assert_eq!(row("arb:1")["mtm"], "-45.000000");

    let v = json(
        &ledger(&db)
            .args([
                "--json",
                "pnl",
                "poly-usdc",
                "--by",
                "meta:strategy",
                "--marks",
                &marks_arg,
            ])
            .output()
            .unwrap()
            .stdout,
    );
    let farm = v["accounts"][0]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["bucket"] == "farm")
        .unwrap();
    assert_eq!(farm["net"], "-30.640000");
    assert_eq!(farm["open_value"], "27.100000");
    assert_eq!(farm["mtm"], "-3.540000");

    // Without --marks the fields are still there: null and equal to net.
    let v = json(
        &ledger(&db)
            .args(["--json", "pnl", "poly-usdc", "--by", "group"])
            .output()
            .unwrap()
            .stdout,
    );
    for r in v["accounts"][0]["rows"].as_array().unwrap() {
        assert_eq!(r["open_value"], Value::Null);
        assert_eq!(r["mtm"], r["net"]);
    }
    // The table shows the two columns only when marks were given.
    let plain = ledger(&db)
        .args(["pnl", "poly-usdc", "--by", "group"])
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&plain.stdout).contains("mtm"));
    let marked = ledger(&db)
        .args(["pnl", "poly-usdc", "--by", "group", "--marks", &marks_arg])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&marked.stdout);
    assert!(
        text.contains("open_value") && text.contains("mtm"),
        "{text}"
    );

    // arb:1 has legs on both accounts, so one mark cannot be attributed to a single row.
    std::fs::write(&marks, r#"{"arb:1":"10"}"#).unwrap();
    let out = ledger(&db)
        .args(["--json", "pnl", "--by", "group", "--marks", &marks_arg])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json(&out.stderr)["error"]["code"], "mark_ambiguous");

    // A group id that exists nowhere is a typo, not something to ignore.
    std::fs::write(&marks, r#"{"farm:zzz":"1"}"#).unwrap();
    let out = ledger(&db)
        .args(["--json", "pnl", "--by", "group", "--marks", &marks_arg])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json(&out.stderr)["error"]["code"], "group_not_found");

    // An unreadable marks file is an I/O error naming the path.
    ledger(&db)
        .args([
            "--json",
            "pnl",
            "--marks",
            dir.path().join("nope.json").to_str().unwrap(),
        ])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("io_error"))
        .stderr(predicates::str::contains("nope.json"));
}
