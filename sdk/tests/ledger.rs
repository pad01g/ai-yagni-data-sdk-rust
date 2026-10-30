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
use std::time::Duration;

use ai_yagni_data_sdk::check::{Checker, Options};
use ai_yagni_data_sdk::future::Future;
use ai_yagni_data_sdk::lint;
use ai_yagni_data_sdk::mysql::{Db, ENV, Scratch, Server};
use ai_yagni_data_sdk::project::Project;
use ai_yagni_data_sdk::vocab::{Fact, SqlFile, Verdict, any, define, not, sql_empty, table_exists, unique_on};

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

fn options(root: &Path, changed: Option<&[&str]>, today: &str) -> Options {
    Options {
        today: today.to_owned(),
        changed: changed.map(|c| c.iter().map(|s| (*s).to_owned()).collect()),
        repo: root.parent().unwrap().to_path_buf(),
        vocabulary_uses: vec!["crate::ledger".to_owned()],
    }
}

fn run_opts(root: &Path, tweak: impl Fn(Future) -> Future, opts: &Options) -> BTreeMap<String, Vec<String>> {
    let server = server();
    let v = Ledger::new();
    let checker = Checker::new(&server, project(root, &v, tweak));
    checker.run(opts).failures().into_iter().collect()
}

fn run_with(
    root: &Path,
    tweak: impl Fn(Future) -> Future,
    changed: Option<&[&str]>,
    today: &str,
) -> BTreeMap<String, Vec<String>> {
    run_opts(root, tweak, &options(root, changed, today))
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

/// 試料の「今」に extra を当てた DB の上で、f を呼ぶ。
fn on_present<T>(extra: &str, f: impl FnOnce(&Db<'_>) -> T) -> T {
    let server = server();
    let v = Ledger::new();
    let checker = Checker::new(&server, project(Path::new(FIXTURE), &v, |f| f));
    let db = checker.present("on_present").unwrap();
    if !extra.is_empty() {
        db.db.run(extra).unwrap();
    }
    f(&db.db)
}

fn holds(extra: &str, fact: &Fact) -> Verdict {
    on_present(extra, |db| fact.holds(db))
}

fn count(db: &Db<'_>, sql: &str) -> String {
    db.query(sql).unwrap()[0].get("n").unwrap().to_owned()
}

// ---------------------------------------------------------------- 確かめられないことは、成り立つとも成り立たないとも数えない

const NO_BALANCES_YET: SqlFile =
    SqlFile::new("no_balances_yet.sql", "SELECT account_id FROM balances WHERE balance < 0");

#[test]
fn a_query_that_fails_is_not_negated_into_a_pass() {
    let v = holds("", &not(sql_empty(NO_BALANCES_YET, &[])));
    assert!(v.is_error(), "{v:?}");
    let v = holds("", &any([table_exists("accounts"), sql_empty(NO_BALANCES_YET, &[])]));
    assert!(v.is_error(), "{v:?}");
}

#[test]
fn an_infrastructure_error_never_shows_a_declared_conflict() {
    let v = Ledger::new();
    let gone = Server::attach("ai-yagni-mysql-that-does-not-exist");
    let checker = Checker::new(&gone, project(&copy_ledger(), &v, |f| f));
    let problems = checker.check_conflicts();
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(problems.iter().all(|p| p.contains("cannot tell whether the declared conflict")), "{problems:?}");
}

#[test]
fn a_present_that_cannot_be_built_does_not_show_a_declared_conflict() {
    let root = copy_ledger();
    edit(&root, "schema.sql", "CREATE TABLE daily_stats (", "CREATE TABLE daily_stats (oops");
    let r = run(&root);
    let s = joined(&r, "conflicts");
    assert!(s.contains("audit-trail then erasure: cannot tell") && s.contains("present:"), "{s}");
}

// ---------------------------------------------------------------- sql_empty は SELECT を 1 つだけ、読むだけで流す

#[test]
fn sql_empty_reads_exactly_one_select() {
    const TWO: SqlFile = SqlFile::new("two.sql", "SELECT id FROM accounts;\nSELECT id FROM accounts WHERE 0;\n");
    let fact = sql_empty(TWO, &[]);
    assert_eq!(fact.flaws(), ["two.sql: 2 statements (write exactly one)"]);
    assert!(holds("", &fact).is_error());
}

#[test]
fn sql_empty_cannot_change_the_database() {
    const DROP: SqlFile = SqlFile::new("drop.sql", "DROP TABLE daily_stats");
    const WRITES: SqlFile = SqlFile::new("writes.sql", "SELECT id FROM accounts WHERE touch() <> 0");
    let function = "CREATE TABLE touched (n INT);\n\
                    DELIMITER $$\n\
                    CREATE FUNCTION touch() RETURNS INT MODIFIES SQL DATA BEGIN INSERT INTO touched VALUES (1); RETURN 0; END$$\n\
                    DELIMITER ;\n";
    on_present(function, |db| {
        assert!(sql_empty(DROP, &[]).holds(db).is_error());
        assert!(table_exists("daily_stats").holds(db).ok());
        let v = sql_empty(WRITES, &[]).holds(db);
        assert!(v.is_error() && v.problems()[0].contains("READ ONLY"), "{v:?}");
        assert_eq!(count(db, "SELECT COUNT(*) AS n FROM touched"), "0");
    });
}

// ---------------------------------------------------------------- unique_on は、ちょうどその列だけの一意索引を数える

#[test]
fn unique_on_ignores_functional_prefix_and_wider_indexes() {
    let extra = "ALTER TABLE accounts ADD UNIQUE KEY by_email_and_name (email, (LOWER(name)));\n\
                 ALTER TABLE accounts ADD UNIQUE KEY by_name_prefix (name(5));\n\
                 ALTER TABLE transfers ADD UNIQUE KEY by_pair (from_id, to_id, created_at);";
    on_present(extra, |db| {
        assert!(!unique_on("accounts", &["email"]).holds(db).ok());
        assert!(!unique_on("accounts", &["name"]).holds(db).ok());
        assert!(!unique_on("transfers", &["from_id", "to_id"]).holds(db).ok());
        assert!(unique_on("accounts", &["id"]).holds(db).ok());
        db.run("ALTER TABLE accounts ADD UNIQUE KEY by_email (email)").unwrap();
        assert!(unique_on("accounts", &["email"]).holds(db).ok());
    });
}

// ---------------------------------------------------------------- probe は互いに独立で、accepts の後も then が成り立つ

#[test]
fn a_probe_that_commits_does_not_leak_into_the_next_probe() {
    let root = copy_ledger();
    fs::write(
        root.join("futures/overdraft/drop_floor.sql"),
        "ALTER TABLE accounts DROP CHECK accounts_within_credit;\n",
    )
    .unwrap();
    let tweak =
        |f: Future| if f.id == "overdraft" { f.accepts("drop_floor.sql", "DDL は暗黙に COMMIT する") } else { f };
    assert_eq!(run_with(&root, tweak, None, TODAY), BTreeMap::new());
}

#[test]
fn a_trailing_line_comment_in_a_probe_is_harmless() {
    let root = copy_ledger();
    edit(
        &root,
        "futures/overdraft/within_credit.sql",
        "WHERE id = '00000000-0000-4000-8000-0000000000a3';\nUPDATE accounts SET balance = -50 WHERE id = '00000000-0000-4000-8000-0000000000a3';",
        "WHERE id = '00000000-0000-4000-8000-0000000000a3';\nUPDATE accounts SET balance = -50 WHERE id = '00000000-0000-4000-8000-0000000000a3' -- 与信枠の範囲;",
    );
    assert_eq!(run(&root), BTreeMap::new());
}

#[test]
fn then_must_still_hold_after_each_accepted_probe() {
    let root = copy_ledger();
    edit(&root, "futures/multi-currency/usd_grant.sql", "'USD', 500)", "'USD', 400)");
    let r = run(&root);
    assert_eq!(keys(&r), ["future multi-currency"]);
    let s = joined(&r, "future multi-currency");
    assert!(s.contains("then after futures/multi-currency/usd_grant.sql: balances_per_unit_match_transfers"), "{s}");
}

// ---------------------------------------------------------------- 共変更はリポジトリの根を正しく読む

#[test]
fn cochange_reads_the_repository_root_however_it_is_written() {
    let root = copy_ledger();
    let mut opts = options(&root, Some(&["ledger/vocab/ledger.rs", "ledger/futures/erasure/future.rs"]), TODAY);
    opts.repo = root.join("..").join(".");
    let r = run_opts(&root, |f| f, &opts);
    assert_eq!(keys(&r), ["cochange audit-trail", "cochange multi-currency", "cochange overdraft"]);
    opts.repo = std::env::temp_dir().join("datafutures-elsewhere");
    fs::create_dir_all(&opts.repo).unwrap();
    let r = run_opts(&root, |f| f, &opts);
    assert_eq!(keys(&r), ["cochange"]);
    assert!(r["cochange"][0].contains("is not under the repository"), "{r:?}");
}

// ---------------------------------------------------------------- lint、日付、名前の大小、時間切れ

#[test]
fn layer_3_may_not_reach_for_layer_1_through_an_alias_or_a_full_path() {
    let root = copy_ledger();
    edit(
        &root,
        "futures/erasure/future.rs",
        "use ai_yagni_data_sdk::future::Future;",
        "use ai_yagni_data_sdk::{future::Future as Fut, vocab::not};\nuse ai_yagni_data_sdk::future::Future;\n\
         fn sneak() {\n    let _ = ai_yagni_data_sdk::vocab::table_exists(\"x\");\n    let _ = \"INSERT IGNORE INTO accounts\";\n    let _ = \"UPDATE\\naccounts SET name = 1\";\n}",
    );
    assert_eq!(
        run(&root)["rules"],
        [
            "futures/erasure/future.rs:1: may not use ai_yagni_data_sdk::vocab::not",
            "futures/erasure/future.rs:4: may not use ai_yagni_data_sdk::vocab::table_exists",
            "futures/erasure/future.rs:5: SQL in a string (put it in a .sql file)",
            "futures/erasure/future.rs:6: SQL in a string (put it in a .sql file)",
        ]
    );
}

#[test]
fn impossible_dates_and_a_review_in_the_future_are_rules_problems() {
    let tweak = |f: Future| match f.id.as_str() {
        "overdraft" => f.reviewed("2026-11-01"),
        "erasure" => f.reviewed("2026-02-31"),
        "multi-currency" => f.expires("2027-13"),
        _ => f,
    };
    let r = run_with(&copy_ledger(), tweak, None, TODAY);
    assert_eq!(
        r["rules"],
        [
            "erasure: reviewed must be YYYY-MM-DD",
            "multi-currency: expires must be YYYY-MM",
            "overdraft: reviewed 2026-11-01 is after today (2026-10-28)",
        ]
    );
    let r = run_with(&copy_ledger(), |f| f, None, "2026-02-30");
    assert_eq!(r["rules"], ["today must be a date YYYY-MM-DD: 2026-02-30"]);
}

#[test]
fn personal_columns_are_found_whatever_their_case() {
    let v = Ledger::new();
    let verdict = holds("ALTER TABLE transfers ADD COLUMN To_Email VARCHAR(255) NULL", &v.pii_only_in_accounts.fact());
    assert_eq!(verdict.problems(), ["pii_only_in_accounts: transfers.To_Email is outside accounts"]);
}

#[test]
fn a_statement_that_runs_too_long_is_stopped() {
    let shared = server();
    let impatient = Server::attach(shared.container()).timeout(Duration::from_secs(2));
    let db = Scratch::create(&impatient, "slow").unwrap();
    let e = db.db.run("SELECT SLEEP(10)").unwrap_err();
    assert!(e.is_other() && e.message.contains("timed out"), "{e}");
}

#[test]
fn a_word_written_wrong_is_reported_once_as_a_rules_problem() {
    const UNNAMED: SqlFile = SqlFile::new("unnamed.sql", "SELECT id FROM {{accounts}}");
    let server = server();
    let v = Ledger::new();
    let broken = define("broken", "名前を渡し忘れた", sql_empty(UNNAMED, &[]));
    let wrapper = define("wrapper", "壊れた語を使う", broken.fact());
    let checker = Checker::new(
        &server,
        project(&copy_ledger(), &v, |f| f).words([broken, wrapper, define("nothing", "空", any([]))]),
    );
    assert_eq!(
        checker.check_rules(&options(Path::new(FIXTURE), None, TODAY)),
        ["word broken: unnamed.sql: no name for {{accounts}}", "word nothing: any of nothing (no alternatives)"]
    );
}
