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
use ai_yagni_data_sdk::vocab::{
    Fact, SqlFile, Verdict, any, check_exists, define, not, sql_empty, table_exists, unique_on, unique_on_not_null,
    unique_within,
};

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
        .queries("queries")
        .grants("grants.sql", "app", Some("ledger"))
        .accepts("probes/new_account.sql", "残高 0 の口座は送金なしに作れる")
        .rejects_with("probes/negative_balance.sql", "3819", "accounts_balance_nonnegative", "残高は 0 を下回らない")
        .rejects("probes/orphan_transfer.sql", "1452", "送金の相手は、ある口座")
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
    assert_eq!(keys(&r), ["future overdraft", "present"]);
    let s = joined(&r, "future overdraft");
    assert!(s.contains("balance_floor_is_named") && s.contains("change does not apply"), "{s}");
    // 今の probe は、拒んだ制約の名前も確かめる
    let s = joined(&r, "present");
    assert!(s.contains("with 3819 naming 'accounts_balance_nonnegative', but got 3819"), "{s}");
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
fn a_statement_that_runs_too_long_is_stopped_on_the_server_too() {
    let shared = server();
    let impatient = Server::attach(shared.container()).timeout(Duration::from_secs(2));
    let db = Scratch::create(&impatient, "slow").unwrap();
    let e = db.db.run("SELECT SLEEP(29.25)").unwrap_err();
    assert!(e.is_other() && e.message.contains("timed out"), "{e}");
    // docker のクライアントを止めるだけでなく、サーバーの上の問い合わせも止める
    let still = "SELECT COUNT(*) AS n FROM information_schema.PROCESSLIST WHERE INFO LIKE 'SELECT SLEEP(29.25)%'";
    let probe = Scratch::create(&shared, "watch").unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(count(&probe.db, still), "0");
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

// ---------------------------------------------------------------- sql_empty の SELECT は、サーバーの PREPARE で 1 文だけ流す

/// 行を 1 つ持つ表 touched を作る。
const TOUCHED: &str = "CREATE TABLE touched (n INT);\nINSERT INTO touched VALUES (1);\n";

#[test]
fn sql_empty_cannot_smuggle_a_second_statement_past_the_lexer() {
    // mysql CLI は `\g` を「送る」と読む。サーバーは版のコメント `/*!99999 ... */` を読み飛ばす
    const GO: SqlFile = SqlFile::new("go.sql", "SELECT n FROM touched WHERE 0 \\g COMMIT \\g DELETE FROM touched");
    const VERSIONED: SqlFile =
        SqlFile::new("versioned.sql", "SELECT n FROM touched WHERE 0 /*!99999 ' */; COMMIT; DELETE FROM touched; -- '");
    on_present(TOUCHED, |db| {
        for file in [GO, VERSIONED] {
            let fact = sql_empty(file, &[]);
            assert_eq!(fact.flaws(), Vec::<String>::new(), "the lexer sees one statement in {}", file.name);
            let v = fact.holds(db);
            assert!(v.is_error(), "{}: {v:?}", file.name);
            assert_eq!(count(db, "SELECT COUNT(*) AS n FROM touched"), "1", "{}", file.name);
        }
    });
}

#[test]
fn mysql_cli_commands_are_refused_in_every_script() {
    let shared = server();
    let marker = format!("/tmp/ayg-shell-{}", std::process::id());
    let db = Scratch::create(&shared, "bang").unwrap();
    let e = db.db.run(&format!("SELECT 1;\n\\! touch {marker}\n")).unwrap_err();
    assert!(e.message.contains("Unknown command"), "{e}");
    let found = std::process::Command::new("docker")
        .args(["exec", shared.container(), "test", "-e", &marker])
        .status()
        .unwrap();
    assert!(!found.success(), "the shell command ran");
    let e = db.db.run("SELECT 1 \\g SELECT 2").unwrap_err();
    assert!(e.is_other(), "{e}");
}

#[test]
fn errors_carry_the_line_of_the_script() {
    let shared = server();
    let db = Scratch::create(&shared, "lines").unwrap();
    let e = db.db.run("SELECT 1;\n-- 2\nSELECT nope;\n").unwrap_err();
    assert_eq!((e.code, e.line), (Some(1054), Some(3)), "{e:?}");
    let root = copy_ledger();
    edit(&root, "futures/overdraft/change.sql", "DROP CHECK accounts_balance_nonnegative", "DROP CHECK nope");
    let s = joined(&run(&root), "future overdraft");
    assert!(s.contains("futures/overdraft/change.sql:2: Check constraint 'nope'"), "{s}");
}

// ---------------------------------------------------------------- 共変更と見張り

#[test]
fn a_changed_submodule_counts_as_a_change_of_the_watched_files_inside_it() {
    let root = copy_ledger();
    fs::create_dir_all(root.join("vendor/proxy/queries")).unwrap();
    fs::write(root.join("vendor/proxy/queries/lock.sql"), "SELECT 1").unwrap();
    let tweak = |f: Future| if f.id == "erasure" { f.watches("vendor/proxy/queries/lock.sql") } else { f };
    // `git diff --name-only` はサブモジュールの中のファイルでなく、サブモジュールのパスだけを出す
    let r = run_with(&root, tweak, Some(&["ledger/vendor/proxy"]), TODAY);
    assert_eq!(keys(&r), ["cochange erasure"]);
    assert!(r["cochange erasure"][0].starts_with("ledger/vendor/proxy changed"), "{r:?}");
}

#[test]
fn watches_must_exist_and_directories_must_end_with_a_star() {
    let tweak = |f: Future| {
        if f.id == "erasure" {
            f.watches("queries/nope.sql").watches("queries").watches("queries/").watches("queries/*")
        } else {
            f
        }
    };
    let r = run_with(&copy_ledger(), tweak, None, TODAY);
    assert_eq!(
        r["rules"],
        [
            "erasure: watch 'queries/nope.sql': no such file",
            "erasure: watch 'queries' is a directory (write it as queries/*)",
            "erasure: watch 'queries/' ends with '/' (write a directory as dir/*)",
        ]
    );
}

// ---------------------------------------------------------------- 固定クエリは、未来の差分の後にも PREPARE し直す

#[test]
fn a_future_that_breaks_a_fixed_query_must_watch_it() {
    let root = copy_ledger();
    let change = root.join("futures/erasure/change.sql");
    let mut sql = fs::read_to_string(&change).unwrap();
    sql.push_str("ALTER TABLE daily_stats RENAME COLUMN volume TO amount;\n");
    fs::write(&change, sql).unwrap();
    let r = run(&root);
    assert_eq!(keys(&r), ["future erasure"]);
    let s = joined(&r, "future erasure");
    assert!(s.contains("breaks the fixed query queries/daily_volume.sql but does not watch it"), "{s}");
    for watch in ["queries/daily_volume.sql", "queries/*"] {
        let tweak = |f: Future| if f.id == "erasure" { f.watches(watch) } else { f };
        assert_eq!(run_with(&root, tweak, None, TODAY), BTreeMap::new(), "{watch}");
    }
}

#[test]
fn fixed_queries_are_prepared_with_the_grants_of_their_user() {
    let root = copy_ledger();
    edit(&root, "grants.sql", "GRANT SELECT (id, balance) ON", "GRANT SELECT (id) ON");
    let r = run(&root);
    assert_eq!(keys(&r), ["present"]);
    let s = joined(&r, "present");
    assert!(
        s.contains("query queries/account_balance.sql does not prepare as app (grants.sql): SELECT command denied to user 'app'"),
        "{s}"
    );
    edit(&root, "grants.sql", "GRANT SELECT (id) ON", "CREATE USER x; GRANT SELECT ON");
    assert!(joined(&run(&root), "present").contains("only GRANT statements are allowed"));
}

// ---------------------------------------------------------------- rejects は、拒んだ制約の名前も確かめられる

#[test]
fn a_rejection_by_another_constraint_with_the_same_number_does_not_count() {
    let root = copy_ledger();
    // 与信枠の CHECK の名前を変えると、3819 で拒まれても名前が合わない
    edit(
        &root,
        "futures/overdraft/change.sql",
        "ADD CONSTRAINT accounts_within_credit",
        "ADD CONSTRAINT accounts_credit_floor",
    );
    let r = run(&root);
    assert_eq!(keys(&r), ["future overdraft"]);
    let s = joined(&r, "future overdraft");
    assert!(
        s.contains("should reject futures/overdraft/beyond_credit.sql with 3819 naming 'accounts_within_credit'"),
        "{s}"
    );
}

#[test]
fn a_reject_that_expects_the_generic_hy000_is_warned_about() {
    let server = server();
    let v = Ledger::new();
    let tweak =
        |f: Future| if f.id == "audit-trail" { f.rejects("rewrite_amount.sql", "HY000", "何でも通る") } else { f };
    let checker = Checker::new(&server, project(&copy_ledger(), &v, tweak));
    let w = checker.warnings();
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].starts_with("audit-trail: rejects futures/audit-trail/rewrite_amount.sql expects HY000"), "{w:?}");
}

// ---------------------------------------------------------------- 今の probe

#[test]
fn present_probes_check_what_constraints_and_triggers_do_today() {
    let root = copy_ledger();
    edit(&root, "probes/negative_balance.sql", "SET balance = -1", "SET balance = 5");
    edit(&root, "probes/new_account.sql", "'carol@example.com', 0,", "'carol@example.com', 9,");
    let s = joined(&run(&root), "present");
    assert!(s.contains("present: should reject probes/negative_balance.sql"), "{s}");
    assert!(s.contains("invariants after probes/new_account.sql: balance_is_derivable"), "{s}");
}

#[test]
fn a_fixture_that_turns_off_foreign_key_checks_does_not_leak_into_the_probes() {
    let root = copy_ledger();
    fs::write(root.join("fixtures/0002_session.sql"), "SET FOREIGN_KEY_CHECKS = 0;\n").unwrap();
    assert_eq!(run(&root), BTreeMap::new());
}

// ---------------------------------------------------------------- データベースの外を変える文

#[test]
fn statements_that_change_the_server_are_rules_problems() {
    let root = copy_ledger();
    fs::write(root.join("fixtures/0002_global.sql"), "SET GLOBAL max_connections = 500;\n").unwrap();
    edit(
        &root,
        "futures/overdraft/within_credit.sql",
        "UPDATE accounts SET credit_limit",
        "USE mysql;\nUPDATE ledger.accounts SET credit_limit",
    );
    let r = run_with(&root, |f| f, None, TODAY);
    let rules = &r["rules"];
    assert!(rules.contains(&"fixtures/0002_global.sql: statement 1 changes the server, not only this database (SET GLOBAL / SET PERSIST)".to_owned()), "{rules:?}");
    assert!(
        rules.contains(
            &"futures/overdraft/within_credit.sql: statement 1 changes the server, not only this database (USE)"
                .to_owned()
        ),
        "{rules:?}"
    );
}

// ---------------------------------------------------------------- 一意性: 列の集合、列の一部、NULL

#[test]
fn unique_on_compares_column_sets_and_unique_within_sees_narrower_indexes() {
    let extra = "ALTER TABLE transfers ADD UNIQUE KEY by_pair (to_id, from_id);\n\
                 ALTER TABLE transfers ADD UNIQUE KEY by_from (from_id);";
    on_present(extra, |db| {
        // 順序と大文字小文字は問わない
        assert!(unique_on("transfers", &["from_id", "to_id"]).holds(db).ok());
        assert!(unique_on("transfers", &["FROM_ID", "To_Id"]).holds(db).ok());
        // from_id だけの一意索引があれば、(from_id, amount) の組も一意。not(unique_on) はそれを見落とす
        assert!(not(unique_on("transfers", &["from_id", "amount"])).holds(db).ok());
        assert!(!not(unique_within("transfers", &["from_id", "amount"])).holds(db).ok());
        assert!(not(unique_within("transfers", &["amount", "created_at"])).holds(db).ok());
        // 一意索引は NULL を何行でも許す
        assert!(unique_on("transfers", &["from_id"]).holds(db).ok());
        let v = unique_on_not_null("transfers", &["from_id"]).holds(db);
        assert_eq!(v.problems(), ["transfers.from_id is nullable"]);
        assert!(unique_on_not_null("accounts", &["id"]).holds(db).ok());
    });
}

#[test]
fn a_check_that_is_not_enforced_does_not_count() {
    let fact = check_exists("accounts", "accounts_balance_nonnegative");
    assert!(holds("", &fact).ok());
    let v = holds("ALTER TABLE accounts ALTER CHECK accounts_balance_nonnegative NOT ENFORCED", &fact);
    assert_eq!(v.problems(), ["no enforced check accounts_balance_nonnegative on accounts"]);
}

#[test]
fn a_typo_in_a_column_name_is_an_error_not_a_panic() {
    let typo = Fact::new("typo", |db| match db.query("SELECT 1 AS n") {
        Ok(rows) => match rows[0].try_get("m") {
            Ok(_) => Verdict::pass(),
            Err(e) => Verdict::error(e),
        },
        Err(e) => Verdict::error(e.to_string()),
    });
    assert!(holds("", &typo).is_error());
}
