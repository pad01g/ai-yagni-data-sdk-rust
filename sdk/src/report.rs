//! 対応表: 未来の一覧、語彙（業務の言葉 → スキーマの言明）と使う未来、未来どうしの組み合わせの表（Markdown）。
use crate::check::Checker;
use crate::vocab::Verdict;

/// 表の升の中身（`|` を逃がし、改行を `<br>` にする）。
fn cell(s: &str) -> String {
    s.replace('\\', "\\\\").replace('|', "\\|").replace("\r\n", "<br>").replace(['\n', '\r'], "<br>")
}

/// 表の 1 行。
fn row(cells: &[&str]) -> String {
    format!("| {} |", cells.iter().map(|c| cell(c)).collect::<Vec<_>>().join(" | "))
}

pub fn markdown(checker: &Checker<'_>, title: &str) -> String {
    let p = &checker.project;
    let mut out = vec![
        format!("# {title}"),
        String::new(),
        "## 未来".to_owned(),
        String::new(),
        "| 未来 | なぜ | 期限 | 衝突 |".to_owned(),
        "|---|---|---|---|".to_owned(),
    ];
    for f in &p.futures {
        let with: Vec<&str> = f.conflicts.iter().map(|c| c.with.as_str()).collect();
        out.push(row(&[&f.id, &f.why, &f.expires, &with.join(", ")]));
    }
    out.extend([
        String::new(),
        "## 語彙（層2）".to_owned(),
        String::new(),
        "| 語 | 意味 | スキーマの言明 | 使う未来 |".to_owned(),
        "|---|---|---|---|".to_owned(),
    ]);
    for w in &p.words {
        let mut users: Vec<String> = Vec::new();
        if p.invariants.contains(w) {
            users.push("(今の不変条件)".to_owned());
        }
        users.extend(p.futures.iter().filter(|f| f.words().contains(w)).map(|f| f.id.clone()));
        out.push(row(&[w.name(), w.meaning(), w.says(), &users.join(", ")]));
    }
    let ids: Vec<&str> = p.futures.iter().map(|f| f.id.as_str()).collect();
    out.extend([
        String::new(),
        "## 組み合わせ（行の差分の後に列の差分を当てる。○ = 両方の期待が成り立つ、× = 失敗、? = 確かめられない、! = 衝突を宣言）"
            .to_owned(),
        String::new(),
        row(&[&[""], ids.as_slice()].concat()),
        format!("|---|{}|", ids.iter().map(|_| "---").collect::<Vec<_>>().join("|")),
    ]);
    let mut undeclared = Vec::new();
    for a in &p.futures {
        let mut cells = Vec::new();
        for b in &p.futures {
            if a.id == b.id {
                cells.push("—".to_owned());
                continue;
            }
            let declared = a.conflicts.iter().any(|c| c.with == b.id);
            let verdict = checker.compose(a, b);
            let mark = match verdict {
                Verdict::Pass => "○",
                Verdict::Fail(_) => "×",
                Verdict::Error(_) => "?",
            };
            cells.push(format!("{mark}{}", if declared { "!" } else { "" }));
            if let Some(first) = verdict.problems().first()
                && (!declared || verdict.is_error())
            {
                undeclared.push(format!("- {} → {}: {}", a.id, b.id, first.replace('\n', " ")));
            }
        }
        let mut line = vec![a.id.as_str()];
        line.extend(cells.iter().map(String::as_str));
        out.push(row(&line));
    }
    if !undeclared.is_empty() {
        out.extend([
            String::new(),
            "宣言していない組み合わせの失敗と、確かめられなかった組み合わせ（最初の理由）:".to_owned(),
            String::new(),
        ]);
        out.extend(undeclared);
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_cannot_break_the_table() {
        assert_eq!(row(&["a|b", "x\ny", "c\\|d"]), "| a\\|b | x<br>y | c\\\\\\|d |");
    }
}
