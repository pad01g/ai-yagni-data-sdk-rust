//! 対象のプロジェクトの宣言。利用者の crate の bin が組み立てて [`crate::cli::main`] に渡す。
//!
//! - schema:      宣言的なスキーマ（正本。Atlas が当てるもの）。1 ファイル。
//! - fixtures:    代表データ（ファイル名の順に当てる）。「今」は schema ＋ fixtures。
//! - futures:     未来のディレクトリ。`<id>/` を 1 つずつ持ち、宣言は `<id>/future.rs`。
//! - vocabulary:  層2（業務寄りのフレームワークの当てはめ）のファイル。語を使う未来はこれを暗黙に見張る。
//! - words:       語彙。未来と不変条件が使う語は、ここに載っていなければならない（`report` の対応表にもなる）。
//! - invariants:  今、いつも成り立つべきこと（層2の語）。
//! - queries:     固定クエリのディレクトリ（任意）。各ファイルが今のスキーマで PREPARE できることを確かめる。
use std::path::{Path, PathBuf};

use crate::future::Future;
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
    pub futures: Vec<Future>,
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
        for w in words {
            if !self.words.contains(&w) {
                self.words.push(w);
            }
        }
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
