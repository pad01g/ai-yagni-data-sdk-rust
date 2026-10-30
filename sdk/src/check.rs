//! 検査の本体。本物の MySQL の上で、次を確かめる。
//!
//! 規則:
//! 0. 未来のディレクトリと宣言が 1 対 1 で、日付の形と範囲・ファイルの有無・衝突の対称性が正しい。
//!    最後に読み直した日（reviewed）が今日より後でない。見張るファイル（watches）があり、正しく書かれている。
//!    未来と不変条件の語が語彙に載っていて、語に書き間違い（[`crate::vocab::Fact::flaws`]）がない。
//!    未来のディレクトリの Rust のソースが SQL を文字列で持たず、許されたパスだけを使う。
//!    SQL のファイル（スキーマ・代表データ・差分・probe）が、データベースの外（サーバーの設定・利用者・権限・
//!    ほかのデータベース）を変える文を持たない（検査はデータベースだけを分けるので、ほかの検査に漏れる）。
//!
//! 今:
//! 1. schema.sql が空のデータベースに当たる。代表データ（fixtures）が当たり、不変条件（invariants）が成り立つ。
//! 2. 固定クエリ（queries）が、今のスキーマで PREPARE できる。権限のファイル（grants）があれば、その権限だけを
//!    持つ使い捨ての利用者でも PREPARE できる。
//! 3. 今の probe（accepts と rejects）を、probe ごとに新しく作った今の複製の上で流す。accepts の後も不変条件が成り立つ。
//!
//! 未来（それぞれ独立に、新しく作った「今」の複製の上で）:
//! 4. 期限が切れていない。実現済み（realized_if）でない。前提（requires）が成り立つ。
//! 5. 差分（change）が当たり、then が成り立ち、accepts を受け入れ、rejects を拒む。
//!    probe は 1 つずつ、差分を当てた新しい複製の上で流す（probe の DDL や COMMIT がほかに漏れない）。
//!    accepts を流した後も then が成り立つこと（埋め戻しただけのデータで then が素通りしないように）。
//!    差分を当てた後に PREPARE できなくなった固定クエリは、その未来が見張っていなければならない。
//!
//! 未来どうし:
//! 6. 衝突を宣言した 2 つは、どちらの順で続けて当てても、どこかで失敗する。
//!    確かめられなかったこと（docker の失敗、今が作れない、語が確かめられない）は、衝突が現れたとは数えない。
//!
//! 共変更（changed を渡したときだけ）:
//! 7. 未来が見張るファイルが変わったのに、その未来のディレクトリが変わっていなければ失敗。
//!    語を使う未来は、語彙のファイル（業務寄りのフレームワークの当てはめ）を暗黙に見張る。
//!    変わったパスがディレクトリ（git のサブモジュール）なら、その下の見張るファイルも変わったとみなす。
//!    プロジェクトの根と未来のディレクトリは、リポジトリの根の下になければならない。
//!
//! 複製は、データベースを作ってスクリプトを流すところまでを 1 回の docker exec で作る（[`Scratch::build`]）。
//! probe の複製は、いくつかを並べて作る（言明を確かめるのは 1 つずつ）。
use std::cell::RefCell;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::date;
use crate::future::{Future, Probe, Rejection};
use crate::lint;
use crate::mysql::{
    Db, Scratch, ScratchUser, Server, SqlError, rewrite_grants, server_side, server_wide, single_statement, statements,
};
use crate::project::Project;
use crate::unique;
use crate::vocab::{Verdict, Word};

/// 未来の宣言（`futures/<id>/future.rs`）が use してよいもの。
pub const FUTURE_USES: &[&str] = &["ai_yagni_data_sdk::future"];

/// probe の複製を並べて作る数。
const PARALLEL: usize = 4;

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// 今日（YYYY-MM-DD）。期限と reviewed の判定に使う。
    pub today: String,
    /// 変わったファイル（repo からの相対パス）。渡せば共変更を検査する。
    /// `git diff --name-only --no-renames` で作る（`--no-renames` がないと、名前を変えたファイルの元の名前が出ない）。
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
    /// 失敗にはしないが、読んで直すべきこと（多くのエラーが共有する SQLSTATE `HY000` を期待する rejects など）。
    pub warnings: Vec<String>,
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

/// ディレクトリの下の .rs（symlink の輪は 1 度だけたどる）。
fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let (mut files, mut seen, mut stack) = (Vec::new(), Vec::new(), vec![dir.to_path_buf()]);
    while let Some(d) = stack.pop() {
        let real = d.canonicalize().unwrap_or_else(|_| d.clone());
        if seen.contains(&real) {
            continue;
        }
        seen.push(real);
        let Ok(entries) = fs::read_dir(&d) else { continue };
        for p in entries.flatten().map(|e| e.path()) {
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                files.push(p);
            }
        }
    }
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

/// 見張り（`a/b.sql` か `dir/*`）が、変わったパス（ファイルか、サブモジュールのようなディレクトリ）に当たるか。
fn watch_hits(watch: &str, changed: &str) -> bool {
    let under = |path: &str, dir: &str| path.starts_with(&format!("{dir}/"));
    match watch.strip_suffix("/*") {
        Some(dir) => under(changed, dir) || dir == changed || under(dir, changed),
        None => watch == changed || under(watch, changed),
    }
}

/// 見張り（プロジェクトからの相対）の書き方の誤り。
fn watch_problem(root: &Path, w: &str) -> Option<String> {
    if w.ends_with('/') {
        return Some(format!("watch '{w}' ends with '/' (write a directory as dir/*)"));
    }
    if let Some(dir) = w.strip_suffix("/*") {
        return (!root.join(dir).is_dir()).then(|| format!("watch '{w}': no directory {dir}"));
    }
    if w.contains('*') {
        return Some(format!("watch '{w}': only a trailing /* is allowed"));
    }
    let p = root.join(w);
    if p.is_dir() {
        Some(format!("watch '{w}' is a directory (write it as {w}/*)"))
    } else if !p.exists() {
        Some(format!("watch '{w}': no such file"))
    } else {
        None
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

/// エラーの前に、どのファイルの何行目かを付ける。
fn located(shown: &str, e: SqlError) -> SqlError {
    let at = e.line.map_or(String::new(), |l| format!(":{l}"));
    SqlError { message: format!("{shown}{at}: {}", e.message), ..e }
}

/// 読んだスクリプト（表示する名前, 中身）。
type Script = (String, String);

/// 作った複製。今が作れなければ Err（その理由）。作れたら、続けて当てるファイルのどれかで止まったなら、その番号とエラー。
type Built<'s> = Result<(Scratch<'s>, Option<(usize, SqlError)>), String>;

/// 今（present）に extra を続けて当てた複製を作る。
fn build<'s>(server: &'s Server, label: &str, present: &[Script], extra: &[Script]) -> Built<'s> {
    let scripts: Vec<&str> = present.iter().chain(extra).map(|(_, sql)| sql.as_str()).collect();
    let (db, failed) = Scratch::build(server, label, &scripts).map_err(|e| e.to_string())?;
    match failed {
        None => Ok((db, None)),
        Some((i, e)) if i < present.len() => Err(located(&present[i].0, e).to_string()),
        Some((i, e)) => {
            let i = i - present.len();
            Ok((db, Some((i, located(&extra[i].0, e)))))
        }
    }
}

/// 複製をいくつも作る（PARALLEL ずつ並べて）。結果は jobs と同じ順。
fn build_all<'s>(server: &'s Server, present: &[Script], jobs: &[(String, Vec<Script>)]) -> Vec<Built<'s>> {
    let mut out = Vec::with_capacity(jobs.len());
    for chunk in jobs.chunks(PARALLEL) {
        std::thread::scope(|s| {
            let handles: Vec<_> =
                chunk.iter().map(|(label, extra)| s.spawn(move || build(server, label, present, extra))).collect();
            out.extend(handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("a builder panicked".to_owned()))));
        });
    }
    out
}

/// 1 つの probe。
enum Job<'a> {
    Accept(&'a Probe),
    Reject(&'a Rejection),
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

    /// ファイルを読む（表示する名前と中身）。
    fn script(&self, file: &Path) -> Result<Script, SqlError> {
        Ok((self.rel(file), read(file)?))
    }

    /// 今（schema ＋ fixtures）のスクリプト。
    fn present_scripts(&self) -> Result<Vec<Script>, String> {
        std::iter::once(self.project.schema.clone())
            .chain(sql_files(&self.project.fixtures))
            .map(|f| self.script(&f).map_err(|e| e.to_string()))
            .collect()
    }

    /// ファイルを当てる。
    fn run_file(&self, db: &Db<'_>, file: &Path) -> Result<(), SqlError> {
        let (shown, sql) = self.script(file)?;
        db.run(&sql).map(|_| ()).map_err(|e| located(&shown, e))
    }

    /// 今に files を続けて当てた複製を作る。
    fn copy(&self, label: &str, files: &[&Path]) -> Built<'s> {
        let present = self.present_scripts()?;
        let extra: Vec<Script> =
            files.iter().map(|f| self.script(f)).collect::<Result<_, _>>().map_err(|e| e.to_string())?;
        build(self.server, label, &present, &extra)
    }

    /// 「今」（schema ＋ fixtures）を新しく作る。
    pub fn present(&self, label: &str) -> Result<Scratch<'s>, String> {
        self.copy(label, &[]).map(|(db, _)| db)
    }

    /// 新しく作った「今」に、changes の差分を順に当てる。当たらなければその理由（作れなければ Error）。
    fn changed(&self, label: &str, changes: &[&Future]) -> Result<Scratch<'s>, Verdict> {
        let files: Vec<&Path> = changes.iter().map(|f| f.change.as_path()).collect();
        match self.copy(label, &files) {
            Err(e) => Err(Verdict::error(format!("present: {e}"))),
            Ok((db, None)) => Ok(db),
            Ok((_, Some((i, e)))) => Err(judged(&e, &format!("change of {} does not apply", changes[i].id))),
        }
    }

    /// SQL のファイルの、データベースの外を変える文。
    fn server_wide(&self, file: &Path) -> Vec<String> {
        let Ok(sql) = fs::read_to_string(file) else { return Vec::new() };
        statements(&sql)
            .iter()
            .enumerate()
            .filter_map(|(n, s)| {
                server_wide(s).map(|what| {
                    format!(
                        "{}: statement {} changes the server, not only this database ({what})",
                        self.rel(file),
                        n + 1
                    )
                })
            })
            .collect()
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
        let mut sql: Vec<PathBuf> = std::iter::once(p.schema.clone()).chain(sql_files(&p.fixtures)).collect();
        for f in &p.futures {
            if !dirs.contains(&f.id) {
                out.push(format!("{}: no directory {}", f.id, self.rel(&f.dir)));
                continue;
            }
            let decl = f.dir.join("future.rs");
            if !decl.exists() {
                out.push(format!("{}: no future.rs", self.rel(&f.dir)));
            }
            // future.rs が `mod x;` で読み込むファイルも同じ規則で見る
            for file in rust_files(&f.dir) {
                out.extend(lint::file(&file, &self.rel(&file), &allowed));
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
            sql.push(f.change.clone());
            sql.extend(f.accepts.iter().map(|x| x.file.clone()).chain(f.rejects.iter().map(|x| x.file.clone())));
            for w in &f.watches {
                if let Some(problem) = watch_problem(&p.root, w) {
                    out.push(format!("{}: {problem}", f.id));
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
        for file in p.accepts.iter().map(|x| &x.file).chain(p.rejects.iter().map(|x| &x.file)) {
            if !file.exists() {
                out.push(format!("present: no probe {}", self.rel(file)));
            }
            sql.push(file.clone());
        }
        if let Some(g) = &p.grants
            && !g.file.exists()
        {
            out.push(format!("grants: no file {}", self.rel(&g.file)));
        }
        // データベースの外を変える文（1 つのファイルは 1 度だけ見る）
        for file in unique(sql) {
            out.extend(self.server_wide(&file));
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

    /// 失敗にはしないが、読んで直すべきこと。
    pub fn warnings(&self) -> Vec<String> {
        let p = &self.project;
        let owners = p.futures.iter().flat_map(|f| f.rejects.iter().map(move |r| (f.id.as_str(), r)));
        owners
            .chain(p.rejects.iter().map(|r| ("present", r)))
            .filter(|(_, r)| r.error.eq_ignore_ascii_case("HY000") && r.naming.is_none())
            .map(|(id, r)| {
                format!(
                    "{id}: rejects {} expects HY000, which many unrelated errors share; write the error number \
                     (or use rejects_with to name the constraint)",
                    self.rel(&r.file)
                )
            })
            .collect()
    }

    /// 固定クエリを PREPARE する（1 回の docker exec で、エラーで止まらずに全部）。
    /// user を渡せばその利用者で。PREPARE できなかったクエリと、その理由を返す。
    fn prepare_queries(&self, db: &Db<'_>, user: Option<(&str, &str)>) -> Result<Vec<(PathBuf, String)>, String> {
        let mut failed = Vec::new();
        let mut sent: Vec<PathBuf> = Vec::new();
        let mut script = String::new();
        for q in self.project.queries.iter().flat_map(|d| sql_files(d)) {
            match read(&q).and_then(|sql| single_statement(&sql).map_err(SqlError::other)) {
                Ok(statement) => {
                    // 1 行に 1 つ（エラーの行番号でどのクエリか分かる）
                    script.push_str(&server_side(&statement, false));
                    script.push('\n');
                    sent.push(q);
                }
                Err(e) => failed.push((q, e.to_string())),
            }
        }
        if sent.is_empty() {
            return Ok(failed);
        }
        let errors = db.server.run_forced(&db.database, &script, user).map_err(|e| e.to_string())?;
        for e in errors {
            match e.line.and_then(|l| sent.get((l as usize).wrapping_sub(1))) {
                Some(q) if !failed.iter().any(|(f, _)| f == q) => failed.push((q.clone(), e.to_string())),
                Some(_) => {}
                None => return Err(format!("cannot tell which query failed: {e}")),
            }
        }
        failed.sort();
        Ok(failed)
    }

    /// 権限のファイルの利用者で、固定クエリを PREPARE する。
    fn check_grants(&self, db: &Db<'_>) -> Vec<String> {
        let Some(g) = &self.project.grants else { return Vec::new() };
        let shown = self.rel(&g.file);
        let user = match ScratchUser::create(self.server) {
            Ok(u) => u,
            Err(e) => return vec![format!("grants {shown}: error: cannot create a user: {e}")],
        };
        let applied = read(&g.file)
            .and_then(|sql| {
                rewrite_grants(&sql, &g.user, g.schema.as_deref(), &user.name, &db.database).map_err(SqlError::other)
            })
            .and_then(|sql| db.run(&sql).map_err(|e| located(&shown, e)));
        if let Err(e) = applied {
            return vec![format!("grants {shown} do not apply: {e}")];
        }
        match self.prepare_queries(db, Some((&user.name, &user.password))) {
            Ok(failed) => failed
                .into_iter()
                .map(|(q, e)| {
                    format!(
                        "query {} does not prepare as {} ({shown}): {}",
                        self.rel(&q),
                        g.user,
                        e.replace(&user.name, &g.user)
                    )
                })
                .collect(),
            Err(e) => vec![format!("grants {shown}: error: {e}")],
        }
    }

    pub fn check_present(&self) -> Vec<String> {
        let db = match self.present("present") {
            Ok(db) => db,
            Err(e) => return vec![e],
        };
        let p = &self.project;
        let mut out: Vec<String> =
            facts(&db.db, &p.invariants).problems().into_iter().map(|d| format!("invariant: {d}")).collect();
        for dir in &p.queries {
            if sql_files(dir).is_empty() {
                out.push(format!("no queries in {}", self.rel(dir)));
            }
        }
        match self.prepare_queries(&db.db, None) {
            Ok(failed) => {
                out.extend(failed.into_iter().map(|(q, e)| format!("query {} does not prepare: {e}", self.rel(&q))));
            }
            Err(e) => out.push(format!("queries: error: {e}")),
        }
        out.extend(self.check_grants(&db.db));
        drop(db);
        let expect = [("invariants".to_owned(), p.invariants.as_slice())];
        out.extend(self.probes("present", &p.accepts, &p.rejects, &[], &expect).problems());
        out
    }

    /// probe を 1 つずつ、changes を当てた新しい複製の上で流す（複製はいくつかを並べて作る）。
    /// accepts を受け入れた後は、その複製の上で expect（ラベル, 語）が成り立つことも確かめる。
    fn probes(
        &self,
        owner: &str,
        accepts: &[Probe],
        rejects: &[Rejection],
        changes: &[&Future],
        expect: &[(String, &[Word])],
    ) -> Verdict {
        let jobs: Vec<Job<'_>> = accepts.iter().map(Job::Accept).chain(rejects.iter().map(Job::Reject)).collect();
        if jobs.is_empty() {
            return Verdict::pass();
        }
        let present = match self.present_scripts() {
            Ok(p) => p,
            Err(e) => return Verdict::error(format!("{owner}: present: {e}")),
        };
        let mut out = Vec::new();
        let mut builds = Vec::new();
        for job in &jobs {
            let (label, file) = match job {
                Job::Accept(p) => (format!("a_{owner}"), &p.file),
                Job::Reject(r) => (format!("r_{owner}"), &r.file),
            };
            let files = changes.iter().map(|f| &f.change).chain([file]);
            match files.map(|f| self.script(f)).collect::<Result<Vec<_>, _>>() {
                Ok(extra) => builds.push((label, extra)),
                Err(e) => return Verdict::error(format!("{owner}: {e}")),
            }
        }
        let built = build_all(self.server, &present, &builds);
        for (job, built) in jobs.iter().zip(built) {
            let shown = self.rel(match job {
                Job::Accept(p) => &p.file,
                Job::Reject(r) => &r.file,
            });
            let (db, failed) = match built {
                Ok(b) => b,
                Err(e) => {
                    out.push(Verdict::error(format!("{owner}: before {shown}: present: {e}")));
                    continue;
                }
            };
            // 差分のどれかで止まった
            if let Some((i, e)) = &failed
                && *i < changes.len()
            {
                let v = judged(e, &format!("change of {} does not apply", changes[*i].id));
                out.push(v.labeled(&format!("{owner}: before {shown}")));
                continue;
            }
            let probe_error = failed.map(|(_, e)| e);
            out.push(match (job, probe_error) {
                (Job::Accept(_), None) => Verdict::merge(
                    expect.iter().map(|(label, words)| facts(&db.db, words).labeled(&format!("{label} after {shown}"))),
                ),
                (Job::Accept(p), Some(e)) => judged(&e, &format!("{owner}: should accept {shown} ({}), but", p.why)),
                (Job::Reject(r), None) => {
                    Verdict::fail(format!("{owner}: should reject {shown} ({}), but it was accepted", r.why))
                }
                (Job::Reject(_), Some(e)) if e.is_other() => {
                    judged(&e, &format!("{owner}: cannot tell whether {shown} is rejected"))
                }
                (Job::Reject(r), Some(e))
                    if !e.is(&r.error) || r.naming.as_ref().is_some_and(|n| !e.message.contains(n.as_str())) =>
                {
                    Verdict::fail(format!(
                        "{owner}: should reject {shown} with {}, but got {} ({}): {}",
                        r.expected(),
                        e.code.map_or("?".to_owned(), |c| c.to_string()),
                        e.sqlstate.as_deref().unwrap_or("?"),
                        e.message
                    ))
                }
                (Job::Reject(_), Some(_)) => Verdict::pass(),
            });
        }
        Verdict::merge(out)
    }

    /// f が見張るファイルか（そのパスか、それを含む `dir/*`）。
    fn watched(&self, f: &Future, file: &Path) -> bool {
        let Some(rel) = relative(&self.project.root, file) else { return false };
        f.watches.iter().any(|w| match w.strip_suffix("/*") {
            Some(dir) => rel.starts_with(&format!("{dir}/")),
            None => *w == rel,
        })
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
        // 差分の前に PREPARE できていた固定クエリ（できなかったものは今の節で報告する）
        let has_queries = self.project.queries.iter().any(|d| !sql_files(d).is_empty());
        let before = if has_queries { Some(self.prepare_queries(&db.db, None)) } else { None };
        if let Err(e) = self.run_file(&db.db, &f.change) {
            out.extend(judged(&e, "change does not apply (the present closes this future)").problems());
            return out;
        }
        out.extend(facts(&db.db, &f.then).labeled("then").problems());
        match (before, has_queries.then(|| self.prepare_queries(&db.db, None))) {
            (Some(Ok(before)), Some(Ok(after))) => {
                for (q, e) in after {
                    if !before.iter().any(|(b, _)| *b == q) && !self.watched(f, &q) {
                        let shown = self.rel(&q);
                        out.push(format!(
                            "breaks the fixed query {shown} but does not watch it (add .watches(\"{shown}\")): {e}"
                        ));
                    }
                }
            }
            (Some(Err(e)), _) | (_, Some(Err(e))) => out.push(format!("error: queries after the change: {e}")),
            _ => {}
        }
        drop(db);
        out.extend(self.probes(&f.id, &f.accepts, &f.rejects, &[f], &[("then".to_owned(), &f.then)]).problems());
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
        drop(db);
        for f in [a, b] {
            out.push(self.probes(&f.id, &f.accepts, &f.rejects, &[a, b], &expect));
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

    /// 共変更。changed は repo からの相対パス（`git diff --name-only --no-renames` の出力）。
    /// 変わったパスがディレクトリ（サブモジュール）なら、その下の見張るファイルも変わったとみなす。
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
        let changed: Vec<&str> = changed.iter().map(|c| c.trim_start_matches("./").trim_end_matches('/')).collect();
        p.futures
            .iter()
            .map(|f| {
                // 末尾の `/*` は、ないファイルの名前として字句のまま残る
                let mut watched: Vec<String> = f.watches.iter().filter_map(|w| to_repo(&p.root.join(w))).collect();
                if !f.words().is_empty() {
                    watched.extend(p.vocabulary.iter().filter_map(|v| to_repo(v)));
                }
                let hits = unique(changed.iter().copied().filter(|c| watched.iter().any(|w| watch_hits(w, c))));
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
        Report { sections, warnings: self.warnings() }
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

    #[test]
    fn a_changed_submodule_hits_the_watches_inside_it() {
        assert!(watch_hits("p/submodules/proxy/queries/a.sql", "p/submodules/proxy"));
        assert!(watch_hits("p/submodules/proxy/queries/*", "p/submodules/proxy"));
        assert!(watch_hits("p/submodules/proxy/*", "p/submodules/proxy"));
        assert!(watch_hits("p/q/*", "p/q/a.sql"));
        assert!(watch_hits("p/q/a.sql", "p/q/a.sql"));
        assert!(!watch_hits("p/q/a.sql", "p/q/b.sql"));
        assert!(!watch_hits("p/submodules/proxy-2/a.sql", "p/submodules/proxy"));
        assert!(!watch_hits("p/q/*", "p/qq/a.sql"));
    }

    #[test]
    fn watches_must_exist_and_name_directories_with_a_star() {
        let root = std::env::temp_dir().join(format!("watch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("q")).unwrap();
        fs::write(root.join("q/a.sql"), "SELECT 1").unwrap();
        assert_eq!(watch_problem(&root, "q/a.sql"), None);
        assert_eq!(watch_problem(&root, "q/*"), None);
        assert!(watch_problem(&root, "q/b.sql").unwrap().contains("no such file"));
        assert!(watch_problem(&root, "q").unwrap().contains("is a directory"));
        assert!(watch_problem(&root, "q/").unwrap().contains("ends with '/'"));
        assert!(watch_problem(&root, "r/*").unwrap().contains("no directory"));
        assert!(watch_problem(&root, "q/*.sql").unwrap().contains("only a trailing"));
        let _ = fs::remove_dir_all(&root);
    }
}
