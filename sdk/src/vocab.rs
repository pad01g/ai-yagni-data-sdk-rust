//! 層1: 基礎語彙。information_schema に写る事実と、「違反する行を返す SELECT が空」だけで書ける言明。
//!
//! プロジェクトをまたいで同じ。業務の言葉はここに入れない。業務の言葉は層2（業務寄りのフレームワーク）が
//! [`define`] でこれらの合成として定義する（定義した名前を「語」と呼ぶ）。
//!
//! 言明（[`Fact`]）は、あるデータベースの上で成り立つか（[`Fact::holds`]）と、何を言っているか（[`Fact::says`]）を持つ。
//! 層3（未来の宣言）は語（[`Word`]）しか受け取らない。層1の言明を直接使えないことを型で守る。
use std::fmt;
use std::rc::Rc;

use crate::mysql::{Db, Row, first_word, ident, literal, single_statement};
use crate::unique;

/// 言明を確かめた結果。
///
/// 確かめられなかったこと（クエリの失敗、docker の失敗、言明の書き間違い）は [`Verdict::Error`] で、
/// 成り立つとも成り立たないとも数えない。[`not`] で裏返しても成り立たない。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Verdict {
    #[default]
    Pass,
    /// 成り立たなかった。その理由。
    Fail(Vec<String>),
    /// 確かめられなかった。その理由。
    Error(String),
}

impl Verdict {
    pub fn pass() -> Self {
        Self::Pass
    }

    pub fn fail(detail: impl Into<String>) -> Self {
        Self::Fail(vec![detail.into()])
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::Error(message.into())
    }

    /// 成り立たない理由の一覧から（空なら成り立つ）。
    pub fn of(details: Vec<String>) -> Self {
        if details.is_empty() { Self::Pass } else { Self::Fail(details) }
    }

    pub fn ok(&self) -> bool {
        matches!(self, Self::Pass)
    }

    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error(_))
    }

    /// 問題として並べる文（成り立てば空。確かめられなければ `error: ` で始まる 1 つ）。
    pub fn problems(&self) -> Vec<String> {
        match self {
            Self::Pass => Vec::new(),
            Self::Fail(details) => details.clone(),
            Self::Error(message) => vec![format!("error: {message}")],
        }
    }

    /// 理由の前に label を付ける。
    pub fn labeled(self, label: &str) -> Self {
        match self {
            Self::Pass => Self::Pass,
            Self::Fail(details) => Self::Fail(details.into_iter().map(|d| format!("{label}: {d}")).collect()),
            Self::Error(message) => Self::Error(format!("{label}: {message}")),
        }
    }

    /// すべてが成り立つか。確かめられないものが 1 つでもあれば Error（理由をつなぐ）、
    /// なければ成り立たない理由をすべて集める。
    pub fn merge(verdicts: impl IntoIterator<Item = Verdict>) -> Self {
        let (mut details, mut errors) = (Vec::new(), Vec::new());
        for v in verdicts {
            match v {
                Self::Pass => {}
                Self::Fail(d) => details.extend(d),
                Self::Error(e) => errors.push(e),
            }
        }
        if errors.is_empty() { Self::of(details) } else { Self::Error(errors.join("; ")) }
    }
}

type Check = dyn Fn(&Db<'_>) -> Verdict;

struct Node {
    says: String,
    /// 合成の材料（語の依存をたどるため）。
    parts: Vec<Fact>,
    /// 語なら（名前, 意味）。
    word: Option<(String, String)>,
    /// 組み立てたときに分かった書き間違い。あれば確かめずに Error を返す。
    flaw: Option<String>,
    check: Box<Check>,
}

/// 言明。複製は安く、同一性（どの語か）は複製しても保たれる。
#[derive(Clone)]
pub struct Fact(Rc<Node>);

impl Fact {
    /// 新しい言明。層1の言明を足すときと、層2のフレームワークが独自の確かめ方を持つときに使う。
    pub fn new(says: impl Into<String>, check: impl Fn(&Db<'_>) -> Verdict + 'static) -> Self {
        Self(Rc::new(Node { says: says.into(), parts: Vec::new(), word: None, flaw: None, check: Box::new(check) }))
    }

    /// 書き間違いのある言明（確かめると Error）。検査は規則の節でも報告する（[`Fact::flaws`]）。
    fn flawed(says: impl Into<String>, flaw: String) -> Self {
        Self(Rc::new(Node {
            says: says.into(),
            parts: Vec::new(),
            word: None,
            flaw: Some(flaw),
            check: Box::new(|_| Verdict::Pass),
        }))
    }

    fn composed(
        says: String,
        parts: Vec<Fact>,
        flaw: Option<String>,
        check: impl Fn(&Db<'_>) -> Verdict + 'static,
    ) -> Self {
        Self(Rc::new(Node { says, parts, word: None, flaw, check: Box::new(check) }))
    }

    pub fn says(&self) -> &str {
        &self.0.says
    }

    pub fn parts(&self) -> &[Fact] {
        &self.0.parts
    }

    pub fn holds(&self, db: &Db<'_>) -> Verdict {
        match &self.0.flaw {
            Some(flaw) => Verdict::error(flaw.clone()),
            None => (self.0.check)(db),
        }
    }

    /// 自分とその材料の書き間違い（確かめる前に分かるもの。`{{名前}}` の不足、SELECT でない SQL、空の合成）。
    pub fn flaws(&self) -> Vec<String> {
        unique(self.0.flaw.iter().cloned().chain(self.parts().iter().flat_map(Fact::flaws)))
    }

    /// 語ならその語。
    pub fn as_word(&self) -> Option<Word> {
        self.0.word.as_ref().map(|_| Word(self.clone()))
    }

    /// 自分とその材料にある語（自分を含む）を、浅い順に重複なく返す。
    pub fn words(&self) -> Vec<Word> {
        let mut found = Vec::new();
        let mut queue = vec![self.clone()];
        while !queue.is_empty() {
            found.extend(queue.iter().filter_map(Fact::as_word));
            queue = queue.iter().flat_map(|f| f.parts().iter().cloned()).collect();
        }
        unique(found)
    }
}

impl fmt::Debug for Fact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fact({})", self.says())
    }
}

/// 層2の語: 業務の言葉に名前を付けた言明。
#[derive(Clone)]
pub struct Word(Fact);

impl Word {
    pub fn name(&self) -> &str {
        &self.0.0.word.as_ref().expect("a word has a name").0
    }

    pub fn meaning(&self) -> &str {
        &self.0.0.word.as_ref().expect("a word has a meaning").1
    }

    pub fn says(&self) -> &str {
        self.0.says()
    }

    pub fn holds(&self, db: &Db<'_>) -> Verdict {
        self.0.holds(db)
    }

    pub fn fact(&self) -> Fact {
        self.0.clone()
    }

    /// 自分と、定義に使った語。
    pub fn words(&self) -> Vec<Word> {
        self.0.words()
    }
}

impl PartialEq for Word {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0.0, &other.0.0)
    }
}

impl Eq for Word {}

impl fmt::Debug for Word {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Word({})", self.name())
    }
}

impl From<Word> for Fact {
    fn from(w: Word) -> Fact {
        w.0
    }
}

impl From<&Word> for Fact {
    fn from(w: &Word) -> Fact {
        w.0.clone()
    }
}

/// 層2: 業務の言葉に名前を付ける。理由には語の名前を前に付ける。
pub fn define(word: &str, meaning: &str, fact: impl Into<Fact>) -> Word {
    let fact = fact.into();
    let name = word.to_owned();
    let inner = fact.clone();
    Word(Fact(Rc::new(Node {
        says: fact.says().to_owned(),
        parts: vec![fact],
        word: Some((word.to_owned(), meaning.to_owned())),
        flaw: None,
        check: Box::new(move |db| inner.holds(db).labeled(&name)),
    })))
}

// ------------------------------------------------------------------------------------------ 道具

fn query(db: &Db<'_>, sql: &str) -> Result<Vec<Row>, Verdict> {
    db.query(sql).map_err(|e| Verdict::error(format!("query failed: {e}")))
}

/// sql（数を `n` という列で 1 行返す）が 0 より大きければ成り立つ言明。
fn exists(says: String, sql: String, missing: String) -> Fact {
    Fact::new(says, move |db| {
        let rows = match query(db, &sql) {
            Ok(rows) => rows,
            Err(v) => return v,
        };
        match rows.first().and_then(|r| r.get("n")).and_then(|n| n.parse::<u64>().ok()) {
            Some(n) if n > 0 => Verdict::pass(),
            Some(_) => Verdict::fail(missing.clone()),
            None => Verdict::error(format!("no count from: {sql}")),
        }
    })
}

fn joined(xs: &[&str]) -> String {
    xs.join(", ")
}

fn owned(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|s| (*s).to_owned()).collect()
}

// ------------------------------------------------------------------------------------- 表と列

pub fn table_exists(table: &str) -> Fact {
    let sql = format!(
        "SELECT COUNT(*) AS n FROM information_schema.TABLES WHERE TABLE_SCHEMA = DATABASE() \
         AND TABLE_TYPE = 'BASE TABLE' AND TABLE_NAME = {}",
        literal(table)
    );
    exists(format!("table {table} exists"), sql, format!("no table {table}"))
}

/// 型の大分類。MySQL の DATA_TYPE をこの分類に写す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Integer,
    Decimal,
    Float,
    String,
    Datetime,
    Date,
    Time,
    Json,
    Binary,
}

impl Kind {
    fn data_types(self) -> &'static [&'static str] {
        match self {
            Kind::Integer => &["tinyint", "smallint", "mediumint", "int", "bigint"],
            Kind::Decimal => &["decimal"],
            Kind::Float => &["float", "double"],
            Kind::String => &["char", "varchar", "tinytext", "text", "mediumtext", "longtext", "enum", "set"],
            Kind::Datetime => &["datetime", "timestamp"],
            Kind::Date => &["date"],
            Kind::Time => &["time"],
            Kind::Json => &["json"],
            Kind::Binary => &["binary", "varbinary", "blob", "tinyblob", "mediumblob", "longblob"],
        }
    }

    fn name(self) -> &'static str {
        match self {
            Kind::Integer => "integer",
            Kind::Decimal => "decimal",
            Kind::Float => "float",
            Kind::String => "string",
            Kind::Datetime => "datetime",
            Kind::Date => "date",
            Kind::Time => "time",
            Kind::Json => "json",
            Kind::Binary => "binary",
        }
    }
}

/// 列の形。書いたものだけを確かめる。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Shape {
    kind: Option<Kind>,
    column_type: Option<String>,
    nullable: Option<bool>,
}

pub fn shape() -> Shape {
    Shape::default()
}

impl Shape {
    pub fn kind(mut self, kind: Kind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// information_schema の COLUMN_TYPE と完全に一致すること（`char(36)` など）。
    pub fn column_type(mut self, t: &str) -> Self {
        self.column_type = Some(t.to_ascii_lowercase());
        self
    }

    pub fn nullable(mut self) -> Self {
        self.nullable = Some(true);
        self
    }

    pub fn not_null(mut self) -> Self {
        self.nullable = Some(false);
        self
    }

    fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(k) = self.kind {
            parts.push(k.name().to_owned());
        }
        if let Some(t) = &self.column_type {
            parts.push(t.clone());
        }
        if let Some(n) = self.nullable {
            parts.push(if n { "null" } else { "not null" }.to_owned());
        }
        parts.join(", ")
    }
}

struct ColumnInfo {
    data_type: String,
    column_type: String,
    nullable: bool,
}

fn column_info(db: &Db<'_>, table: &str, column: &str) -> Result<Option<ColumnInfo>, Verdict> {
    let sql = format!(
        "SELECT DATA_TYPE AS d, COLUMN_TYPE AS t, IS_NULLABLE AS n FROM information_schema.COLUMNS \
         WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = {} AND COLUMN_NAME = {}",
        literal(table),
        literal(column)
    );
    Ok(query(db, &sql)?.first().map(|r| ColumnInfo {
        data_type: r.get("d").unwrap_or("").to_ascii_lowercase(),
        column_type: r.get("t").unwrap_or("").to_ascii_lowercase(),
        nullable: r.get("n") == Some("YES"),
    }))
}

pub fn column_exists(table: &str, column: &str) -> Fact {
    let (t, c) = (table.to_owned(), column.to_owned());
    Fact::new(format!("column {table}.{column} exists"), move |db| match column_info(db, &t, &c) {
        Ok(Some(_)) => Verdict::pass(),
        Ok(None) => Verdict::fail(format!("no column {t}.{c}")),
        Err(v) => v,
    })
}

pub fn column_is(table: &str, column: &str, shape: Shape) -> Fact {
    let (t, c) = (table.to_owned(), column.to_owned());
    Fact::new(format!("column {table}.{column} is {}", shape.describe()), move |db| {
        let info = match column_info(db, &t, &c) {
            Ok(Some(i)) => i,
            Ok(None) => return Verdict::fail(format!("no column {t}.{c}")),
            Err(v) => return v,
        };
        let mut out = Vec::new();
        if let Some(k) = shape.kind
            && !k.data_types().contains(&info.data_type.as_str())
        {
            out.push(format!("{t}.{c} is {}, not {}", info.column_type, k.name()));
        }
        if let Some(want) = &shape.column_type
            && &info.column_type != want
        {
            out.push(format!("{t}.{c} is {}, not {want}", info.column_type));
        }
        if let Some(n) = shape.nullable
            && info.nullable != n
        {
            out.push(format!("{t}.{c} is {}", if info.nullable { "nullable" } else { "not null" }));
        }
        Verdict::of(out)
    })
}

/// 名前が columns のどれか（`email`）か、それで終わる（`to_email`）列が、tables の外にないこと（個人情報の置き場所など）。
/// 名前は大文字と小文字を区別しない（MySQL の列名と同じ）。
pub fn columns_named_only_in(columns: &[&str], tables: &[&str]) -> Fact {
    let lower = |xs: &[&str]| -> Vec<String> { xs.iter().map(|s| s.to_lowercase()).collect() };
    let (cs, ts, shown) = (lower(columns), lower(tables), joined(tables));
    Fact::new(format!("columns named {} appear only in {}", joined(columns), joined(tables)), move |db| {
        let sql = "SELECT TABLE_NAME AS t, COLUMN_NAME AS c FROM information_schema.COLUMNS \
                   WHERE TABLE_SCHEMA = DATABASE() ORDER BY TABLE_NAME, COLUMN_NAME";
        match query(db, sql) {
            Ok(rows) => Verdict::of(
                rows.iter()
                    .filter_map(|r| Some((r.get("t")?, r.get("c")?)))
                    .filter(|(t, c)| {
                        let (t, c) = (t.to_lowercase(), c.to_lowercase());
                        cs.iter().any(|x| c == *x || c.ends_with(&format!("_{x}"))) && !ts.contains(&t)
                    })
                    .map(|(t, c)| format!("{t}.{c} is outside {shown}"))
                    .collect(),
            ),
            Err(v) => v,
        }
    })
}

// ------------------------------------------------------------------------------- 制約と索引

/// columns（順序どおり）にちょうど一致する一意索引（主キーを含む）があること。
/// 式の部分（関数索引）や列の先頭だけ（`email(10)`）を含む索引、ほかの列も含む索引は数えない。
pub fn unique_on(table: &str, columns: &[&str]) -> Fact {
    let (t, cs) = (table.to_owned(), owned(columns));
    Fact::new(format!("{table}({}) is unique", joined(columns)), move |db| {
        let sql = format!(
            "SELECT INDEX_NAME AS i, COLUMN_NAME AS c, SUB_PART AS p \
             FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = {} \
             AND NON_UNIQUE = 0 ORDER BY INDEX_NAME, SEQ_IN_INDEX",
            literal(&t)
        );
        let rows = match query(db, &sql) {
            Ok(rows) => rows,
            Err(v) => return v,
        };
        // 索引ごとの部分（列の部分だけ Some(列名)）
        let mut indexes: Vec<(&str, Vec<Option<&str>>)> = Vec::new();
        for r in &rows {
            let part = if r.get("p").is_some() { None } else { r.get("c") };
            match indexes.last_mut() {
                Some((name, parts)) if Some(*name) == r.get("i") => parts.push(part),
                _ => indexes.push((r.get("i").unwrap_or(""), vec![part])),
            }
        }
        let want: Vec<Option<&str>> = cs.iter().map(|c| Some(c.as_str())).collect();
        if indexes.iter().any(|(_, parts)| *parts == want) {
            Verdict::pass()
        } else {
            Verdict::fail(format!("no unique index on exactly {t}({})", cs.join(", ")))
        }
    })
}

/// 外部キーの削除時の動き。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnDelete {
    Restrict,
    Cascade,
    SetNull,
    NoAction,
}

impl OnDelete {
    fn rule(self) -> &'static str {
        match self {
            OnDelete::Restrict => "RESTRICT",
            OnDelete::Cascade => "CASCADE",
            OnDelete::SetNull => "SET NULL",
            OnDelete::NoAction => "NO ACTION",
        }
    }
}

struct ForeignKey {
    table: String,
    columns: String,
    ref_table: String,
    ref_columns: String,
    delete_rule: String,
}

fn foreign_keys(db: &Db<'_>) -> Result<Vec<ForeignKey>, Verdict> {
    let sql = "SELECT k.TABLE_NAME AS t, GROUP_CONCAT(k.COLUMN_NAME ORDER BY k.ORDINAL_POSITION SEPARATOR ',') AS c, \
               k.REFERENCED_TABLE_NAME AS rt, \
               GROUP_CONCAT(k.REFERENCED_COLUMN_NAME ORDER BY k.ORDINAL_POSITION SEPARATOR ',') AS rc, \
               r.DELETE_RULE AS d FROM information_schema.KEY_COLUMN_USAGE k \
               JOIN information_schema.REFERENTIAL_CONSTRAINTS r ON r.CONSTRAINT_SCHEMA = k.TABLE_SCHEMA \
               AND r.CONSTRAINT_NAME = k.CONSTRAINT_NAME AND r.TABLE_NAME = k.TABLE_NAME \
               WHERE k.TABLE_SCHEMA = DATABASE() AND k.REFERENCED_TABLE_SCHEMA = DATABASE() \
               AND k.REFERENCED_TABLE_NAME IS NOT NULL \
               GROUP BY k.TABLE_NAME, k.CONSTRAINT_NAME, k.REFERENCED_TABLE_NAME, r.DELETE_RULE \
               ORDER BY k.TABLE_NAME, k.CONSTRAINT_NAME";
    Ok(query(db, sql)?
        .iter()
        .map(|r| ForeignKey {
            table: r.get("t").unwrap_or("").to_owned(),
            columns: r.get("c").unwrap_or("").to_owned(),
            ref_table: r.get("rt").unwrap_or("").to_owned(),
            ref_columns: r.get("rc").unwrap_or("").to_owned(),
            delete_rule: r.get("d").unwrap_or("").to_owned(),
        })
        .collect())
}

/// 同じデータベースの中の外部キー（ほかのデータベースの表を指すものは数えない）。
pub fn foreign_key(table: &str, columns: &[&str], ref_table: &str, ref_columns: &[&str]) -> Fact {
    let says = format!("{table}({}) references {ref_table}({})", joined(columns), joined(ref_columns));
    let (t, c, rt, rc) = (table.to_owned(), columns.join(","), ref_table.to_owned(), ref_columns.join(","));
    let shown = says.clone();
    Fact::new(says, move |db| match foreign_keys(db) {
        Ok(fks) if fks.iter().any(|k| k.table == t && k.columns == c && k.ref_table == rt && k.ref_columns == rc) => {
            Verdict::pass()
        }
        Ok(_) => Verdict::fail(format!("no foreign key {shown}")),
        Err(v) => v,
    })
}

/// ref_table を指す外部キーが、どれも削除時に rules のどれかで動くこと（指すものがなければ成り立つ）。
/// MySQL は ON DELETE を書かない外部キーを NO ACTION（InnoDB では RESTRICT と同じ）と報告する。
pub fn references_to(ref_table: &str, rules: &[OnDelete]) -> Fact {
    let rt = ref_table.to_owned();
    let names: Vec<&'static str> = rules.iter().map(|r| r.rule()).collect();
    let says = format!("every foreign key into {ref_table} is ON DELETE {}", names.join(" or "));
    Fact::new(says, move |db| match foreign_keys(db) {
        Ok(fks) => Verdict::of(
            fks.iter()
                .filter(|k| k.ref_table == rt && !names.contains(&k.delete_rule.as_str()))
                .map(|k| format!("{}({}) → {rt} is ON DELETE {}", k.table, k.columns.replace(',', ", "), k.delete_rule))
                .collect(),
        ),
        Err(v) => v,
    })
}

/// table に name という CHECK 制約があること（未来の差分がその名前で外したり付け替えたりするため）。中身は見ない。
pub fn check_exists(table: &str, name: &str) -> Fact {
    let sql = format!(
        "SELECT COUNT(*) AS n FROM information_schema.TABLE_CONSTRAINTS WHERE CONSTRAINT_SCHEMA = DATABASE() \
         AND TABLE_NAME = {} AND CONSTRAINT_NAME = {} AND CONSTRAINT_TYPE = 'CHECK'",
        literal(table),
        literal(name)
    );
    exists(format!("{table} has check {name}"), sql, format!("no check {name} on {table}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Insert,
    Update,
    Delete,
}

impl Event {
    fn name(self) -> &'static str {
        match self {
            Event::Insert => "INSERT",
            Event::Update => "UPDATE",
            Event::Delete => "DELETE",
        }
    }
}

/// table に event の前に走るトリガーがあること（追記のみの表の守りなど）。中身は見ない。
pub fn guarded_against(table: &str, event: Event) -> Fact {
    let sql = format!(
        "SELECT COUNT(*) AS n FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = DATABASE() \
         AND EVENT_OBJECT_TABLE = {} AND EVENT_MANIPULATION = {} AND ACTION_TIMING = 'BEFORE'",
        literal(table),
        literal(event.name())
    );
    exists(
        format!("{} on {table} is guarded by a trigger", event.name()),
        sql,
        format!("no BEFORE {} trigger on {table}", event.name()),
    )
}

// ----------------------------------------------------------------------------------- データ

/// SQL のファイル。層2は SQL を文字列で書かず、`.sql` ファイルを `include_str!` で埋め込んで渡す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SqlFile {
    pub name: &'static str,
    pub text: &'static str,
}

impl SqlFile {
    /// `SqlFile::new("x.sql", include_str!("x.sql"))`
    pub const fn new(name: &'static str, text: &'static str) -> Self {
        Self { name, text }
    }
}

/// 表名・列名の差し込み（SQL ファイルの中の `{{名前}}` を識別子で置き換える）。
pub type Names<'a> = &'a [(&'a str, &'a str)];

/// `{{名前}}` を names の識別子（`` `名前` ``）で、`{{'名前'}}` を同じ値の文字列リテラル（information_schema と比べるとき）で
/// 置き換える。識別子でない値と、names にない名前は拒む。
pub fn fill(sql: &str, names: Names<'_>) -> Result<String, String> {
    let mut out = String::with_capacity(sql.len());
    let mut rest = sql;
    while let Some(at) = rest.find("{{") {
        out.push_str(&rest[..at]);
        let Some(end) = rest[at..].find("}}") else {
            return Err("unclosed {{".to_owned());
        };
        let inner = rest[at + 2..at + end].trim();
        let (quoted, key) = match inner.strip_prefix('\'').and_then(|k| k.strip_suffix('\'')) {
            Some(k) => (true, k),
            None => (false, inner),
        };
        let value =
            names.iter().find(|(k, _)| *k == key).map(|(_, v)| *v).ok_or(format!("no name for {{{{{key}}}}}"))?;
        let quoted_ident = ident(value).map_err(|_| format!("not an identifier for {{{{{key}}}}}: {value}"))?;
        out.push_str(&if quoted { format!("'{value}'") } else { quoted_ident });
        rest = &rest[at + end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// file の SELECT が 1 行も返さないこと。SELECT は「違反している行」を返すように書く。
/// 行を返したら、その先頭の数行を理由にする。
///
/// file はちょうど 1 つの SELECT（`WITH` で始まってもよい）でなければならない。読み取りだけのトランザクションの中で流し、
/// 巻き戻す（書き込みはエラーになる）。SELECT が失敗したら（表がないなど）成り立たないのでなく Error。
///
/// names を渡すと、ファイルの中の `{{名前}}` を識別子に置き換える。業務寄りのフレームワークが、
/// 同じ SQL ファイルをプロジェクトごとの表名・列名で使うためのもの（SQL はファイルに置いたまま）。
/// 置き換えられない名前や SELECT でない文は、組み立てたときに分かる書き間違い（[`Fact::flaws`]）。
pub fn sql_empty(file: SqlFile, names: Names<'_>) -> Fact {
    let shown = if names.is_empty() {
        file.name.to_owned()
    } else {
        let kv: Vec<String> = names.iter().map(|(k, v)| format!("{k}={v}")).collect();
        format!("{}({})", file.name, kv.join(", "))
    };
    let says = format!("{shown} finds nothing");
    let select = fill(file.text, names).and_then(|sql| {
        let statement = single_statement(&sql)?;
        match first_word(&statement).as_str() {
            "SELECT" | "WITH" | "(" => Ok(statement),
            w => Err(format!("must be a single SELECT, not {w}")),
        }
    });
    let select = match select {
        Ok(s) => s,
        Err(e) => return Fact::flawed(says, format!("{}: {e}", file.name)),
    };
    let script = format!("START TRANSACTION READ ONLY;\n{select}\n;\nROLLBACK");
    Fact::new(says, move |db| match db.query(&script) {
        Err(e) => Verdict::error(format!("{} failed: {e}", file.name)),
        Ok(rows) => {
            let mut out: Vec<String> = rows.iter().take(5).map(|r| format!("{}: {r}", file.name)).collect();
            if rows.len() > 5 {
                out.push(format!("... {} rows", rows.len()));
            }
            Verdict::of(out)
        }
    })
}

// ------------------------------------------------------------------------------------- 合成
//
// 空の all と any は書き間違いとみなす（何も確かめない語が、黙って成り立ったり裏返されたりしないように）。
// どれかが Error なら、合成も Error（ほかが成り立っても、成り立たなくても）。

fn nothing(facts: &[Fact], what: &str) -> Option<String> {
    facts.is_empty().then(|| format!("{what} of nothing"))
}

/// すべてが成り立つこと。
pub fn all(facts: impl IntoIterator<Item = Fact>) -> Fact {
    let facts: Vec<Fact> = facts.into_iter().collect();
    let says = facts.iter().map(Fact::says).collect::<Vec<_>>().join(" and ");
    let flaw = nothing(&facts, "all");
    let inner = facts.clone();
    Fact::composed(says, facts, flaw, move |db| Verdict::merge(inner.iter().map(|f| f.holds(db))))
}

/// どれかが成り立つこと（どれも確かめられたうえで）。
pub fn any(facts: impl IntoIterator<Item = Fact>) -> Fact {
    let facts: Vec<Fact> = facts.into_iter().collect();
    let says = facts.iter().map(Fact::says).collect::<Vec<_>>().join(" or ");
    let flaw = nothing(&facts, "any").map(|f| format!("{f} (no alternatives)"));
    let inner = facts.clone();
    Fact::composed(says, facts, flaw, move |db| {
        let verdicts: Vec<Verdict> = inner.iter().map(|f| f.holds(db)).collect();
        if verdicts.iter().any(Verdict::is_error) {
            Verdict::merge(verdicts.into_iter().filter(Verdict::is_error))
        } else if verdicts.iter().any(Verdict::ok) {
            Verdict::pass()
        } else {
            Verdict::merge(verdicts)
        }
    })
}

/// fact が成り立たないこと。確かめられなければ（Error）、裏返さずに Error のまま。
pub fn not(fact: impl Into<Fact>) -> Fact {
    let fact = fact.into();
    let inner = fact.clone();
    Fact::composed(format!("not ({})", fact.says()), vec![fact], None, move |db| match inner.holds(db) {
        Verdict::Pass => Verdict::fail(inner.says().to_owned()),
        Verdict::Fail(_) => Verdict::pass(),
        e @ Verdict::Error(_) => e,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_substitutes_identifiers_and_literals() {
        let names = [("t", "orders"), ("c", "total")];
        assert_eq!(
            fill("SELECT {{c}} FROM {{ t }} WHERE x = {{'t'}}", &names).unwrap(),
            "SELECT `total` FROM `orders` WHERE x = 'orders'"
        );
        assert!(fill("{{nope}}", &names).is_err());
        assert!(fill("{{t}}", &[("t", "a b")]).is_err());
    }

    #[test]
    fn words_are_found_through_compositions() {
        let a = define("a", "A", table_exists("x"));
        let b = define("b", "B", all([a.fact(), not(table_exists("y"))]));
        assert_eq!(b.words(), vec![b.clone(), a.clone()]);
        assert_ne!(a, define("a", "A", table_exists("x")));
        assert_eq!(b.says(), "table x exists and not (table y exists)");
    }

    #[test]
    fn merging_keeps_errors_apart_from_failures() {
        let e = Verdict::error("docker died");
        assert_eq!(
            Verdict::merge([Verdict::pass(), Verdict::fail("a"), Verdict::fail("b")]),
            Verdict::Fail(vec!["a".into(), "b".into()])
        );
        assert_eq!(Verdict::merge([Verdict::fail("a"), e.clone()]), e);
        assert_eq!(Verdict::merge([]), Verdict::Pass);
        assert_eq!(e.clone().labeled("w").problems(), ["error: w: docker died"]);
        assert!(!e.ok() && e.is_error());
    }

    /// 確かめずに決まった結果を返す言明（Db を使わない）。
    fn fixed(v: Verdict) -> Fact {
        Fact::new(format!("{v:?}"), move |_| v.clone())
    }

    fn holds_without_db(f: &Fact) -> Verdict {
        let server = crate::mysql::Server::attach("unused");
        let db = Db { server: &server, database: "unused".to_owned() };
        f.holds(&db)
    }

    #[test]
    fn not_does_not_turn_an_error_into_a_pass() {
        let e = Verdict::error("query failed");
        assert_eq!(holds_without_db(&not(fixed(e.clone()))), e);
        assert!(holds_without_db(&not(fixed(Verdict::fail("x")))).ok());
        assert!(!holds_without_db(&not(fixed(Verdict::pass()))).ok());
    }

    #[test]
    fn any_and_all_treat_an_error_as_an_error() {
        let e = Verdict::error("query failed");
        assert_eq!(holds_without_db(&any([fixed(Verdict::pass()), fixed(e.clone())])), e);
        assert_eq!(holds_without_db(&any([fixed(e.clone()), fixed(Verdict::pass())])), e);
        assert_eq!(holds_without_db(&all([fixed(Verdict::fail("x")), fixed(e.clone())])), e);
        assert!(holds_without_db(&any([fixed(Verdict::fail("x")), fixed(Verdict::pass())])).ok());
        assert_eq!(
            holds_without_db(&any([fixed(Verdict::fail("x")), fixed(Verdict::fail("y"))])).problems(),
            ["x", "y"]
        );
    }

    #[test]
    fn empty_compositions_are_flaws() {
        let none = any([]);
        assert!(holds_without_db(&none).is_error());
        assert_eq!(none.flaws(), ["any of nothing (no alternatives)"]);
        assert!(holds_without_db(&not(any([]))).is_error());
        assert!(holds_without_db(&all([])).is_error());
        assert_eq!(define("w", "W", all([table_exists("t"), all([])])).fact().flaws(), ["all of nothing"]);
    }

    #[test]
    fn sql_files_must_be_one_select_with_every_name() {
        const TWO: SqlFile = SqlFile::new("two.sql", "SELECT 1;\nDELETE FROM t;\n");
        const DDL: SqlFile = SqlFile::new("ddl.sql", "-- 違反\nDROP TABLE t");
        const NAMED: SqlFile = SqlFile::new("named.sql", "SELECT id FROM {{t}} -- 末尾のコメント;\n");
        assert_eq!(sql_empty(TWO, &[]).flaws(), ["two.sql: 2 statements (write exactly one)"]);
        assert_eq!(sql_empty(DDL, &[]).flaws(), ["ddl.sql: must be a single SELECT, not DROP"]);
        assert_eq!(sql_empty(NAMED, &[]).flaws(), ["named.sql: no name for {{t}}"]);
        assert_eq!(sql_empty(NAMED, &[("t", "orders")]).flaws(), Vec::<String>::new());
        assert!(holds_without_db(&sql_empty(TWO, &[])).is_error());
    }
}
