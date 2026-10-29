//! 層2・層3のソースの規則を確かめる（Rust のソースを字句だけ読む）。
//!
//! 型で守れない規則だけをここで見る。層3が層1の言明を直接使えないことは、[`crate::future::Future`] が
//! [`crate::vocab::Word`] しか受け取らないことで型が守る。
//!
//! - SQL を文字列で書かない。SQL は `.sql` ファイルに置き、`include_str!`（層2）かファイル名（層3）で渡す。
//! - `use` してよいのは、許されたパスで始まるものだけ（層3なら SDK の `future` と、語彙のモジュール）。
use std::fs;
use std::path::Path;

/// 文字列リテラル（行番号つき）と、コメントと文字列を除いたコード。
struct Scanned {
    strings: Vec<(usize, String)>,
    code: String,
}

fn scan(src: &str) -> Scanned {
    let chars: Vec<char> = src.chars().collect();
    let mut strings = Vec::new();
    let mut code = String::new();
    let mut line = 1;
    let mut i = 0;
    let at = |i: usize| chars.get(i).copied().unwrap_or('\0');
    while i < chars.len() {
        let c = chars[i];
        if c == '/' && at(i + 1) == '/' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && at(i + 1) == '*' {
            let mut depth = 0;
            while i < chars.len() {
                if chars[i] == '/' && at(i + 1) == '*' {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*' && at(i + 1) == '/' {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    if chars[i] == '\n' {
                        line += 1;
                        code.push('\n');
                    }
                    i += 1;
                }
            }
            continue;
        }
        // 生文字列: r"..." r#"..."# br"..." cr"..."
        let raw_start = match (c, at(i + 1)) {
            ('r', '"' | '#') => Some(i + 1),
            ('b' | 'c', 'r') if matches!(at(i + 2), '"' | '#') => Some(i + 2),
            _ => None,
        };
        let prev_ident = i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_');
        if let (Some(mut j), false) = (raw_start, prev_ident) {
            let mut hashes = 0;
            while at(j) == '#' {
                hashes += 1;
                j += 1;
            }
            if at(j) == '"' {
                let start_line = line;
                let mut text = String::new();
                j += 1;
                loop {
                    if j >= chars.len() {
                        break;
                    }
                    if chars[j] == '"' && (1..=hashes).all(|k| at(j + k) == '#') {
                        j += 1 + hashes;
                        break;
                    }
                    if chars[j] == '\n' {
                        line += 1;
                        code.push('\n');
                    }
                    text.push(chars[j]);
                    j += 1;
                }
                strings.push((start_line, text));
                code.push_str("\"\"");
                i = j;
                continue;
            }
        }
        if c == '"' || ((c == 'b' || c == 'c') && at(i + 1) == '"' && !prev_ident) {
            let start_line = line;
            let mut j = if c == '"' { i + 1 } else { i + 2 };
            let mut text = String::new();
            while j < chars.len() && chars[j] != '"' {
                if chars[j] == '\\' {
                    text.push(at(j + 1));
                    if at(j + 1) == '\n' {
                        line += 1;
                        code.push('\n');
                    }
                    j += 2;
                    continue;
                }
                if chars[j] == '\n' {
                    line += 1;
                    code.push('\n');
                }
                text.push(chars[j]);
                j += 1;
            }
            strings.push((start_line, text));
            code.push_str("\"\"");
            i = j + 1;
            continue;
        }
        if c == '\'' {
            // 文字リテラル（'a' '\n' '\u{..}'）とライフタイム（'a）を分ける
            if at(i + 1) == '\\' {
                let mut j = i + 2;
                while j < chars.len() && chars[j] != '\'' {
                    j += 1;
                }
                code.push_str("' '");
                i = j + 1;
                continue;
            }
            if at(i + 2) == '\'' {
                code.push_str("' '");
                i += 3;
                continue;
            }
        }
        if c == '\n' {
            line += 1;
        }
        code.push(c);
        i += 1;
    }
    Scanned { strings, code }
}

/// 文字列が SQL の文に見えるか。
pub fn looks_like_sql(text: &str) -> bool {
    let words: Vec<String> = text
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_uppercase)
        .collect();
    let pair = |a: &str, b: &[&str]| words.windows(2).any(|w| w[0] == a && b.contains(&w[1].as_str()));
    let select_from =
        words.iter().position(|w| w == "SELECT").is_some_and(|s| words[s + 1..].iter().any(|w| w == "FROM"));
    let update_set = words.windows(3).any(|w| w[0] == "UPDATE" && w[2] == "SET");
    let ddl = ["ALTER", "CREATE", "DROP", "TRUNCATE"]
        .iter()
        .any(|v| pair(v, &["TABLE", "INDEX", "TRIGGER", "VIEW", "DATABASE"]));
    select_from || pair("INSERT", &["INTO"]) || pair("DELETE", &["FROM"]) || update_set || ddl
}

/// `use a::{b, c::{d, e}};` を `a::b`, `a::c::d`, `a::c::e` に開く。
fn expand(tree: &str) -> Vec<String> {
    let tree: String = tree.chars().filter(|c| !c.is_whitespace()).collect();
    let Some(open) = tree.find('{') else {
        return vec![tree];
    };
    let prefix = &tree[..open];
    let inner = tree[open + 1..].strip_suffix('}').unwrap_or(&tree[open + 1..]);
    let mut items = Vec::new();
    let (mut depth, mut start) = (0, 0);
    for (k, c) in inner.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                items.push(&inner[start..k]);
                start = k + 1;
            }
            _ => {}
        }
    }
    items.push(&inner[start..]);
    items
        .into_iter()
        .filter(|s| !s.is_empty())
        .flat_map(|item| {
            expand(item)
                .into_iter()
                .map(|x| if x == "self" { prefix.trim_end_matches("::").to_owned() } else { format!("{prefix}{x}") })
        })
        .collect()
}

/// コードの中の use の対象（行番号つき）。
fn uses(code: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut line = 1;
    let bytes: Vec<char> = code.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == '\n' {
            line += 1;
        }
        let boundary = i == 0 || !(bytes[i - 1].is_alphanumeric() || bytes[i - 1] == '_');
        let is_use =
            boundary && bytes[i..].starts_with(&['u', 's', 'e']) && bytes.get(i + 3).is_some_and(|c| c.is_whitespace());
        if is_use {
            let start_line = line;
            let mut j = i + 3;
            let mut tree = String::new();
            while j < bytes.len() && bytes[j] != ';' {
                if bytes[j] == '\n' {
                    line += 1;
                }
                tree.push(bytes[j]);
                j += 1;
            }
            let tree = tree.trim();
            let tree = tree.split(" as ").next().unwrap_or(tree);
            out.extend(expand(tree).into_iter().map(|p| (start_line, p)));
            i = j;
            continue;
        }
        i += 1;
    }
    out
}

/// 1 つのソースを確かめる。allowed は use してよいパスの接頭辞（`ai_yagni_data_sdk::future` など）。
pub fn source(src: &str, shown: &str, allowed: &[&str]) -> Vec<String> {
    let s = scan(src);
    let mut out = Vec::new();
    for (line, path) in uses(&s.code) {
        let ok = allowed.iter().any(|a| path == *a || path.starts_with(&format!("{a}::")));
        if !ok {
            out.push(format!("{shown}:{line}: may not use {path}"));
        }
    }
    for (line, text) in &s.strings {
        if looks_like_sql(text) {
            out.push(format!("{shown}:{line}: SQL in a string (put it in a .sql file)"));
        }
    }
    out
}

/// ファイルを確かめる。読めなければそれを問題にする。
pub fn file(path: &Path, shown: &str, allowed: &[&str]) -> Vec<String> {
    match fs::read_to_string(path) {
        Ok(src) => source(&src, shown, allowed),
        Err(e) => vec![format!("{shown}: cannot read: {e}")],
    }
}

/// ディレクトリの下の .rs をすべて確かめる（層2のフレームワーク）。
pub fn dir(root: &Path, allowed: &[&str]) -> Vec<String> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                files.push(p);
            }
        }
    }
    files.sort();
    files
        .iter()
        .flat_map(|p| {
            let shown = p.strip_prefix(root).unwrap_or(p).display().to_string();
            file(p, &shown, allowed)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_sql_in_strings_but_not_in_comments() {
        let src = "// SELECT a FROM b\nlet x = \"select id from accounts\";\nlet y = r#\"DROP TABLE t\"#;\nlet z = 'a';\nlet n = \"name\";\n";
        assert_eq!(
            source(src, "f.rs", &[]),
            vec!["f.rs:2: SQL in a string (put it in a .sql file)", "f.rs:3: SQL in a string (put it in a .sql file)"]
        );
    }

    #[test]
    fn checks_use_trees() {
        let src = "use ai_yagni_data_sdk::{future::Future, vocab::table_exists};\nuse crate::shop::Shop;\nfn f<'a>(x: &'a str) {}\n";
        assert_eq!(
            source(src, "f.rs", &["ai_yagni_data_sdk::future", "crate::shop"]),
            vec!["f.rs:1: may not use ai_yagni_data_sdk::vocab::table_exists"]
        );
    }

    #[test]
    fn sql_shapes() {
        assert!(looks_like_sql("UPDATE accounts SET x = 1"));
        assert!(looks_like_sql("insert into t values (1)"));
        assert!(!looks_like_sql("select the product you want"));
        assert!(!looks_like_sql("注文は顧客を ID で指す"));
    }
}
