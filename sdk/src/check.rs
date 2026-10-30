//! 検査の本体。本物の MySQL の上で、次を確かめる。
//!
//! 規則:
//! 0. 未来のディレクトリと宣言が 1 対 1 で、日付の形と範囲・ファイルの有無・衝突の対称性が正しい。
//!    最後に読み直した日（reviewed）が今日より後でない。
//!    未来と不変条件の語が語彙に載っていて、語に書き間違い（[`crate::vocab::Fact::flaws`]）がない。
//!    未来の宣言（`future.rs`）が SQL を文字列で持たず、許されたパスだけを使う。
//!
//! 今:
//! 1. schema.sql が空のデータベースに当たる。代表データ（fixtures）が当たり、不変条件（invariants）が成り立つ。
//! 2. 固定クエリ（queries）が、今のスキーマで PREPARE できる。
//!
//! 未来（それぞれ独立に、新しく作った「今」の複製の上で）:
//! 3. 期限が切れていない。実現済み（realized_if）でない。前提（requires）が成り立つ。
//! 4. 差分（change）が当たり、then が成り立ち、accepts を受け入れ、rejects を拒む。
//!    probe は 1 つずつ、差分を当てた新しい複製の上で流す（probe の DDL や COMMIT がほかに漏れない）。
//!    accepts を流した後も then が成り立つこと（埋め戻しただけのデータで then が素通りしないように）。
//!
//! 未来どうし:
//! 5. 衝突を宣言した 2 つは、どちらの順で続けて当てても、どこかで失敗する。
//!    確かめられなかったこと（docker の失敗、今が作れない、語が確かめられない）は、衝突が現れたとは数えない。
//!
//! 共変更（changed を渡したときだけ）:
//! 6. 未来が見張るファイルが変わったのに、その未来のディレクトリが変わっていなければ失敗。
//!    語を使う未来は、語彙のファイル（業務寄りのフレームワークの当てはめ）を暗黙に見張る。
//!    プロジェクトの根と未来のディレクトリは、リポジトリの根の下になければならない。
use std::cell::RefCell;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::date;
use crate::future::Future;
use crate::lint;
use crate::mysql::{Db, Scratch, Server, SqlError, literal, single_statement};
use crate::project::Project;
use crate::unique;
use crate::vocab::{Verdict, Word};

/// 未来の宣言（`futures/<id>/future.rs`）が use してよいもの。
pub const FUTURE_USES: &[&str] = &["ai_yagni_data_sdk::future"];

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// 今日（YYYY-MM-DD）。期限と reviewed の判定に使う。
    pub today: String,
    /// 変わったファイル（repo からの相対パス）。渡せば共変更を検査する。
    pub changed: Option<Vec<String>>,
    /// リポジトリの根（changed の基準）。
    pub repo: PathBuf,
    /// 未来の宣言が use してよい語彙のモジュール（`crate::shop` など）。
    pub vocabulary_uses: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub name: String,
    pub problems: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Report {
    pub sections: Vec<Section>,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.sections.iter().all(|s| s.problems.is_empty())
    }

    /// 失敗した節だけ（名前 → 問題）。
    pub fn failures(&self) -> Vec<(String, Vec<String>)> {
        self.sections.iter().filter(|s| !s.problems.is_empty()).map(|s| (s.name.clone(), s.problems.clone())).collect()
    }
}

fn sql_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> =
        entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "sql")).collect();
    files.sort();
    files
}

/// p の base からの相対パス（base の下になければ None）。
fn relative(base: &Path, p: &Path) -> Option<String> {
    p.strip_prefix(base).ok().map(|r| r.display().to_string())
}

/// 絶対パスにして `.` と `..` を字句で畳み、ある祖先までは symlink も解く（ないファイルも扱える）。
fn canonical(p: &Path) -> PathBuf {
    let mut lexical = PathBuf::new();
    for c in std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()).components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                lexical.pop();
            }
            c => lexical.push(c),
        }
    }
    let mut rest = Vec::new();
    let mut base = lexical.as_path();
    loop {
        if let Ok(mut real) = base.canonicalize() {
            real.extend(rest.iter().rev());
            return real;
        }
        match (base.file_name(), base.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name.to_owned());
                base = parent;
            }
            _ => return lexical,
        }
    }
}

/// words がすべて成り立つか。
fn facts(db: &Db<'_>, words: &[Word]) -> Verdict {
    Verdict::merge(words.iter().map(|w| w.holds(db)))
}

/// SQL の失敗を判定にする。サーバーが返したエラーは成り立たない理由、それ以外（docker など）は Error。
fn judged(e: &SqlError, context: &str) -> Verdict {
    let detail = format!("{context}: {e}");
    if e.is_other() { Verdict::error(detail) } else { Verdict::fail(detail) }
}

fn read(path: &Path) -> Result<String, SqlError> {
    fs::read_to_string(path).map_err(|e| SqlError::other(format!("cannot read {}: {e}", path.display())))
}

pub struct Checker<'s> {
    pub server: &'s Server,
    pub project: Project,
    /// 組み合わせ（a の id, b の id）の結果。report と衝突の検査で 1 度だけ計算する。
    composed: RefCell<Vec<((String, String), Verdict)>>,
}

impl<'s> Checker<'s> {
    pub fn new(server: &'s Server, project: Project) -> Self {
        Self { server, project, composed: RefCell::new(Vec::new()) }
    }

    /// 表示のためのパス（プロジェクトの根からの相対。外ならそのまま）。
    fn rel(&self, p: &Path) -> String {
        relative(&self.project.root, p).unwrap_or_else(|| p.display().to_string())
    }

    /// ファイルを当てる。
    fn run_file(&self, db: &Db<'_>, file: &Path) -> Result<(), SqlError> {
        read(file)
            .and_then(|sql| db.run(&sql).map(|_| ()))
            .map_err(|e| SqlError { message: format!("{}: {}", self.rel(file), e.message), ..e })
    }

    /// 「今」（schema ＋ fixtures）を新しく作る。
    pub fn present(&self, label: &str) -> Result<Scratch<'s>, String> {
        let db = Scratch::create(self.server, label).map_err(|e| e.to_string())?;
        self.run_file(&db.db, &self.project.schema).map_err(|e| e.to_string())?;
        for f in sql_files(&self.project.fixtures) {
            self.run_file(&db.db, &f).map_err(|e| e.to_string())?;
        }
        Ok(db)
    }

    /// 新しく作った「今」に、changes の差分を順に当てる。当たらなければその理由（作れなければ Error）。
    fn changed(&self, label: &str, changes: &[&Future]) -> Result<Scratch<'s>, Verdict> {
        let db = self.present(label).map_err(|e| Verdict::error(format!("present: {e}")))?;
        for f in changes {
            if let Err(e) = self.run_file(&db.db, &f.change) {
                return Err(judged(&e, &format!("change of {} does not apply", f.id)));
            }
        }
        Ok(db)
    }

    pub fn check_rules(&self, opts: &Options) -> Vec<String> {
        let p = &self.project;
        let mut out = Vec::new();
        let today = date::day(&opts.today);
        if today.is_none() {
            out.push(format!("today must be a date YYYY-MM-DD: {}", opts.today));
        }
        // 未来のディレクトリと宣言
        let mut dirs: Vec<String> = fs::read_dir(&p.futures_dir)
            .map(|es| {
                es.flatten()
                    .filter(|e| e.path().is_dir())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        dirs.sort();
        for d in &dirs {
            if p.find(d).is_none() {
                out.push(format!("{}/{d}: not declared in the project", self.rel(&p.futures_dir)));
            }
        }
        let mut allowed: Vec<&str> = FUTURE_USES.to_vec();
        allowed.extend(opts.vocabulary_uses.iter().map(String::as_str));
        for f in &p.futures {
            if !dirs.contains(&f.id) {
                out.push(format!("{}: no directory {}", f.id, self.rel(&f.dir)));
                continue;
            }
            let decl = f.dir.join("future.rs");
            if decl.exists() {
                out.extend(lint::file(&decl, &self.rel(&decl), &allowed));
            } else {
                out.push(format!("{}: no future.rs", self.rel(&f.dir)));
            }
            if date::month(&f.expires).is_none() {
                out.push(format!("{}: expires must be YYYY-MM", f.id));
            }
            match (date::day(&f.reviewed), today) {
                (None, _) => out.push(format!("{}: reviewed must be YYYY-MM-DD", f.id)),
                (Some(r), Some(t)) if r > t => {
                    out.push(format!("{}: reviewed {} is after today ({})", f.id, f.reviewed, opts.today));
                }
                _ => {}
            }
            if f.why.is_empty() {
                out.push(format!("{}: say why", f.id));
            }
            if !f.change.exists() {
                out.push(format!("{}: no change file {}", f.id, self.rel(&f.change)));
            }
            for file in f.accepts.iter().map(|x| &x.file).chain(f.rejects.iter().map(|x| &x.file)) {
                if !file.exists() {
                    out.push(format!("{}: no probe {}", f.id, self.rel(file)));
                }
            }
            for w in f.requires.iter().chain(&f.then).chain(&f.realized_if) {
                if !p.words.contains(w) {
                    out.push(format!("{}: '{}' is not a word of the vocabulary", f.id, w.name()));
                }
            }
            for c in &f.conflicts {
                match p.find(&c.with) {
                    None => out.push(format!("{}: conflicts with unknown future '{}'", f.id, c.with)),
                    Some(other) if !other.conflicts.iter().any(|d| d.with == f.id) => {
                        out.push(format!("{}: conflict with {} is not declared on both sides", f.id, c.with));
                    }
                    Some(_) => {}
                }
            }
        }
        for (i, f) in p.futures.iter().enumerate() {
            if p.futures[..i].iter().any(|g| g.id == f.id) {
                out.push(format!("{}: declared twice", f.id));
            }
        }
        for w in &p.invariants {
            if !p.words.contains(w) {
                out.push(format!("invariants: '{}' is not a word of the vocabulary", w.name()));
            }
        }
        // 語の書き間違い（1 つの書き間違いは、それを最初に含む語で 1 度だけ）
        let mut seen = Vec::new();
        for w in &p.words {
            for flaw in w.fact().flaws() {
                if !seen.contains(&flaw) {
                    out.push(format!("word {}: {flaw}", w.name()));
                    seen.push(flaw);
                }
            }
        }
        out
    }

    pub fn check_present(&self) -> Vec<String> {
        let db = match self.present("present") {
            Ok(db) => db,
            Err(e) => return vec![e],
        };
        let mut out: Vec<String> =
            facts(&db.db, &self.project.invariants).problems().into_iter().map(|d| format!("invariant: {d}")).collect();
        for dir in &self.project.queries {
            let files = sql_files(dir);
            if files.is_empty() {
                out.push(format!("no queries in {}", self.rel(dir)));
            }
            for q in files {
                let prepared = read(&q).and_then(|sql| {
                    let body = single_statement(&sql).map_err(SqlError::other)?;
                    db.db.run(&format!("SET @q = {};\nPREPARE q FROM @q;\nDEALLOCATE PREPARE q", literal(&body)))
                });
                if let Err(e) = prepared {
                    out.push(format!("query {} does not prepare: {e}", self.rel(&q)));
                }
            }
        }
        out
    }

    /// f の probe を 1 つずつ、changes を当てた新しい複製の上で流す。
    /// accepts を受け入れた後は、その複製の上で expect（ラベル, 語）が成り立つことも確かめる。
    fn probes(&self, f: &Future, changes: &[&Future], expect: &[(String, &[Word])]) -> Verdict {
        let mut out = Vec::new();
        for p in &f.accepts {
            let shown = self.rel(&p.file);
            let db = match self.changed(&format!("a_{}", f.id), changes) {
                Ok(db) => db,
                Err(v) => {
                    out.push(v.labeled(&format!("{}: before {shown}", f.id)));
                    continue;
                }
            };
            match self.run_file(&db.db, &p.file) {
                Ok(()) => out.extend(
                    expect.iter().map(|(label, words)| facts(&db.db, words).labeled(&format!("{label} after {shown}"))),
                ),
                Err(e) => out.push(judged(&e, &format!("{}: should accept {shown} ({}), but", f.id, p.why))),
            }
        }
        for p in &f.rejects {
            let shown = self.rel(&p.file);
            let db = match self.changed(&format!("r_{}", f.id), changes) {
                Ok(db) => db,
                Err(v) => {
                    out.push(v.labeled(&format!("{}: before {shown}", f.id)));
                    continue;
                }
            };
            out.push(match self.run_file(&db.db, &p.file) {
                Ok(()) => Verdict::fail(format!("{}: should reject {shown} ({}), but it was accepted", f.id, p.why)),
                Err(e) if e.is_other() => judged(&e, &format!("{}: cannot tell whether {shown} is rejected", f.id)),
                Err(e) if !e.is(&p.error) => Verdict::fail(format!(
                    "{}: should reject {shown} with {}, but got {} ({}): {}",
                    f.id,
                    p.error,
                    e.code.map_or("?".to_owned(), |c| c.to_string()),
                    e.sqlstate.as_deref().unwrap_or("?"),
                    e.message
                )),
                Err(_) => Verdict::pass(),
            });
        }
        Verdict::merge(out)
    }

    pub fn check_future(&self, f: &Future, today: &str) -> Vec<String> {
        let mut out = Vec::new();
        if let (Some(t), Some(e)) = (date::day(today), date::month(&f.expires))
            && (t.year, t.month) > e
        {
            out.push(format!("expired in {}: decide again whether to keep this future", f.expires));
        }
        let db = match self.present(&format!("f_{}", f.id)) {
            Ok(db) => db,
            Err(e) => {
                out.push(format!("present: {e}"));
                return out;
            }
        };
        if let Some(w) = &f.realized_if {
            match w.holds(&db.db) {
                Verdict::Pass => out.push(format!(
                    "realized ({}): remove this future and move what it requires into the invariants",
                    w.name()
                )),
                Verdict::Fail(_) => {}
                e @ Verdict::Error(_) => out.extend(e.labeled("realized_if").problems()),
            }
        }
        out.extend(
            facts(&db.db, &f.requires)
                .labeled("requires")
                .problems()
                .into_iter()
                .map(|x| format!("{x} (the present closes this future)")),
        );
        if let Err(e) = self.run_file(&db.db, &f.change) {
            out.extend(judged(&e, "change does not apply (the present closes this future)").problems());
            return out;
        }
        out.extend(facts(&db.db, &f.then).labeled("then").problems());
        out.extend(self.probes(f, &[f], &[("then".to_owned(), &f.then)]).problems());
        out
    }

    /// a の差分の後に b の差分を当て、両方の then と probe を確かめる（accepts の後の then も）。
    /// 同じ組は 1 度だけ計算する。
    pub fn compose(&self, a: &Future, b: &Future) -> Verdict {
        let key = (a.id.clone(), b.id.clone());
        if let Some((_, v)) = self.composed.borrow().iter().find(|(k, _)| *k == key) {
            return v.clone();
        }
        let v = self.compose_now(a, b);
        self.composed.borrow_mut().push((key, v.clone()));
        v
    }

    fn compose_now(&self, a: &Future, b: &Future) -> Verdict {
        let db = match self.changed(&format!("p_{}_{}", a.id, b.id), &[a, b]) {
            Ok(db) => db,
            Err(v) => return v,
        };
        let expect = [(format!("{} then", a.id), a.then.as_slice()), (format!("{} then", b.id), b.then.as_slice())];
        let mut out: Vec<Verdict> = expect.iter().map(|(label, words)| facts(&db.db, words).labeled(label)).collect();
        for f in [a, b] {
            out.push(self.probes(f, &[a, b], &expect));
        }
        Verdict::merge(out)
    }

    pub fn check_conflicts(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen: Vec<(String, String)> = Vec::new();
        for a in &self.project.futures {
            for c in &a.conflicts {
                let Some(b) = self.project.find(&c.with) else { continue };
                let key = if a.id < b.id { (a.id.clone(), b.id.clone()) } else { (b.id.clone(), a.id.clone()) };
                if seen.contains(&key) {
                    continue;
                }
                seen.push(key);
                for (x, y) in [(a, b), (b, a)] {
                    match self.compose(x, y) {
                        Verdict::Pass => out.push(format!(
                            "{} then {}: both apply and all their expectations hold, so the declared conflict ({}) does not show in the data",
                            x.id, y.id, c.because
                        )),
                        Verdict::Fail(_) => {}
                        Verdict::Error(e) => out.push(format!(
                            "{} then {}: cannot tell whether the declared conflict ({}) shows: {e}",
                            x.id, y.id, c.because
                        )),
                    }
                }
            }
        }
        out
    }

    /// 共変更。changed は repo からの相対パス。
    pub fn cochange(&self, changed: &[String], repo: &Path) -> Vec<Section> {
        let repo = canonical(repo);
        let to_repo = |p: &Path| relative(&repo, &canonical(p));
        let p = &self.project;
        let mut outside = Vec::new();
        let watches: Vec<PathBuf> = p.futures.iter().flat_map(|f| f.watches.iter().map(|w| p.root.join(w))).collect();
        let places = [("project root", &p.root), ("futures", &p.futures_dir)]
            .into_iter()
            .chain(p.vocabulary.iter().map(|v| ("vocabulary", v)))
            .chain(watches.iter().map(|w| ("watched file", w)));
        for (what, path) in places {
            if to_repo(path).is_none() {
                outside.push(format!("{what} {} is not under the repository {}", path.display(), repo.display()));
            }
        }
        if !outside.is_empty() {
            return vec![Section { name: "cochange".to_owned(), problems: outside }];
        }
        let changed: Vec<&str> = changed.iter().map(|c| c.trim_start_matches("./")).collect();
        let hit = |pattern: &str, file: &str| match pattern.strip_suffix("/*") {
            Some(dir) => file.starts_with(&format!("{dir}/")),
            None => file == pattern,
        };
        p.futures
            .iter()
            .map(|f| {
                // 末尾の `/*` は、ないファイルの名前として字句のまま残る
                let mut watched: Vec<String> = f.watches.iter().filter_map(|w| to_repo(&p.root.join(w))).collect();
                if !f.words().is_empty() {
                    watched.extend(p.vocabulary.iter().filter_map(|v| to_repo(v)));
                }
                let hits = unique(changed.iter().copied().filter(|c| watched.iter().any(|w| hit(w, c))));
                let dir = format!("{}/", to_repo(&f.dir).unwrap_or_default());
                let touched = changed.iter().any(|c| c.starts_with(&dir));
                Section {
                    name: format!("cochange {}", f.id),
                    problems: if !hits.is_empty() && !touched {
                        vec![format!(
                            "{} changed; re-read this future and bump 'reviewed' (or update it)",
                            hits.join(", ")
                        )]
                    } else {
                        Vec::new()
                    },
                }
            })
            .collect()
    }

    pub fn run(&self, opts: &Options) -> Report {
        let mut sections = vec![
            Section { name: "rules".to_owned(), problems: self.check_rules(opts) },
            Section { name: "present".to_owned(), problems: self.check_present() },
        ];
        for f in &self.project.futures {
            sections.push(Section { name: format!("future {}", f.id), problems: self.check_future(f, &opts.today) });
        }
        sections.push(Section { name: "conflicts".to_owned(), problems: self.check_conflicts() });
        if let Some(changed) = &opts.changed {
            sections.extend(self.cochange(changed, &opts.repo));
        }
        Report { sections }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_paths_fold_dots_and_missing_files() {
        let tmp = canonical(&std::env::temp_dir());
        assert_eq!(canonical(&tmp.join("a/./b/../c.txt")), tmp.join("a/c.txt"));
        assert_eq!(relative(&tmp, &canonical(&tmp.join("x/../y"))), Some("y".to_owned()));
        assert_eq!(relative(&tmp.join("y"), &tmp.join("z")), None);
    }
}
