//! mysql CLI には束縛変数がないので、値はクライアント側でリテラルにして埋め込む。
//! 文字列は 16 進のリテラルに文字セットを付けて書く（`_utf8mb4 X'..'`）。SQL モード（NO_BACKSLASH_ESCAPES）に左右されない。
//!
//! スクリプトを文に分けるのも、ここで字句を読んで行う（引用・コメント・`DELIMITER` を mysql CLI と同じように扱う）。
//!
//! ただし、字句の読み方は mysql CLI やサーバーと完全には一致しない（`/*!99999 ... */` の版のコメント、CLI のコマンドなど）。
//! そこで、1 つの文だけを流したいとき（`sql_empty` と固定クエリ）は、文を 16 進のリテラルにしてサーバーへ渡し、
//! サーバーの `PREPARE` に読ませる（[`server_side`]）。`PREPARE` は 2 つ以上の文を拒む。

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

/// 文をサーバーの側で読ませる 1 行（`SET @q = <16 進>; PREPARE q FROM @q;`）。
/// execute なら続けて流し（`EXECUTE`）、片付ける。クライアントの字句の読み方に左右されない。
pub(crate) fn server_side(statement: &str, execute: bool) -> String {
    let prepare = format!("SET @ai_yagni_q = {}; PREPARE ai_yagni_q FROM @ai_yagni_q;", literal(statement));
    if execute { format!("{prepare} EXECUTE ai_yagni_q; DEALLOCATE PREPARE ai_yagni_q;") } else { prepare }
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

/// スクリプトの文（区切りを除く。閉じていない最後の文も含む）。
pub(crate) fn statements(script: &str) -> Vec<String> {
    let lexed = lex(script);
    let mut out = lexed.statements;
    out.extend(lexed.pending);
    out
}

/// 文の語（大文字にする）。引用とコメントは飛ばし、版のコメント（`/*!80000 ... */`）の中身は語として読む。
/// `@@GLOBAL.x` は `@@GLOBAL` という 1 語にする。
fn words(statement: &str) -> Vec<String> {
    let chars: Vec<char> = statement.chars().collect();
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '/' && chars.get(i + 1) == Some(&'*') && chars.get(i + 2) == Some(&'!') {
            i += 3;
            while chars.get(i).is_some_and(char::is_ascii_digit) {
                i += 1;
            }
        } else if c == '*' && chars.get(i + 1) == Some(&'/') {
            i += 2;
        } else if let Some(end) = comment_end(&chars, i) {
            i = end;
        } else if matches!(c, '\'' | '"' | '`') {
            i = quote_end(&chars, i);
        } else if c == '@' && chars.get(i + 1) == Some(&'@') {
            let end = (i + 2..chars.len()).find(|&k| !is_word(chars[k])).unwrap_or(chars.len());
            out.push(chars[i..end].iter().collect::<String>().to_ascii_uppercase());
            i = end;
        } else if is_word(c) {
            let end = (i..chars.len()).find(|&k| !is_word(chars[k])).unwrap_or(chars.len());
            out.push(chars[i..end].iter().collect::<String>().to_ascii_uppercase());
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

/// 文が、そのデータベースの外（サーバー全体）を変えるなら、その種類（`SET GLOBAL` など）。
/// 検査はデータベースだけを分けるので、サーバーの設定・利用者・権限・ほかのデータベースを変える文は、
/// ほかの検査に漏れる。字句だけで見る（ストアドプロシージャやトリガーの本体の中までは見ない）。
pub(crate) fn server_wide(statement: &str) -> Option<String> {
    let w = words(statement);
    let at = |k: usize| w.get(k).map_or("", String::as_str);
    let (first, second) = (at(0), at(1));
    let global = ["GLOBAL", "PERSIST", "PERSIST_ONLY", "@@GLOBAL", "@@PERSIST", "@@PERSIST_ONLY"];
    let found = match first {
        "SET" if w.iter().any(|x| global.contains(&x.as_str())) => "SET GLOBAL / SET PERSIST",
        "SET" if matches!(second, "PASSWORD" | "DEFAULT" | "RESOURCE") => "SET PASSWORD / SET DEFAULT ROLE",
        "USE" => "USE",
        "GRANT" => "GRANT",
        "REVOKE" => "REVOKE",
        "CREATE" | "DROP" | "ALTER" | "RENAME" if matches!(second, "USER" | "ROLE") => "a user or a role",
        "CREATE" | "DROP" if matches!(second, "DATABASE" | "SCHEMA") => "CREATE / DROP DATABASE",
        "CREATE" | "DROP" | "ALTER"
            if matches!(second, "TABLESPACE" | "SERVER" | "LOGFILE" | "RESOURCE" | "SPATIAL") =>
        {
            "a server-wide object"
        }
        "INSTALL" | "UNINSTALL" => "a plugin or a component",
        "SHUTDOWN" | "RESTART" | "KILL" | "FLUSH" | "RESET" | "PURGE" | "CHANGE" => "server administration",
        "START" | "STOP" if matches!(second, "REPLICA" | "SLAVE" | "GROUP_REPLICATION") => "replication",
        _ if w.windows(2).any(|p| p[0] == "INTO" && matches!(p[1].as_str(), "OUTFILE" | "DUMPFILE")) => {
            "INTO OUTFILE / DUMPFILE"
        }
        _ => return None,
    };
    Some(found.to_owned())
}

/// 権限のファイル（GRANT の文だけ）を、検査の使い捨てのデータベースと利用者に向けて書き換える。
///
/// - `{{database}}` を database（識別子）にする。
/// - schema があれば、その名前で修飾した表（`shop.orders`、`` `shop`.* ``）の修飾を database にする。
/// - user（`'proxy'@'%'` の `proxy`。引用してもしなくてもよい）を、scratch_user（`'名前'@'%'`）にする。
pub(crate) fn rewrite_grants(
    script: &str,
    user: &str,
    schema: Option<&str>,
    scratch_user: &str,
    database: &str,
) -> Result<String, String> {
    for s in statements(script) {
        let w = first_word(&s);
        if w != "GRANT" {
            return Err(format!("only GRANT statements are allowed, not {w}"));
        }
    }
    let chars: Vec<char> = script.chars().collect();
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
    let skip_ws = |mut k: usize| {
        while chars.get(k).is_some_and(|c| c.is_whitespace()) {
            k += 1;
        }
        k
    };
    // `@` の後ろのホスト（引用か、空白と区切りまでの字）の終わり
    let host_end = |k: usize| {
        let k = skip_ws(k);
        match chars.get(k) {
            Some('\'' | '"' | '`') => quote_end(&chars, k),
            _ => (k..chars.len())
                .find(|&j| !(is_word(chars[j]) || matches!(chars[j], '%' | '.' | '-')))
                .unwrap_or(chars.len()),
        }
    };
    let db = ident(database)?;
    let mut out = String::with_capacity(script.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i..].starts_with(&['{', '{']) {
            let close = chars[i..].windows(2).position(|w| w == ['}', '}']).map(|n| i + n);
            let inner: Option<String> = close.map(|c| chars[i + 2..c].iter().collect());
            if let (Some(c), Some("database")) = (close, inner.as_deref().map(str::trim)) {
                out.push_str(&db);
                i = c + 2;
                continue;
            }
        }
        if let Some(end) = comment_end(&chars, i) {
            out.extend(&chars[i..end]);
            i = end;
            continue;
        }
        let (end, name) = if matches!(chars[i], '\'' | '"' | '`') {
            let end = quote_end(&chars, i);
            (end, chars[i + 1..end.saturating_sub(1).max(i + 1)].iter().collect::<String>())
        } else if is_word(chars[i]) && (i == 0 || !(is_word(chars[i - 1]) || chars[i - 1] == '.')) {
            let end = (i..chars.len()).find(|&k| !is_word(chars[k])).unwrap_or(chars.len());
            (end, chars[i..end].iter().collect::<String>())
        } else {
            out.push(chars[i]);
            i += 1;
            continue;
        };
        let after = skip_ws(end);
        if name == user && chars.get(after) == Some(&'@') {
            out.push_str(&format!("'{scratch_user}'@'%'"));
            i = host_end(after + 1);
        } else if schema == Some(name.as_str()) && chars.get(end) == Some(&'.') && chars[i] != '\'' && chars[i] != '"' {
            out.push_str(&db);
            i = end;
        } else {
            out.extend(&chars[i..end]);
            i = end;
        }
    }
    Ok(out)
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

    #[test]
    fn statements_go_to_the_server_as_a_hex_literal() {
        let line = server_side("SELECT 1 /*!99999 ' */; COMMIT; -- '", true);
        assert!(!line.contains("COMMIT") && line.contains("EXECUTE ai_yagni_q"), "{line}");
        assert_eq!(line.lines().count(), 1);
        assert!(!server_side("SELECT 1", false).contains("EXECUTE"));
    }

    #[test]
    fn statements_that_reach_beyond_the_database_are_found() {
        for (sql, found) in [
            ("SET GLOBAL max_connections = 10", true),
            ("set @@global.sql_mode = ''", true),
            ("SET SESSION sql_mode = '', PERSIST x = 1", true),
            ("/*!80000 SET GLOBAL x = 1 */", true),
            ("SET SESSION innodb_lock_wait_timeout = 5", false),
            ("SET @x = 'GLOBAL'", false),
            ("USE other", true),
            ("CREATE USER 'u'@'%'", true),
            ("GRANT SELECT ON *.* TO 'u'@'%'", true),
            ("DROP DATABASE shop", true),
            ("drop schema if exists x", true),
            ("SELECT * FROM t INTO OUTFILE '/tmp/x'", true),
            ("CREATE TABLE users (id INT)", false),
            ("ALTER TABLE t ADD COLUMN `global` INT", false),
            ("INSERT INTO t VALUES ('DROP DATABASE x')", false),
            ("-- USE x\nSELECT 1", false),
        ] {
            assert_eq!(server_wide(sql).is_some(), found, "{sql}");
        }
    }

    #[test]
    fn grants_are_pointed_at_the_scratch_database_and_user() {
        let src = "-- 'proxy'@'%' の権限\nGRANT SELECT ON shop.customers TO 'proxy'@'%';\n\
                   GRANT INSERT (a, b) ON `shop`.orders TO proxy@'%';\nGRANT SELECT ON {{database}}.x TO \"proxy\" @ \"%\";\n\
                   GRANT SELECT ON shopping.t TO 'proxy'@'%';\n";
        let out = rewrite_grants(src, "proxy", Some("shop"), "ayg_1", "db_1").unwrap();
        assert_eq!(
            out,
            "-- 'proxy'@'%' の権限\nGRANT SELECT ON `db_1`.customers TO 'ayg_1'@'%';\n\
             GRANT INSERT (a, b) ON `db_1`.orders TO 'ayg_1'@'%';\nGRANT SELECT ON `db_1`.x TO 'ayg_1'@'%';\n\
             GRANT SELECT ON shopping.t TO 'ayg_1'@'%';\n"
        );
        assert!(rewrite_grants("CREATE USER x;", "proxy", None, "u", "d").is_err());
    }
}
