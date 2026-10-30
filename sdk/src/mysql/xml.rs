//! `mysql --xml` の出力を読む。
//!
//! 文ごとに `<resultset statement="...">` が 1 つ出る（結果を返さない文は何も出さない）。
//! 値はすべて文字列で、NULL は `xsi:nil="true"` で区別される。型の解釈は呼ぶ側に任せる。

/// 1 行。列の順序を保つ。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Row(pub Vec<(String, Option<String>)>);

impl Row {
    /// 列の値（NULL なら None）。
    ///
    /// # Panics
    ///
    /// 列がなければ panic する（クエリの書き間違い。NULL と取り違えないように）。
    pub fn get(&self, column: &str) -> Option<&str> {
        match self.0.iter().find(|(k, _)| k == column) {
            Some((_, v)) => v.as_deref(),
            None => panic!("no column {column} in {self}"),
        }
    }
}

impl std::fmt::Display for Row {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let cells: Vec<String> =
            self.0.iter().map(|(k, v)| format!("{k}={}", v.as_deref().unwrap_or("NULL"))).collect();
        write!(f, "{{{}}}", cells.join(", "))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResultSet {
    pub rows: Vec<Row>,
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.find(';') else { break };
        let entity = &rest[1..end];
        let decoded = match entity {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            e if e.starts_with("#x") => u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `start` の後ろから `name="..."` の値を読み、値の後ろの位置を返す。
fn attribute<'a>(s: &'a str, name: &str) -> Option<(&'a str, usize)> {
    let key = format!("{name}=\"");
    let at = s.find(&key)? + key.len();
    let len = s[at..].find('"')?;
    Some((&s[at..at + len], at + len + 1))
}

fn parse_row(body: &str) -> Row {
    let mut row = Row::default();
    let mut rest = body;
    while let Some(at) = rest.find("<field") {
        rest = &rest[at..];
        let Some((name, after)) = attribute(rest, "name") else { break };
        let Some(close) = rest[after..].find('>') else { break };
        let tag = &rest[after..after + close];
        let content_start = after + close + 1;
        if tag.contains("xsi:nil=\"true\"") {
            row.0.push((unescape(name), None));
            rest = &rest[content_start..];
        } else if tag.ends_with('/') {
            row.0.push((unescape(name), Some(String::new())));
            rest = &rest[content_start..];
        } else {
            let Some(end) = rest[content_start..].find("</field>") else { break };
            row.0.push((unescape(name), Some(unescape(&rest[content_start..content_start + end]))));
            rest = &rest[content_start + end + "</field>".len()..];
        }
    }
    row
}

pub fn parse_result_sets(xml: &str) -> Vec<ResultSet> {
    let mut sets = Vec::new();
    let mut rest = xml;
    while let Some(at) = rest.find("<resultset") {
        rest = &rest[at..];
        let Some((_, after)) = attribute(rest, "statement") else { break };
        let Some(close) = rest[after..].find('>') else { break };
        let body_start = after + close + 1;
        if rest[..body_start - 1].ends_with('/') {
            sets.push(ResultSet { rows: Vec::new() });
            rest = &rest[body_start..];
            continue;
        }
        let end = rest[body_start..].find("</resultset>").map_or(rest.len(), |e| body_start + e);
        let mut rows = Vec::new();
        let mut body = &rest[body_start..end];
        while let Some(r) = body.find("<row>") {
            let Some(e) = body[r..].find("</row>") else { break };
            rows.push(parse_row(&body[r + 5..r + e]));
            body = &body[r + e + 6..];
        }
        sets.push(ResultSet { rows });
        rest = &rest[(end + "</resultset>".len()).min(rest.len())..];
    }
    sets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_rows_nulls_and_entities() {
        let xml = r#"<?xml version="1.0"?>
<resultset statement="SELECT a, b FROM t" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
  <row>
	<field name="a">1 &lt;&amp;&gt; &#x41;</field>
	<field name="b" xsi:nil="true" />
  </row>
  <row>
	<field name="a"></field>
	<field name="b">x</field>
  </row>
</resultset>
<resultset statement="SELECT 1 WHERE 0" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
</resultset>
"#;
        let sets = parse_result_sets(xml);
        assert_eq!(sets.len(), 2);
        assert_eq!(sets[0].rows.len(), 2);
        assert_eq!(sets[0].rows[0].get("a"), Some("1 <&> A"));
        assert_eq!(sets[0].rows[0].get("b"), None);
        assert_eq!(sets[0].rows[1].get("a"), Some(""));
        assert!(sets[1].rows.is_empty());
    }

    #[test]
    #[should_panic(expected = "no column c")]
    fn a_missing_column_is_not_null() {
        let sets = parse_result_sets(r#"<resultset statement="x"><row><field name="a">1</field></row></resultset>"#);
        let _ = sets[0].rows[0].get("c");
    }
}
