//! mysql CLI には束縛変数がないので、値はクライアント側でリテラルにして埋め込む。
//! 文字列は 16 進のリテラルに文字セットを付けて書く（`_utf8mb4 X'..'`）。SQL モード（NO_BACKSLASH_ESCAPES）に左右されない。
//!
//! スクリプトを文に分けるのも、ここで字句を読んで行う（引用・コメント・`DELIMITER` を mysql CLI と同じように扱う）。

/// 文字列のリテラル。
pub fn literal(v: &str) -> String {
    let hex: String = v.bytes().map(|b| format!("{b:02x}")).collect();
    format!("_utf8mb4 X'{hex}'")
}

/// 識別子（表名・列名・データベース名）。判定は Verus で証明した関数で行う。
pub fn ident(name: &str) -> Result<String, String> {
    if ai_yagni_data_sdk_verified::is_identifier(name.as_bytes()) {
        Ok(format!("`{name}`"))
    } else {
        Err(format!("bad identifier: {name}"))
    }
}

/// スクリプトを字句で読んだ結果。
struct Lexed {
    /// 区切りで閉じた文（区切りを除く。コメントと空白だけのものは入れない）。
    statements: Vec<String>,
    /// 最後の区切りの後ろに残った、閉じていない文（コメントと空白だけなら None）。
    pending: Option<String>,
    /// 終わりの時点の区切り（`DELIMITER` で変わる）。
    delimiter: String,
}

/// コメントなら、その終わりの位置。`-- ` は後ろに空白が要る。`/*! */` はコードとして読む。
fn comment_end(chars: &[char], i: usize) -> Option<usize> {
    let at = |k: usize| chars.get(k).copied().unwrap_or('\0');
    let line = |from: usize| chars[from..].iter().position(|&c| c == '\n').map_or(chars.len(), |n| from + n);
    match (at(i), at(i + 1)) {
        ('#', _) => Some(line(i)),
        ('-', '-') if i + 2 >= chars.len() || at(i + 2).is_whitespace() => Some(line(i)),
        ('/', '*') if at(i + 2) != '!' => {
            Some(chars[i + 2..].windows(2).position(|w| w == ['*', '/']).map_or(chars.len(), |n| i + 2 + n + 2))
        }
        _ => None,
    }
}

/// 引用（`'..'` `".."` `` `..` ``）の終わりの位置。
fn quote_end(chars: &[char], i: usize) -> usize {
    let q = chars[i];
    let mut j = i + 1;
    while j < chars.len() {
        // 逃がした字と、2 つ重ねた引用符は飛ばす
        let escaped = chars[j] == '\\' && q != '`';
        if escaped || (chars[j] == q && chars.get(j + 1) == Some(&q)) {
            j += 2;
        } else if chars[j] == q {
            return j + 1;
        } else {
            j += 1;
        }
    }
    chars.len()
}

fn lex(script: &str) -> Lexed {
    let chars: Vec<char> = script.chars().collect();
    let mut delimiter: Vec<char> = vec![';'];
    let mut statements = Vec::new();
    let mut current = String::new();
    // current にコメントと空白でない字があるか
    let mut code = false;
    let mut line_start = true;
    let mut i = 0;
    while i < chars.len() {
        // DELIMITER は、文の始まりの行頭だけで効く
        if line_start && !code {
            let line: String = chars[i..].iter().take_while(|&&c| c != '\n').collect();
            let mut words = line.split_whitespace();
            if words.next().is_some_and(|w| w.eq_ignore_ascii_case("delimiter")) {
                if let Some(d) = words.next() {
                    delimiter = d.chars().collect();
                }
                current.clear();
                i += line.chars().count();
                continue;
            }
        }
        let c = chars[i];
        line_start = c == '\n';
        if chars[i..].starts_with(&delimiter) {
            if code {
                statements.push(current.trim().to_owned());
            }
            current.clear();
            code = false;
            i += delimiter.len();
            continue;
        }
        let end = match comment_end(&chars, i) {
            Some(end) => end,
            None if matches!(c, '\'' | '"' | '`') => {
                code = true;
                quote_end(&chars, i)
            }
            None => {
                code |= !c.is_whitespace();
                i + 1
            }
        };
        current.extend(&chars[i..end]);
        i = end;
    }
    Lexed { statements, pending: code.then(|| current.trim().to_owned()), delimiter: delimiter.into_iter().collect() }
}

/// 後ろに `;` で区切った文を続けられるように、スクリプトを閉じる。
/// 閉じていない最後の文を区切りで閉じ（行末のコメントの中の `;` は区切りではない）、区切りを `;` に戻す。
pub fn terminate(script: &str) -> String {
    let lexed = lex(script);
    let mut out = script.trim_end().to_owned();
    if out.is_empty() {
        return out;
    }
    out.push('\n');
    if lexed.pending.is_some() {
        out.push_str(&lexed.delimiter);
        out.push('\n');
    }
    if lexed.delimiter != ";" {
        out.push_str("DELIMITER ;\n");
    }
    out
}

/// スクリプトがちょうど 1 つの文なら、その文（区切りを除く）。
pub(crate) fn single_statement(script: &str) -> Result<String, String> {
    let lexed = lex(script);
    if lexed.delimiter != ";" {
        return Err("DELIMITER is not allowed here".to_owned());
    }
    let mut statements = lexed.statements;
    statements.extend(lexed.pending);
    match statements.len() {
        1 => Ok(statements.remove(0)),
        0 => Err("no statement".to_owned()),
        n => Err(format!("{n} statements (write exactly one)")),
    }
}

/// 文の最初の語（前のコメントを飛ばし、大文字にする）。括弧で始まれば `(`。
pub(crate) fn first_word(statement: &str) -> String {
    let chars: Vec<char> = statement.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if let Some(end) = comment_end(&chars, i) {
            i = end;
        } else if chars[i].is_whitespace() {
            i += 1;
        } else {
            break;
        }
    }
    if chars.get(i) == Some(&'(') {
        return "(".to_owned();
    }
    chars[i..].iter().take_while(|c| c.is_ascii_alphanumeric() || **c == '_').collect::<String>().to_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_are_hex_and_independent_of_the_sql_mode() {
        assert_eq!(literal("it's"), "_utf8mb4 X'69742773'");
        assert_eq!(literal("a\\b"), "_utf8mb4 X'615c62'");
        assert_eq!(literal(""), "_utf8mb4 X''");
    }

    #[test]
    fn identifiers_are_checked() {
        assert_eq!(ident("orders").unwrap(), "`orders`");
        assert!(ident("x`; DROP TABLE y").is_err());
    }

    #[test]
    fn terminate_closes_the_last_statement_once() {
        assert_eq!(terminate("SELECT 1\n-- end\n\n"), "SELECT 1\n-- end\n;\n");
        assert_eq!(terminate("SELECT 1;"), "SELECT 1;\n");
        assert_eq!(terminate("SELECT 1; -- done\n"), "SELECT 1; -- done\n");
        assert_eq!(terminate("DELIMITER $$\nX$$\nDELIMITER ;\n"), "DELIMITER $$\nX$$\nDELIMITER ;\n");
        assert_eq!(terminate("DELIMITER $$\nX$$\n"), "DELIMITER $$\nX$$\nDELIMITER ;\n");
        assert_eq!(terminate("-- only\n"), "-- only\n");
        assert_eq!(terminate(""), "");
    }

    #[test]
    fn a_semicolon_in_a_trailing_line_comment_does_not_close_the_statement() {
        assert_eq!(terminate("UPDATE t SET a = 1 -- note;"), "UPDATE t SET a = 1 -- note;\n;\n");
        assert_eq!(terminate("SELECT ';'"), "SELECT ';'\n;\n");
        assert_eq!(terminate("SELECT 1 /* ; */"), "SELECT 1 /* ; */\n;\n");
        assert_eq!(terminate("SELECT 1 # x;"), "SELECT 1 # x;\n;\n");
    }

    #[test]
    fn single_statements() {
        assert_eq!(single_statement("-- x\nSELECT 1;\n-- y\n").unwrap(), "-- x\nSELECT 1");
        assert_eq!(single_statement("SELECT ';' -- ;").unwrap(), "SELECT ';' -- ;");
        assert_eq!(single_statement("SELECT 'a\\'; b'").unwrap(), "SELECT 'a\\'; b'");
        assert!(single_statement("SELECT 1; SELECT 2").is_err());
        assert!(single_statement("SELECT 1; DROP TABLE t;").is_err());
        assert!(single_statement("-- nothing\n").is_err());
        assert!(single_statement("DELIMITER $$\nSELECT 1$$\n").is_err());
        assert_eq!(first_word("-- x\n/* y */ select 1"), "SELECT");
        assert_eq!(first_word("(SELECT 1)"), "(");
    }
}
