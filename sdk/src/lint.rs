//! 層2・層3のソースの規則を確かめる（Rust のソースを字句だけ読む）。
//!
//! 型で守れない規則だけをここで見る。層3が層1の言明を直接使えないことは、[`crate::future::Future`] が
//! [`crate::vocab::Word`] しか受け取らないことで型が守る。
//!
//! - SQL を文字列で書かない。SQL は `.sql` ファイルに置き、`include_str!`（層2）かファイル名（層3）で渡す。
//! - `use` してよいのは、許されたパスで始まるものだけ（層3なら SDK の `future` と、語彙のモジュール）。
//!   `use` を書かずにコードの中でパスを書き切る（`ai_yagni_data_sdk::vocab::table_exists(..)`）のも同じ規則で見る。
//!   コードの中のパスは、許されたパスの先頭（crate の名前）か `crate`・`std`・`core`・`alloc`・SDK で始まるものだけを見る
//!   （`Future::new` のような、スコープにある型から始まるパスと区別できないため）。
//!   先頭の `::`（`::ai_yagni_data_sdk::future`）は落として読む。
//!   `extern crate a as b;` の別名 `b` は `a` と読み替える。
//! - `super::` で始まるパスは、crate 全体（`crate`）を許すときだけ使える（層3では `crate::` から書く）。
//! - `#[path = ..]` と `include!(..)` は使えない（読み込んだファイルをこの検査が見ないため）。
//!
//! 字句だけで見るので、マクロが組み立てるパス（`concat_idents!` や、別の crate のマクロの中身）は見つけられない。
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
                    let (decoded, len) = escape(&chars[j + 1..]);
                    let newlines = chars[j + 1..j + 1 + len].iter().filter(|&&c| c == '\n').count();
                    line += newlines;
                    code.extend(std::iter::repeat_n('\n', newlines));
                    text.push_str(&decoded);
                    j += 1 + len;
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
                // 逃がした字（`'\''` の 2 つめの引用符も）を飛ばしてから、閉じる引用符を探す
                let mut j = i + 3;
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

/// 文字列の逃がし（`\` の後ろ）を読み、その字と、読んだ長さを返す。
/// 行の継続（`\` と改行）は、後ろの空白ごと空白 1 つにする（語がつながらないように）。
fn escape(rest: &[char]) -> (String, usize) {
    let Some(&first) = rest.first() else { return (String::new(), 0) };
    match first {
        'n' => ("\n".to_owned(), 1),
        'r' => ("\r".to_owned(), 1),
        't' => ("\t".to_owned(), 1),
        '0' => ("\0".to_owned(), 1),
        'x' => {
            let hex: String = rest.iter().skip(1).take(2).collect();
            (u8::from_str_radix(&hex, 16).map_or(String::new(), |b| char::from(b).to_string()), 1 + hex.chars().count())
        }
        'u' if rest.get(1) == Some(&'{') => {
            let close = rest.iter().position(|&c| c == '}').unwrap_or(rest.len() - 1);
            let hex: String = rest[2.min(close)..close].iter().collect();
            let c = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32);
            (c.map_or(String::new(), String::from), close + 1)
        }
        '\n' => (" ".to_owned(), 1 + rest[1..].iter().take_while(|c| c.is_whitespace()).count()),
        c => (c.to_string(), 1),
    }
}

/// 文字列が SQL の文に見えるか。語の順序を、近い範囲で見る（`CREATE UNIQUE INDEX`、`UPDATE a JOIN b ... SET` など）。
pub fn looks_like_sql(text: &str) -> bool {
    let words: Vec<String> = text
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_uppercase)
        .collect();
    // a の後ろ window 語以内に bs のどれかがある
    let near = |a: &str, bs: &[&str], window: usize| {
        words
            .iter()
            .enumerate()
            .any(|(i, w)| w == a && words.iter().skip(i + 1).take(window).any(|x| bs.contains(&x.as_str())))
    };
    let objects = ["TABLE", "INDEX", "TRIGGER", "VIEW", "DATABASE", "SCHEMA", "PROCEDURE", "FUNCTION", "EVENT"];
    near("SELECT", &["FROM"], usize::MAX)
        || near("INSERT", &["INTO"], 3)
        || near("REPLACE", &["INTO"], 3)
        || near("UPDATE", &["SET"], 12)
        || near("DELETE", &["FROM"], 6)
        || ["ALTER", "CREATE", "DROP", "TRUNCATE", "RENAME"].iter().any(|v| near(v, &objects, 4))
}

/// use の木から `as 別名` を葉ごとに落とし、空白を除く（`a::{b as c, d}` → `a::{b,d}`）。
fn strip_aliases(tree: &str) -> String {
    // 語（英数字と _ の並び）と、空白でない記号 1 字ずつに分ける
    let mut tokens: Vec<String> = Vec::new();
    let mut word = false;
    for c in tree.chars() {
        let is_word = c.is_alphanumeric() || c == '_';
        match tokens.last_mut() {
            Some(t) if is_word && word => t.push(c),
            _ if !c.is_whitespace() => tokens.push(c.to_string()),
            _ => {}
        }
        word = is_word;
    }
    let mut out = String::new();
    let mut it = tokens.into_iter();
    while let Some(t) = it.next() {
        if t == "as" {
            it.next();
        } else {
            out.push_str(&t);
        }
    }
    out
}

/// `use a::{b, c::{d, e}};` を `a::b`, `a::c::d`, `a::c::e` に開く。
fn expand(tree: &str) -> Vec<String> {
    let tree = strip_aliases(tree);
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

/// コードの中の use の対象（行番号つき）と、use の文を空白にしたコード（改行は残す）。
fn uses(code: &str) -> (Vec<(usize, String)>, String) {
    let mut out = Vec::new();
    let mut rest = String::new();
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
            rest.extend(tree.chars().filter(|&c| c == '\n'));
            out.extend(expand(tree.trim()).into_iter().map(|p| (start_line, p.trim_start_matches("::").to_owned())));
            i = j;
            continue;
        }
        rest.push(bytes[i]);
        i += 1;
    }
    (out, rest)
}

/// コードの中で書き切ったパス（`a::b::c`、2 つ以上の部分）を、行番号つきで返す。先頭の `::` は落とす。
fn paths(code: &str) -> Vec<(usize, String)> {
    let chars: Vec<char> = code.chars().collect();
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = Vec::new();
    let (mut line, mut i) = (1, 0);
    while i < chars.len() {
        if chars[i] == '\n' {
            line += 1;
        }
        let starts = is_ident(chars[i]) && !chars[i].is_ascii_digit() && (i == 0 || !is_ident(chars[i - 1]));
        if !starts {
            i += 1;
            continue;
        }
        let mut segments = Vec::new();
        let mut j = i;
        loop {
            let from = j;
            while j < chars.len() && is_ident(chars[j]) {
                j += 1;
            }
            segments.push(chars[from..j].iter().collect::<String>());
            if chars.get(j) == Some(&':')
                && chars.get(j + 1) == Some(&':')
                && chars.get(j + 2).is_some_and(|&c| is_ident(c))
            {
                j += 2;
            } else {
                break;
            }
        }
        if segments.len() > 1 {
            out.push((line, segments.join("::")));
        }
        i = j;
    }
    out
}

/// `extern crate a as b;` の（別名, 元の名前）。
fn extern_aliases(code: &str) -> Vec<(String, String)> {
    let words: Vec<&str> =
        code.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == ';')).filter(|w| !w.is_empty()).collect();
    let mut out = Vec::new();
    for (i, w) in words.iter().enumerate() {
        if *w == "extern" && words.get(i + 1) == Some(&"crate") && words.get(i + 3) == Some(&"as") {
            let (name, alias) = (words.get(i + 2).copied().unwrap_or(""), words.get(i + 4).copied().unwrap_or(""));
            out.push((alias.trim_end_matches(';').to_owned(), name.trim_end_matches(';').to_owned()));
        }
    }
    out
}

/// `#[path = ..]` と `include!` の行番号と、その名前。
fn file_inclusions(code: &str) -> Vec<(usize, &'static str)> {
    let chars: Vec<char> = code.chars().collect();
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let skip_ws = |mut k: usize| {
        while chars.get(k).is_some_and(|c| c.is_whitespace()) {
            k += 1;
        }
        k
    };
    let word_at = |k: usize| -> String { chars[k..].iter().take_while(|&&c| is_ident(c)).collect() };
    let mut out = Vec::new();
    let mut line = 1;
    for i in 0..chars.len() {
        if chars[i] == '\n' {
            line += 1;
        }
        if chars[i] == '#' {
            let mut k = skip_ws(i + 1);
            if chars.get(k) == Some(&'!') {
                k = skip_ws(k + 1);
            }
            if chars.get(k) == Some(&'[') && word_at(skip_ws(k + 1)) == "path" {
                out.push((line, "#[path]"));
            }
        } else if (i == 0 || !is_ident(chars[i - 1]))
            && word_at(i) == "include"
            && chars.get(skip_ws(i + "include".len())) == Some(&'!')
        {
            out.push((line, "include!"));
        }
    }
    out
}

/// 1 つのソースを確かめる。allowed は use してよいパスの接頭辞（`ai_yagni_data_sdk::future` など）。
pub fn source(src: &str, shown: &str, allowed: &[&str]) -> Vec<String> {
    let s = scan(src);
    let mut out = Vec::new();
    let aliases = extern_aliases(&s.code);
    // `extern crate a as b;` の b を a に読み替え、先頭の `::` を落とす
    let resolve = |path: &str| -> String {
        let path = path.trim_start_matches("::");
        let (root, rest) = path.split_once("::").map_or((path, None), |(r, rest)| (r, Some(rest)));
        let root = aliases.iter().find(|(alias, _)| alias == root).map_or(root, |(_, name)| name.as_str());
        rest.map_or(root.to_owned(), |rest| format!("{root}::{rest}"))
    };
    let whole_crate = allowed.contains(&"crate");
    let ok = |path: &str| {
        (whole_crate && (path == "super" || path.starts_with("super::")))
            || allowed.iter().any(|a| path == *a || path.starts_with(&format!("{a}::")))
    };
    let (used, code) = uses(&s.code);
    for (line, path) in used {
        let path = resolve(&path);
        if !ok(&path) {
            out.push(format!("{shown}:{line}: may not use {path}"));
        }
    }
    // コードの中のパスは、crate の根から書いたと分かるものだけを見る
    let roots: Vec<String> = allowed
        .iter()
        .map(|a| a.split("::").next().unwrap_or(a))
        .chain(["crate", "super", "std", "core", "alloc", "ai_yagni_data_sdk"])
        .map(str::to_owned)
        .chain(aliases.iter().map(|(_, name)| name.clone()))
        .collect();
    for (line, path) in paths(&code) {
        let path = resolve(&path);
        let root = path.split("::").next().unwrap_or("");
        if roots.iter().any(|r| r == root) && !ok(&path) {
            out.push(format!("{shown}:{line}: may not use {path}"));
        }
    }
    for (line, what) in file_inclusions(&s.code) {
        out.push(format!("{shown}:{line}: {what} is not allowed (the file it reads is not checked)"));
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

/// ディレクトリの下の .rs をすべて確かめる（層2のフレームワーク）。symlink の輪は 1 度だけたどる。
pub fn dir(root: &Path, allowed: &[&str]) -> Vec<String> {
    let mut files = Vec::new();
    let mut seen = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let real = d.canonicalize().unwrap_or_else(|_| d.clone());
        if seen.contains(&real) {
            continue;
        }
        seen.push(real);
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
        for sql in [
            "CREATE UNIQUE INDEX i ON t (a)",
            "CREATE TEMPORARY TABLE x (a INT)",
            "CREATE OR REPLACE VIEW v AS SELECT 1",
            "INSERT IGNORE INTO t VALUES (1)",
            "INSERT LOW_PRIORITY IGNORE INTO t VALUES (1)",
            "REPLACE INTO t VALUES (1)",
            "UPDATE db.t SET a = 1",
            "UPDATE a AS b SET b.x = 1",
            "UPDATE a JOIN b ON a.id = b.a_id SET a.x = b.y",
            "DELETE a FROM a JOIN b ON a.id = b.id",
            "DELETE t1, t2 FROM t1 JOIN t2",
        ] {
            assert!(looks_like_sql(sql), "{sql}");
        }
        assert!(!looks_like_sql("replace the label"));
        assert!(!looks_like_sql("update the settings"));
    }

    #[test]
    fn escapes_do_not_merge_words() {
        let src = "let x = \"UPDATE\\taccounts\\nSET x = 1\";\nlet y = \"DELETE\\\n    FROM t\";\nlet z = \"\\u{53}ELECT 1 FROM t\";\n";
        assert_eq!(source(src, "f.rs", &[]).len(), 3, "{:?}", source(src, "f.rs", &[]));
    }

    #[test]
    fn an_escaped_quote_in_a_char_literal_does_not_open_a_string() {
        let src = "let q = '\\'';\nlet s = \"DROP TABLE t\";\nlet c = '\\u{41}';\n";
        assert_eq!(source(src, "f.rs", &[]), ["f.rs:2: SQL in a string (put it in a .sql file)"]);
    }

    #[test]
    fn aliases_are_dropped_per_leaf() {
        let src =
            "use ai_yagni_data_sdk::{future::Future as F, vocab::table_exists};\nuse crate::ledger::Ledger as L;\n";
        assert_eq!(
            source(src, "f.rs", &["ai_yagni_data_sdk::future", "crate::ledger"]),
            vec!["f.rs:1: may not use ai_yagni_data_sdk::vocab::table_exists"]
        );
        assert_eq!(expand(" a::{b as c, d::{e as _, f}}"), ["a::b", "a::d::e", "a::d::f"]);
    }

    #[test]
    fn paths_written_out_in_code_follow_the_same_rule() {
        let src = "use ai_yagni_data_sdk::future::Future;\nfn f() {\n    let t = ai_yagni_data_sdk::vocab::table_exists(\"x\");\n    let g = ::ai_yagni_data_sdk::future::Future::new(\"g\");\n    let s = std::fs::read(\"x\");\n    let u = Future::new(\"u\");\n    let l = crate::ledger::Ledger::new();\n}\n";
        assert_eq!(
            source(src, "f.rs", &["ai_yagni_data_sdk::future", "crate::ledger"]),
            vec!["f.rs:3: may not use ai_yagni_data_sdk::vocab::table_exists", "f.rs:5: may not use std::fs::read"]
        );
    }

    #[test]
    fn crate_aliases_super_and_file_inclusions_are_found() {
        let allowed = ["ai_yagni_data_sdk::future", "crate::shop"];
        let src = "extern crate ai_yagni_data_sdk as s;\nuse ::ai_yagni_data_sdk::future::Future;\nfn f() {\n    let _ = s::vocab::table_exists(\"x\");\n    let _ = super::helpers::raw();\n    let _ = ::ai_yagni_data_sdk::future::Future::new(\"x\");\n}\n#[path = \"../x.rs\"]\nmod x;\ninclude!(\"y.rs\");\nconst T: &str = include_str!(\"t.txt\");\nuse s::vocab::not;\n";
        assert_eq!(
            source(src, "f.rs", &allowed),
            [
                "f.rs:12: may not use ai_yagni_data_sdk::vocab::not",
                "f.rs:4: may not use ai_yagni_data_sdk::vocab::table_exists",
                "f.rs:5: may not use super::helpers::raw",
                "f.rs:8: #[path] is not allowed (the file it reads is not checked)",
                "f.rs:10: include! is not allowed (the file it reads is not checked)",
            ]
        );
        // crate 全体を許すなら super:: も使える（層2のテストの `use super::*;` など）
        assert_eq!(source("use super::*;\nfn f() { super::g(); }\n", "f.rs", &["crate"]), Vec::<String>::new());
    }

    #[test]
    fn symlink_cycles_are_followed_once() {
        let root = std::env::temp_dir().join(format!("lint-cycle-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("a")).unwrap();
        fs::write(root.join("a/x.rs"), "const S: &str = \"DROP TABLE t\";\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&root, root.join("a/loop")).unwrap();
        assert_eq!(dir(&root, &[]).len(), 1);
        let _ = fs::remove_dir_all(&root);
    }
}
