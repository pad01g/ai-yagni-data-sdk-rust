//! 層1〜3: 検査が、未来を閉じる変更を止め、閉じない変更を通すこと。
//!
//! 試料のプロジェクト（tests/fixtures/ledger。ポイント台帳）を一時ディレクトリに複製して書き換え、検査を走らせる。
//! 語彙（vocab/ledger.rs）と未来の宣言（futures/<id>/future.rs）はこのテストに組み込み、
//! SQL のファイル（スキーマ・代表データ・差分・probe）は複製から読む。
//!
//! MySQL は `AI_YAGNI_MYSQL` のサーバーを使う（`with-mysql cargo test`、`make test` がそうする）。
#[path = "fixtures/ledger/vocab/ledger.rs"]
mod ledger;

#[path = "fixtures/ledger/futures/audit-trail/future.rs"]
mod audit_trail;
#[path = "fixtures/ledger/futures/erasure/future.rs"]
mod erasure;
#[path = "fixtures/ledger/futures/multi-currency/future.rs"]
mod multi_currency;
#[path = "fixtures/ledger/futures/overdraft/future.rs"]
mod overdraft;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use ai_yagni_data_sdk::check::{Checker, Options};
use ai_yagni_data_sdk::future::Future;
use ai_yagni_data_sdk::lint;
use ai_yagni_data_sdk::mysql::{ENV, Server};
use ai_yagni_data_sdk::project::Project;

use ledger::Ledger;

const TODAY: &str = "2026-10-28";
const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ledger");

fn server() -> Server {
    let name = std::env::var(ENV).unwrap_or_else(|_| panic!("set {ENV} (run the tests with `with-mysql cargo test`)"));
    Server::attach(name)
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()));
        } else {
            fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

/// 試料の複製。`<一時ディレクトリ>/ledger` を返す。
fn copy_ledger() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let ws =
        std::env::temp_dir().join(format!("datafutures-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    let _ = fs::remove_dir_all(&ws);
    let root = ws.join("ledger");
    copy_dir(Path::new(FIXTURE), &root);
    root
}

fn edit(root: &Path, file: &str, old: &str, new: &str) {
    let p = root.join(file);
    let s = fs::read_to_string(&p).unwrap();
    assert!(s.contains(old), "{file} does not contain {old}");
    fs::write(&p, s.replacen(old, new, 1)).unwrap();
}

fn project(root: &Path, v: &Ledger, tweak: impl Fn(Future) -> Future) -> Project {
    Project::new(root)
        .vocabulary("vocab/ledger.rs")
        .words(v.all())
        .invariants([&v.balance_is_derivable, &v.amounts_are_whole_points, &v.transfers_are_keyed_by_request])
        .future(tweak(audit_trail::future(v)))
        .future(tweak(erasure::future(v)))
        .future(tweak(multi_currency::future(v)))
        .future(tweak(overdraft::future(v)))
}

fn run_with(
    root: &Path,
    tweak: impl Fn(Future) -> Future,
    changed: Option<&[&str]>,
    today: &str,
) -> BTreeMap<String, Vec<String>> {
    let server = server();
    let v = Ledger::new();
    let checker = Checker::new(&server, project(root, &v, tweak));
    let opts = Options {
        today: today.to_owned(),
        changed: changed.map(|c| c.iter().map(|s| (*s).to_owned()).collect()),
        repo: root.parent().unwrap().to_path_buf(),
        vocabulary_uses: vec!["crate::ledger".to_owned()],
    };
    checker.run(&opts).failures().into_iter().collect()
}

fn run(root: &Path) -> BTreeMap<String, Vec<String>> {
    run_with(root, |f| f, None, TODAY)
}

fn keys(r: &BTreeMap<String, Vec<String>>) -> Vec<&str> {
    r.keys().map(String::as_str).collect()
}

fn joined(r: &BTreeMap<String, Vec<String>>, key: &str) -> String {
    r.get(key).map(|v| v.join("\n")).unwrap_or_default()
}

#[test]
fn the_unchanged_copy_passes() {
    assert_eq!(run(&copy_ledger()), BTreeMap::new());
}

#[test]
fn copying_the_recipients_email_into_transfers_closes_erasure() {
    let root = copy_ledger();
    edit(
        &root,
        "schema.sql",
        "  created_at DATETIME(3) NOT NULL,\n  PRIMARY KEY (id),\n  CONSTRAINT transfers_from",
        "  created_at DATETIME(3) NOT NULL,\n  to_email   VARCHAR(255) NULL,\n  PRIMARY KEY (id),\n  CONSTRAINT transfers_from",
    );
    let r = run(&root);
    assert_eq!(keys(&r), ["future erasure"]);
    assert!(joined(&r, "future erasure").contains("pii_only_in_accounts: transfers.to_email is outside accounts"));
}

#[test]
fn money_as_decimal_closes_multi_currency_and_overdraft() {
    let root = copy_ledger();
    edit(&root, "schema.sql", "  amount     BIGINT      NOT NULL,", "  amount     DECIMAL(12,2) NOT NULL,");
    let r = run(&root);
    assert_eq!(keys(&r), ["future multi-currency", "future overdraft", "present"]);
    assert!(joined(&r, "present").contains("invariant: amounts_are_whole_points"));
    assert!(joined(&r, "future overdraft").contains("(the present closes this future)"));
}

#[test]
fn renaming_the_balance_floor_breaks_the_overdraft_change() {
    let root = copy_ledger();
    edit(&root, "schema.sql", "CONSTRAINT accounts_balance_nonnegative CHECK", "CONSTRAINT balance_floor CHECK");
    let r = run(&root);
    assert_eq!(keys(&r), ["future overdraft"]);
    let s = joined(&r, "future overdraft");
    assert!(s.contains("balance_floor_is_named") && s.contains("change does not apply"), "{s}");
}

#[test]
fn data_that_cannot_be_rebuilt_from_the_records_closes_multi_currency() {
    let root = copy_ledger();
    fs::write(
        root.join("fixtures/0002_manual_adjustment.sql"),
        "UPDATE accounts SET balance = balance + 5 WHERE id = '00000000-0000-4000-8000-0000000000a3';\n",
    )
    .unwrap();
    let r = run(&root);
    assert_eq!(keys(&r), ["future audit-trail", "future multi-currency", "present"]);
}

#[test]
fn a_legitimate_change_that_keeps_every_future_open_passes() {
    let root = copy_ledger();
    edit(
        &root,
        "schema.sql",
        "  created_at DATETIME(3)  NOT NULL,\n  PRIMARY KEY (id),\n  CONSTRAINT accounts_balance",
        "  created_at DATETIME(3)  NOT NULL,\n  frozen_at  DATETIME(3)  NULL,\n  PRIMARY KEY (id),\n  CONSTRAINT accounts_balance",
    );
    assert_eq!(run(&root), BTreeMap::new());
}

#[test]
fn a_future_that_has_come_is_reported_as_realized() {
    let root = copy_ledger();
    edit(
        &root,
        "schema.sql",
        "  amount     BIGINT      NOT NULL,",
        "  amount     BIGINT      NOT NULL,\n  unit       CHAR(3)     NOT NULL DEFAULT 'PT',",
    );
    let r = run(&root);
    assert!(joined(&r, "future multi-currency").contains("realized"));
}

#[test]
fn an_expired_future_fails() {
    let r = run_with(&copy_ledger(), |f| f, None, "2027-05-01");
    assert_eq!(keys(&r), ["future overdraft"]);
    assert!(r["future overdraft"][0].contains("expired in 2027-04"));
}

#[test]
fn a_declared_conflict_must_show_in_the_data() {
    let tweak = |f: Future| match f.id.as_str() {
        "overdraft" => f.conflicts_with("multi-currency", "思い込み"),
        "multi-currency" => f.conflicts_with("overdraft", "思い込み"),
        _ => f,
    };
    let r = run_with(&copy_ledger(), tweak, None, TODAY);
    assert_eq!(r["conflicts"].len(), 2);
}

#[test]
fn the_conflict_between_audit_and_erasure_disappears_if_audit_stops_guarding_identities() {
    let root = copy_ledger();
    let change = fs::read_to_string(root.join("futures/audit-trail/change.sql")).unwrap();
    let cut = change.find("CREATE TRIGGER accounts_no_delete").unwrap();
    fs::write(root.join("futures/audit-trail/change.sql"), &change[..cut]).unwrap();
    let tweak = |mut f: Future| {
        f.rejects.retain(|p| !p.file.ends_with("rename_account.sql"));
        f
    };
    let r = run_with(&root, tweak, None, TODAY);
    assert_eq!(keys(&r), ["conflicts"]);
}

#[test]
fn layer_3_may_not_hold_sql_or_reach_for_layer_1() {
    let root = copy_ledger();
    edit(
        &root,
        "futures/erasure/future.rs",
        "use crate::ledger::Ledger;",
        "use crate::ledger::Ledger;\nuse ai_yagni_data_sdk::vocab::table_exists;\nconst RAW: &str = \"DELETE FROM accounts\";",
    );
    let r = run(&root);
    assert_eq!(
        r["rules"],
        [
            "futures/erasure/future.rs:4: may not use ai_yagni_data_sdk::vocab::table_exists",
            "futures/erasure/future.rs:5: SQL in a string (put it in a .sql file)",
        ]
    );
}

#[test]
fn layer_2_may_not_hold_sql() {
    let root = copy_ledger();
    edit(
        &root,
        "vocab/ledger.rs",
        "pub struct Ledger {",
        "const RAW: &str = \"SELECT id FROM accounts\";\npub struct Ledger {",
    );
    let allowed = ["ai_yagni_data_sdk::vocab"];
    assert_eq!(lint::dir(Path::new(FIXTURE).join("vocab").as_path(), &allowed), Vec::<String>::new());
    let problems = lint::dir(&root.join("vocab"), &allowed);
    assert_eq!(problems.len(), 1);
    assert!(problems[0].contains("SQL in a string"), "{problems:?}");
}

#[test]
fn undeclared_future_directories_and_unknown_words_are_rules_problems() {
    let root = copy_ledger();
    fs::create_dir_all(root.join("futures/sharding")).unwrap();
    let r = run_with(&root, |f| if f.id == "overdraft" { f.expires("2027") } else { f }, None, TODAY);
    assert_eq!(r["rules"], ["futures/sharding: not declared in the project", "overdraft: expires must be YYYY-MM"]);
}

#[test]
fn changing_the_vocabulary_makes_every_future_that_uses_it_to_be_re_read() {
    let root = copy_ledger();
    let r = run_with(&root, |f| f, Some(&["ledger/vocab/ledger.rs", "ledger/futures/erasure/future.rs"]), TODAY);
    assert_eq!(keys(&r), ["cochange audit-trail", "cochange multi-currency", "cochange overdraft"]);
    assert_eq!(run_with(&root, |f| f, Some(&["ledger/schema.sql"]), TODAY), BTreeMap::new());
}
