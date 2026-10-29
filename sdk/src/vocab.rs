//! 層1: 基礎語彙。information_schema に写る事実と、「違反する行を返す SELECT が空」だけで書ける言明。
//!
//! プロジェクトをまたいで同じ。業務の言葉はここに入れない。業務の言葉は層2（業務寄りのフレームワーク）が
//! [`define`] でこれらの合成として定義する（定義した名前を「語」と呼ぶ）。
//!
//! 言明（[`Fact`]）は、あるデータベースの上で成り立つか（[`Fact::holds`]）と、何を言っているか（[`Fact::says`]）を持つ。
//! 層3（未来の宣言）は語（[`Word`]）しか受け取らない。層1の言明を直接使えないことを型で守る。
use std::fmt;
use std::rc::Rc;

use crate::mysql::{Db, Row, ident, literal};

/// 言明が成り立たなかった理由（成り立てば空）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Verdict(pub Vec<String>);

impl Verdict {
    pub fn pass() -> Self {
        Self(Vec::new())
    }

    pub fn fail(detail: impl Into<String>) -> Self {
        Self(vec![detail.into()])
    }

    pub fn ok(&self) -> bool {
        self.0.is_empty()
    }
}

type Check = dyn Fn(&Db<'_>) -> Verdict;

struct Node {
    says: String,
    /// 合成の材料（語の依存をたどるため）。
    parts: Vec<Fact>,
    /// 語なら（名前, 意味）。
    word: Option<(String, String)>,
    check: Box<Check>,
}

/// 言明。複製は安く、同一性（どの語か）は複製しても保たれる。
#[derive(Clone)]
pub struct Fact(Rc<Node>);

impl Fact {
    /// 新しい言明。層1の言明を足すときと、層2のフレームワークが独自の確かめ方を持つときに使う。
    pub fn new(says: impl Into<String>, check: impl Fn(&Db<'_>) -> Verdict + 'static) -> Self {
        Self(Rc::new(Node { says: says.into(), parts: Vec::new(), word: None, check: Box::new(check) }))
    }

    fn composed(says: String, parts: Vec<Fact>, check: impl Fn(&Db<'_>) -> Verdict + 'static) -> Self {
        Self(Rc::new(Node { says, parts, word: None, check: Box::new(check) }))
    }

    pub fn says(&self) -> &str {
        &self.0.says
    }

    pub fn parts(&self) -> &[Fact] {
        &self.0.parts
    }

    pub fn holds(&self, db: &Db<'_>) -> Verdict {
        (self.0.check)(db)
    }

    /// 語ならその語。
    pub fn as_word(&self) -> Option<Word> {
        self.0.word.as_ref().map(|_| Word(self.clone()))
    }

    /// 自分とその材料にある語（自分を含む）を、浅い順に重複なく返す。
    pub fn words(&self) -> Vec<Word> {
        let mut out: Vec<Word> = Vec::new();
        let mut queue = vec![self.clone()];
        while !queue.is_empty() {
            let mut next = Vec::new();
            for f in queue {
                if let Some(w) = f.as_word()
                    && !out.contains(&w)
                {
                    out.push(w);
                }
                next.extend(f.parts().iter().cloned());
            }
            queue = next;
        }
        out
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
        check: Box::new(move |db| {
            let v = inner.holds(db);
            Verdict(v.0.into_iter().map(|d| format!("{name}: {d}")).collect())
        }),
    })))
}

// ------------------------------------------------------------------------------------------ 道具

fn query(db: &Db<'_>, sql: &str) -> Result<Vec<Row>, Verdict> {
    db.query(sql).map_err(|e| Verdict::fail(format!("query failed: {e}")))
}

fn count(db: &Db<'_>, sql: &str) -> Result<u64, Verdict> {
    Ok(query(db, sql)?.first().and_then(|r| r.get("n")).and_then(|n| n.parse().ok()).unwrap_or(0))
}

fn joined(xs: &[&str]) -> String {
    xs.join(", ")
}

fn owned(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|s| (*s).to_owned()).collect()
}

// ------------------------------------------------------------------------------------- 表と列

pub fn table_exists(table: &str) -> Fact {
    let t = table.to_owned();
    Fact::new(format!("table {table} exists"), move |db| {
        let sql = format!(
            "SELECT COUNT(*) AS n FROM information_schema.TABLES WHERE TABLE_SCHEMA = DATABASE() \
             AND TABLE_TYPE = 'BASE TABLE' AND TABLE_NAME = {}",
            literal(&t)
        );
        match count(db, &sql) {
            Ok(n) if n > 0 => Verdict::pass(),
            Ok(_) => Verdict::fail(format!("no table {t}")),
            Err(v) => v,
        }
    })
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
        Verdict(out)
    })
}

/// 名前が columns のどれか（`email`）か、それで終わる（`to_email`）列が、tables の外にないこと（個人情報の置き場所など）。
pub fn columns_named_only_in(columns: &[&str], tables: &[&str]) -> Fact {
    let (cs, ts) = (owned(columns), owned(tables));
    Fact::new(format!("columns named {} appear only in {}", joined(columns), joined(tables)), move |db| {
        let sql = "SELECT TABLE_NAME AS t, COLUMN_NAME AS c FROM information_schema.COLUMNS \
                   WHERE TABLE_SCHEMA = DATABASE() ORDER BY TABLE_NAME, COLUMN_NAME";
        match query(db, sql) {
            Ok(rows) => Verdict(
                rows.iter()
                    .filter_map(|r| Some((r.get("t")?, r.get("c")?)))
                    .filter(|(t, c)| {
                        cs.iter().any(|x| c == x || c.ends_with(&format!("_{x}"))) && !ts.iter().any(|x| x == t)
                    })
                    .map(|(t, c)| format!("{t}.{c} is outside {}", ts.join(", ")))
                    .collect(),
            ),
            Err(v) => v,
        }
    })
}

// ------------------------------------------------------------------------------- 制約と索引

/// columns（順序どおり）に一意索引（主キーを含む）があること。
pub fn unique_on(table: &str, columns: &[&str]) -> Fact {
    let (t, cs) = (table.to_owned(), columns.join(","));
    Fact::new(format!("{table}({}) is unique", joined(columns)), move |db| {
        let sql = format!(
            "SELECT INDEX_NAME AS i, GROUP_CONCAT(COLUMN_NAME ORDER BY SEQ_IN_INDEX SEPARATOR ',') AS c \
             FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = {} \
             AND NON_UNIQUE = 0 GROUP BY INDEX_NAME",
            literal(&t)
        );
        match query(db, &sql) {
            Ok(rows) if rows.iter().any(|r| r.get("c") == Some(cs.as_str())) => Verdict::pass(),
            Ok(_) => Verdict::fail(format!("no unique index on {t}({})", cs.replace(',', ", "))),
            Err(v) => v,
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
               WHERE k.TABLE_SCHEMA = DATABASE() AND k.REFERENCED_TABLE_NAME IS NOT NULL \
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

/// ref_table を指す外部キーが、どれも削除時に rule で動くこと（指すものがなければ成り立つ）。
pub fn references_to(ref_table: &str, rule: OnDelete) -> Fact {
    let rt = ref_table.to_owned();
    Fact::new(format!("every foreign key into {ref_table} is ON DELETE {}", rule.rule()), move |db| match foreign_keys(
        db,
    ) {
        Ok(fks) => Verdict(
            fks.iter()
                .filter(|k| k.ref_table == rt && k.delete_rule != rule.rule())
                .map(|k| format!("{}({}) → {rt} is ON DELETE {}", k.table, k.columns.replace(',', ", "), k.delete_rule))
                .collect(),
        ),
        Err(v) => v,
    })
}

/// table に name という CHECK 制約があること（未来の差分がその名前で外したり付け替えたりするため）。中身は見ない。
pub fn check_exists(table: &str, name: &str) -> Fact {
    let (t, n) = (table.to_owned(), name.to_owned());
    Fact::new(format!("{table} has check {name}"), move |db| {
        let sql = format!(
            "SELECT COUNT(*) AS n FROM information_schema.TABLE_CONSTRAINTS WHERE CONSTRAINT_SCHEMA = DATABASE() \
             AND TABLE_NAME = {} AND CONSTRAINT_NAME = {} AND CONSTRAINT_TYPE = 'CHECK'",
            literal(&t),
            literal(&n)
        );
        match count(db, &sql) {
            Ok(k) if k > 0 => Verdict::pass(),
            Ok(_) => Verdict::fail(format!("no check {n} on {t}")),
            Err(v) => v,
        }
    })
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
    let t = table.to_owned();
    Fact::new(format!("{} on {table} is guarded by a trigger", event.name()), move |db| {
        let sql = format!(
            "SELECT COUNT(*) AS n FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = DATABASE() \
             AND EVENT_OBJECT_TABLE = {} AND EVENT_MANIPULATION = {} AND ACTION_TIMING = 'BEFORE'",
            literal(&t),
            literal(event.name())
        );
        match count(db, &sql) {
            Ok(k) if k > 0 => Verdict::pass(),
            Ok(_) => Verdict::fail(format!("no BEFORE {} trigger on {t}", event.name())),
            Err(v) => v,
        }
    })
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
/// names を渡すと、ファイルの中の `{{名前}}` を識別子に置き換える。業務寄りのフレームワークが、
/// 同じ SQL ファイルをプロジェクトごとの表名・列名で使うためのもの（SQL はファイルに置いたまま）。
pub fn sql_empty(file: SqlFile, names: Names<'_>) -> Fact {
    let shown = if names.is_empty() {
        file.name.to_owned()
    } else {
        let kv: Vec<String> = names.iter().map(|(k, v)| format!("{k}={v}")).collect();
        format!("{}({})", file.name, kv.join(", "))
    };
    let sql = fill(file.text, names);
    Fact::new(format!("{shown} finds nothing"), move |db| {
        let sql = match &sql {
            Ok(s) => s,
            Err(e) => return Verdict::fail(format!("{}: {e}", file.name)),
        };
        match db.query(sql) {
            Err(e) => Verdict::fail(format!("{} failed: {e}", file.name)),
            Ok(rows) => {
                let mut out: Vec<String> = rows.iter().take(5).map(|r| format!("{}: {r}", file.name)).collect();
                if rows.len() > 5 {
                    out.push(format!("... {} rows", rows.len()));
                }
                Verdict(out)
            }
        }
    })
}

// ------------------------------------------------------------------------------------- 合成

pub fn all(facts: impl IntoIterator<Item = Fact>) -> Fact {
    let facts: Vec<Fact> = facts.into_iter().collect();
    let says = facts.iter().map(Fact::says).collect::<Vec<_>>().join(" and ");
    let inner = facts.clone();
    Fact::composed(says, facts, move |db| Verdict(inner.iter().flat_map(|f| f.holds(db).0).collect()))
}

pub fn any(facts: impl IntoIterator<Item = Fact>) -> Fact {
    let facts: Vec<Fact> = facts.into_iter().collect();
    let says = facts.iter().map(Fact::says).collect::<Vec<_>>().join(" or ");
    let inner = facts.clone();
    Fact::composed(says, facts, move |db| {
        let mut detail = Vec::new();
        for f in &inner {
            let v = f.holds(db);
            if v.ok() {
                return Verdict::pass();
            }
            detail.extend(v.0);
        }
        Verdict(detail)
    })
}

pub fn not(fact: impl Into<Fact>) -> Fact {
    let fact = fact.into();
    let inner = fact.clone();
    Fact::composed(format!("not ({})", fact.says()), vec![fact], move |db| {
        if inner.holds(db).ok() { Verdict::fail(inner.says().to_owned()) } else { Verdict::pass() }
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
}
