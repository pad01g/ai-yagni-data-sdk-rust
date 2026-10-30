//! 対象のプロジェクトの宣言。利用者の crate の bin が組み立てて [`crate::cli::main`] に渡す。
//!
//! - schema:      宣言的なスキーマ（正本。Atlas が当てるもの）。1 ファイル。
//! - fixtures:    代表データ（ファイル名の順に当てる）。「今」は schema ＋ fixtures。
//! - futures:     未来のディレクトリ。`<id>/` を 1 つずつ持ち、宣言は `<id>/future.rs`。
//! - vocabulary:  層2（業務寄りのフレームワークの当てはめ）のファイル。語を使う未来はこれを暗黙に見張る。
//! - words:       語彙。未来と不変条件が使う語は、ここに載っていなければならない（`report` の対応表にもなる）。
//! - invariants:  今、いつも成り立つべきこと（層2の語）。
//! - queries:     固定クエリのディレクトリ（任意）。各ファイルが今のスキーマで PREPARE できることを確かめる。
//!   各未来の差分を当てた後にも PREPARE し直し、できなくなったクエリはその未来が見張っていなければならない。
//! - grants:      固定クエリを使う DB の利用者の権限（任意。GRANT の文だけのファイル）。あれば、今の上で
//!   その権限だけを持つ使い捨ての利用者を作り、固定クエリをその利用者で PREPARE する（列の権限の不足が分かる）。
//! - accepts / rejects: 今（schema ＋ fixtures）の上の probe（任意）。CHECK 制約やトリガーの中身を、今の上で確かめる。
//!   probe ごとに新しく作った今の複製の上で流し、accepts の後も不変条件が成り立つことを確かめる。
use std::path::{Path, PathBuf};

use crate::future::{Future, Probe, Rejection};
use crate::unique;
use crate::vocab::Word;

#[derive(Debug, Clone)]
pub struct Project {
    /// 表示する名前（既定は root のディレクトリ名）。
    pub name: String,
    pub root: PathBuf,
    pub schema: PathBuf,
    pub fixtures: PathBuf,
    pub futures_dir: PathBuf,
    pub vocabulary: Vec<PathBuf>,
    pub words: Vec<Word>,
    pub invariants: Vec<Word>,
    pub queries: Vec<PathBuf>,
    pub grants: Option<Grants>,
    pub accepts: Vec<Probe>,
    pub rejects: Vec<Rejection>,
    pub futures: Vec<Future>,
}

/// 固定クエリを使う DB の利用者の権限。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grants {
    /// GRANT の文だけのファイル。
    pub file: PathBuf,
    /// ファイルの中の利用者の名前（`'proxy'@'%'` の `proxy`）。使い捨ての利用者の名前に書き換える。
    pub user: String,
    /// ファイルが表を修飾するデータベースの名前（`shop.orders` の `shop`）。使い捨てのデータベースの名前に書き換える。
    /// ファイルに `{{database}}` と書いてもよい（いつも書き換える）。
    pub schema: Option<String>,
}

impl Project {
    pub fn new(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref().to_path_buf();
        Self {
            name: root.file_name().map_or("project".to_owned(), |n| n.to_string_lossy().into_owned()),
            schema: root.join("schema.sql"),
            fixtures: root.join("fixtures"),
            futures_dir: root.join("futures"),
            vocabulary: Vec::new(),
            words: Vec::new(),
            invariants: Vec::new(),
            queries: Vec::new(),
            grants: None,
            accepts: Vec::new(),
            rejects: Vec::new(),
            futures: Vec::new(),
            root,
        }
    }

    pub fn name(mut self, name: &str) -> Self {
        self.name = name.to_owned();
        self
    }

    pub fn schema(mut self, file: &str) -> Self {
        self.schema = self.root.join(file);
        self
    }

    pub fn fixtures(mut self, dir: &str) -> Self {
        self.fixtures = self.root.join(dir);
        self
    }

    pub fn futures_dir(mut self, dir: &str) -> Self {
        self.futures_dir = self.root.join(dir);
        self
    }

    pub fn vocabulary(mut self, file: &str) -> Self {
        self.vocabulary.push(self.root.join(file));
        self
    }

    pub fn words(mut self, words: impl IntoIterator<Item = Word>) -> Self {
        self.words = unique(self.words.into_iter().chain(words));
        self
    }

    pub fn invariants<'a>(mut self, words: impl IntoIterator<Item = &'a Word>) -> Self {
        self.invariants.extend(words.into_iter().cloned());
        self
    }

    pub fn queries(mut self, dir: &str) -> Self {
        self.queries.push(self.root.join(dir));
        self
    }

    /// 固定クエリを使う DB の利用者の権限（file はプロジェクトからの相対パス）。
    /// user はファイルの中の利用者の名前、schema はファイルが表を修飾するデータベースの名前（なければ None）。
    /// `.grants("submodules/proxy/grants.sql", "proxy", Some("shop"))`
    pub fn grants(mut self, file: &str, user: &str, schema: Option<&str>) -> Self {
        self.grants =
            Some(Grants { file: self.root.join(file), user: user.to_owned(), schema: schema.map(str::to_owned) });
        self
    }

    /// 今の上で受け入れる書き込み（file はプロジェクトからの相対パス）。
    pub fn accepts(mut self, file: &str, why: &str) -> Self {
        self.accepts.push(Probe { file: self.root.join(file), why: why.to_owned() });
        self
    }

    /// 今の上で拒む書き込み（期待するエラーの番号か SQLSTATE）。
    pub fn rejects(mut self, file: &str, error: &str, why: &str) -> Self {
        let r = Rejection::new(file, error, None, why);
        self.rejects.push(Rejection { file: self.root.join(&r.file), ..r });
        self
    }

    /// rejects に加えて、エラーの文が naming（制約の名前、SIGNAL の文の一部）を含むこと。
    pub fn rejects_with(mut self, file: &str, error: &str, naming: &str, why: &str) -> Self {
        let r = Rejection::new(file, error, Some(naming), why);
        self.rejects.push(Rejection { file: self.root.join(&r.file), ..r });
        self
    }

    /// 未来を加える。ファイル名は `futures/<id>/` からの相対で解決する。
    pub fn future(mut self, f: Future) -> Self {
        let dir = self.futures_dir.join(&f.id);
        self.futures.push(f.resolve(&dir));
        self
    }

    pub fn find(&self, id: &str) -> Option<&Future> {
        self.futures.iter().find(|f| f.id == id)
    }
}
