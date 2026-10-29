//! mysql CLI には束縛変数がないので、値はクライアント側でリテラルにして埋め込む。
//! 文字列は MySQL の既定（NO_BACKSLASH_ESCAPES なし）のエスケープ規則で引用する。

/// 文字列のリテラル。
pub fn literal(v: &str) -> String {
    let mut out = String::with_capacity(v.len() + 2);
    out.push('\'');
    for c in v.chars() {
        match c {
            '\0' => out.push_str("\\0"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\x1a' => out.push_str("\\Z"),
            '\'' => out.push_str("\\'"),
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// 識別子（表名・列名・データベース名）。判定は Verus で証明した関数で行う。
pub fn ident(name: &str) -> Result<String, String> {
    if ai_yagni_data_sdk_verified::is_identifier(name.as_bytes()) {
        Ok(format!("`{name}`"))
    } else {
        Err(format!("bad identifier: {name}"))
    }
}

fn blank_or_comment(line: &str) -> bool {
    let t = line.trim();
    t.is_empty() || t.starts_with("--") || t.starts_with('#')
}

/// スクリプトの末尾の空行とコメント行を落とし、最後の文を ; で閉じる（空の文はエラーになるので足しすぎない）。
pub fn terminate(script: &str) -> String {
    let mut lines: Vec<&str> = script.split('\n').collect();
    while lines.last().is_some_and(|l| blank_or_comment(l)) {
        lines.pop();
    }
    let body = lines.join("\n");
    let body = body.trim_end();
    if body.is_empty() {
        return String::new();
    }
    let last = lines.last().map_or("", |l| l.trim_end());
    let closed = last.ends_with(';') || last.to_ascii_uppercase().split_whitespace().eq(["DELIMITER", ";"]);
    if closed { format!("{body}\n") } else { format!("{body};\n") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_escape_quotes_and_backslashes() {
        assert_eq!(literal("it's"), "'it\\'s'");
        assert_eq!(literal("a\\b\n"), "'a\\\\b\\n'");
    }

    #[test]
    fn identifiers_are_checked() {
        assert_eq!(ident("orders").unwrap(), "`orders`");
        assert!(ident("x`; DROP TABLE y").is_err());
    }

    #[test]
    fn terminate_closes_the_last_statement_once() {
        assert_eq!(terminate("SELECT 1\n-- end\n\n"), "SELECT 1;\n");
        assert_eq!(terminate("SELECT 1;"), "SELECT 1;\n");
        assert_eq!(terminate("DELIMITER $$\nX$$\nDELIMITER ;\n"), "DELIMITER $$\nX$$\nDELIMITER ;\n");
        assert_eq!(terminate("-- only\n"), "");
    }
}
