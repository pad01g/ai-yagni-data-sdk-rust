//! 層3: 1 つの未来の宣言。
//!
//! 未来は「来るかもしれないデータのあり方」で、今は作らない。代わりに、今のスキーマと代表データ（以下「今」）が
//! その未来へ向かえる状態であり続けることを検査する。
//!
//! - requires:    今、成り立っていなければならない前提。崩れたら、今がその未来を閉じた。
//! - change:      今に当てる差分 SQL（未来へ向かう第一歩。既定は `change.sql`）。当たらなければ、今がその未来を閉じた。
//! - then:        差分を当てた後に成り立つこと。
//! - accepts:     差分を当てた後の DB が受け入れる書き込み（ファイルの SQL を巻き戻すトランザクションの中で流す）。
//! - rejects:     差分を当てた後の DB が拒む書き込み（期待するエラーの番号か SQLSTATE を書く）。
//! - realized_if: 今これが成り立つなら、その未来はもう来ている。未来を消して、前提を今の検査に移すこと（strict xfail と同じ）。
//! - expires:     この月を過ぎたら失敗する。残すか消すかを決め直すこと（todo_or_die と同じ）。
//! - conflicts:   両立しない未来。両方の差分を続けて当てると、どちらの順でも何かが失敗しなければならない。
//! - reviewed:    最後に読み直した日。共変更の検査で、見張っているファイルが変わったのに未来が変わっていなければ失敗する。
//! - watches:     見張るファイル（プロジェクトからの相対パスか、末尾 `/*` のディレクトリ）。
//!   語を使う未来は、語彙（業務寄りのフレームワークの当てはめ）を書かなくても見張る。
//!
//! 未来どうしは矛盾してよい。各未来は、ほかの未来とは独立に「今」へ当てて検査する。
//! 層3は層2の語（[`Word`]）だけを受け取る。SQL は書かず、ファイル名で渡す。
use std::path::{Path, PathBuf};

use crate::vocab::Word;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub file: PathBuf,
    pub why: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    pub file: PathBuf,
    pub why: String,
    /// MySQL のエラー番号（`3819`）か SQLSTATE（`45000`）。
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub with: String,
    pub because: String,
}

#[derive(Debug, Clone)]
pub struct Future {
    pub id: String,
    pub why: String,
    pub reviewed: String,
    pub expires: String,
    pub requires: Vec<Word>,
    pub change: PathBuf,
    pub then: Vec<Word>,
    pub accepts: Vec<Probe>,
    pub rejects: Vec<Rejection>,
    pub realized_if: Option<Word>,
    pub conflicts: Vec<Conflict>,
    pub watches: Vec<String>,
    /// 宣言のあるディレクトリ（プロジェクトに加えたときに `futures/<id>` に決まる）。
    pub dir: PathBuf,
}

impl Future {
    /// ファイル名は、未来のディレクトリ（`futures/<id>/`）からの相対で書く。
    pub fn new(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            why: String::new(),
            reviewed: String::new(),
            expires: String::new(),
            requires: Vec::new(),
            change: PathBuf::from("change.sql"),
            then: Vec::new(),
            accepts: Vec::new(),
            rejects: Vec::new(),
            realized_if: None,
            conflicts: Vec::new(),
            watches: Vec::new(),
            dir: PathBuf::new(),
        }
    }

    pub fn why(mut self, why: &str) -> Self {
        self.why = why.to_owned();
        self
    }

    /// 最後に読み直した日（YYYY-MM-DD）。
    pub fn reviewed(mut self, day: &str) -> Self {
        self.reviewed = day.to_owned();
        self
    }

    /// 決め直す月（YYYY-MM）。
    pub fn expires(mut self, month: &str) -> Self {
        self.expires = month.to_owned();
        self
    }

    pub fn requires<'a>(mut self, words: impl IntoIterator<Item = &'a Word>) -> Self {
        self.requires.extend(words.into_iter().cloned());
        self
    }

    pub fn change(mut self, file: &str) -> Self {
        self.change = PathBuf::from(file);
        self
    }

    pub fn then<'a>(mut self, words: impl IntoIterator<Item = &'a Word>) -> Self {
        self.then.extend(words.into_iter().cloned());
        self
    }

    pub fn accepts(mut self, file: &str, why: &str) -> Self {
        self.accepts.push(Probe { file: PathBuf::from(file), why: why.to_owned() });
        self
    }

    pub fn rejects(mut self, file: &str, error: &str, why: &str) -> Self {
        self.rejects.push(Rejection { file: PathBuf::from(file), why: why.to_owned(), error: error.to_owned() });
        self
    }

    pub fn realized_if(mut self, word: &Word) -> Self {
        self.realized_if = Some(word.clone());
        self
    }

    pub fn conflicts_with(mut self, id: &str, because: &str) -> Self {
        self.conflicts.push(Conflict { with: id.to_owned(), because: because.to_owned() });
        self
    }

    pub fn watches(mut self, path: &str) -> Self {
        self.watches.push(path.to_owned());
        self
    }

    /// ファイル名を dir からの絶対パスにする。
    pub(crate) fn resolve(mut self, dir: &Path) -> Self {
        let at = |p: &Path| if p.is_absolute() { p.to_path_buf() } else { dir.join(p) };
        self.change = at(&self.change);
        for p in &mut self.accepts {
            p.file = at(&p.file);
        }
        for p in &mut self.rejects {
            p.file = at(&p.file);
        }
        self.dir = dir.to_path_buf();
        self
    }

    /// 未来が使う語（材料の語を含む）。
    pub fn words(&self) -> Vec<Word> {
        let mut out: Vec<Word> = Vec::new();
        for w in self.requires.iter().chain(&self.then).chain(&self.realized_if) {
            for x in w.words() {
                if !out.contains(&x) {
                    out.push(x);
                }
            }
        }
        out
    }
}
