//! 検査の本体。本物の MySQL の上で、次を確かめる。
//!
//! 規則:
//! 0. 未来のディレクトリと宣言が 1 対 1 で、日付の形・ファイルの有無・衝突の対称性が正しい。
//!    未来と不変条件の語が語彙に載っている。未来の宣言（`future.rs`）が SQL を文字列で持たず、許された use だけを持つ。
//!
//! 今:
//! 1. schema.sql が空のデータベースに当たる。代表データ（fixtures）が当たり、不変条件（invariants）が成り立つ。
//! 2. 固定クエリ（queries）が、今のスキーマで PREPARE できる。
//!
//! 未来（それぞれ独立に、新しく作った「今」の複製の上で）:
//! 3. 期限が切れていない。実現済み（realized_if）でない。前提（requires）が成り立つ。
//! 4. 差分（change）が当たり、then が成り立ち、accepts を受け入れ、rejects を拒む。
//!
//! 未来どうし:
//! 5. 衝突を宣言した 2 つは、どちらの順で続けて当てても、どこかで失敗する。
//!
//! 共変更（changed を渡したときだけ）:
//! 6. 未来が見張るファイルが変わったのに、その未来のディレクトリが変わっていなければ失敗。
//!    語を使う未来は、語彙のファイル（業務寄りのフレームワークの当てはめ）を暗黙に見張る。
use std::fs;
use std::path::{Path, PathBuf};

use crate::future::Future;
use crate::lint;
use crate::mysql::{Db, Scratch, Server, SqlError, literal, terminate};
use crate::project::Project;
use crate::vocab::Word;

/// 未来の宣言（`futures/<id>/future.rs`）が use してよいもの。
pub const FUTURE_USES: &[&str] = &["ai_yagni_data_sdk::future"];

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// 今日（YYYY-MM-DD）。期限の判定に使う。
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

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).display().to_string()
}

fn facts(db: &Db<'_>, words: &[Word], label: &str) -> Vec<String> {
    words.iter().flat_map(|w| w.holds(db).0).map(|d| format!("{label}: {d}")).collect()
}

fn read(path: &Path) -> Result<String, SqlError> {
    fs::read_to_string(path).map_err(|e| SqlError::other(format!("cannot read {}: {e}", path.display())))
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// YYYY-MM か。
fn is_month(s: &str) -> bool {
    matches!(s.split('-').collect::<Vec<_>>()[..], [y, m] if y.len() == 4 && m.len() == 2 && digits(y) && digits(m))
}

/// YYYY-MM-DD か。
fn is_day(s: &str) -> bool {
    matches!(s.split('-').collect::<Vec<_>>()[..],
        [y, m, d] if y.len() == 4 && m.len() == 2 && d.len() == 2 && digits(y) && digits(m) && digits(d))
}

pub struct Checker<'s> {
    pub server: &'s Server,
    pub project: Project,
}

impl<'s> Checker<'s> {
    pub fn new(server: &'s Server, project: Project) -> Self {
        Self { server, project }
    }

    fn rel(&self, p: &Path) -> String {
        rel(&self.project.root, p)
    }

    /// ファイルを当てる。失敗したら理由を返す。
    fn apply(&self, db: &Db<'_>, file: &Path) -> Result<(), String> {
        read(file).and_then(|sql| db.run(&sql).map(|_| ())).map_err(|e| format!("{}: {e}", self.rel(file)))
    }

    /// 「今」（schema ＋ fixtures）を新しく作る。
    pub fn present(&self, label: &str) -> Result<Scratch<'s>, String> {
        let db = Scratch::create(self.server, label).map_err(|e| e.to_string())?;
        self.apply(&db.db, &self.project.schema)?;
        for f in sql_files(&self.project.fixtures) {
            self.apply(&db.db, &f)?;
        }
        Ok(db)
    }

    pub fn check_rules(&self, opts: &Options) -> Vec<String> {
        let p = &self.project;
        let mut out = Vec::new();
        // 未来のディレクトリと宣言
        let dirs: Vec<String> = fs::read_dir(&p.futures_dir)
            .map(|es| {
                es.flatten()
                    .filter(|e| e.path().is_dir())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        let mut dirs = dirs;
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
            if !is_month(&f.expires) {
                out.push(format!("{}: expires must be YYYY-MM", f.id));
            }
            if !is_day(&f.reviewed) {
                out.push(format!("{}: reviewed must be YYYY-MM-DD", f.id));
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
        out
    }

    pub fn check_present(&self) -> Vec<String> {
        let db = match self.present("present") {
            Ok(db) => db,
            Err(e) => return vec![e],
        };
        let mut out = facts(&db.db, &self.project.invariants, "invariant");
        for dir in &self.project.queries {
            let files = sql_files(dir);
            if files.is_empty() {
                out.push(format!("no queries in {}", self.rel(dir)));
            }
            for q in files {
                let prepared = read(&q).and_then(|sql| {
                    let body = terminate(&sql);
                    let body = body.trim_end().trim_end_matches(';');
                    db.db.run(&format!("PREPARE q FROM {};\nDEALLOCATE PREPARE q", literal(body)))
                });
                if let Err(e) = prepared {
                    out.push(format!("query {} does not prepare: {e}", self.rel(&q)));
                }
            }
        }
        out
    }

    /// 書き込みを巻き戻すトランザクションの中で流す。エラーなら SqlError。
    fn probe(&self, db: &Db<'_>, file: &Path) -> Result<(), SqlError> {
        let sql = read(file)?;
        db.run(&format!("START TRANSACTION;\n{}ROLLBACK", terminate(&sql))).map(|_| ())
    }

    fn probes(&self, db: &Db<'_>, f: &Future) -> Vec<String> {
        let mut out = Vec::new();
        for p in &f.accepts {
            if let Err(e) = self.probe(db, &p.file) {
                out.push(format!("{}: should accept {} ({}), but: {e}", f.id, self.rel(&p.file), p.why));
            }
        }
        for p in &f.rejects {
            match self.probe(db, &p.file) {
                Ok(()) => {
                    out.push(format!("{}: should reject {} ({}), but it was accepted", f.id, self.rel(&p.file), p.why))
                }
                Err(e) if !e.is(&p.error) => out.push(format!(
                    "{}: should reject {} with {}, but got {} ({}): {}",
                    f.id,
                    self.rel(&p.file),
                    p.error,
                    e.code.map_or("?".to_owned(), |c| c.to_string()),
                    e.sqlstate.as_deref().unwrap_or("?"),
                    e.message
                )),
                Err(_) => {}
            }
        }
        out
    }

    pub fn check_future(&self, f: &Future, today: &str) -> Vec<String> {
        let mut out = Vec::new();
        if today.get(..7).is_some_and(|m| m > f.expires.as_str()) {
            out.push(format!("expired in {}: decide again whether to keep this future", f.expires));
        }
        let db = match self.present(&format!("f_{}", f.id)) {
            Ok(db) => db,
            Err(e) => {
                out.push(format!("present: {e}"));
                return out;
            }
        };
        if let Some(w) = &f.realized_if
            && w.holds(&db.db).ok()
        {
            out.push(format!(
                "realized ({}): remove this future and move what it requires into the invariants",
                w.name()
            ));
        }
        out.extend(
            facts(&db.db, &f.requires, "requires").into_iter().map(|x| format!("{x} (the present closes this future)")),
        );
        if let Err(e) = self.apply(&db.db, &f.change) {
            out.push(format!("change does not apply (the present closes this future): {e}"));
            return out;
        }
        out.extend(facts(&db.db, &f.then, "then"));
        out.extend(self.probes(&db.db, f));
        out
    }

    /// a の差分の後に b の差分を当て、両方の then と probe を確かめる。失敗の理由（なければ空）。
    pub fn compose(&self, a: &Future, b: &Future) -> Vec<String> {
        let db = match self.present(&format!("p_{}_{}", a.id, b.id)) {
            Ok(db) => db,
            Err(e) => return vec![e],
        };
        for f in [a, b] {
            if let Err(e) = self.apply(&db.db, &f.change) {
                return vec![e];
            }
        }
        let mut out = Vec::new();
        for f in [a, b] {
            out.extend(facts(&db.db, &f.then, &format!("{} then", f.id)));
            out.extend(self.probes(&db.db, f));
        }
        out
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
                    if self.compose(x, y).is_empty() {
                        out.push(format!(
                            "{} then {}: both apply and all their expectations hold, so the declared conflict ({}) does not show in the data",
                            x.id, y.id, c.because
                        ));
                    }
                }
            }
        }
        out
    }

    /// 共変更。changed は repo からの相対パス。
    pub fn cochange(&self, changed: &[String], repo: &Path) -> Vec<Section> {
        let to_repo = |p: &Path| rel(repo, p);
        let hit = |pattern: &str, file: &str| match pattern.strip_suffix("/*") {
            Some(dir) => file.starts_with(&format!("{dir}/")),
            None => file == pattern,
        };
        self.project
            .futures
            .iter()
            .map(|f| {
                let mut watched: Vec<String> = f.watches.iter().map(|w| to_repo(&self.project.root.join(w))).collect();
                if !f.words().is_empty() {
                    watched.extend(self.project.vocabulary.iter().map(|v| to_repo(v)));
                }
                let mut hits: Vec<&str> =
                    changed.iter().filter(|c| watched.iter().any(|w| hit(w, c))).map(String::as_str).collect();
                hits.dedup();
                let dir = format!("{}/", to_repo(&f.dir));
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
