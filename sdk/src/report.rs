//! 対応表: 未来の一覧、語彙（業務の言葉 → スキーマの言明）と使う未来、未来どうしの組み合わせの表（Markdown）。
use crate::check::Checker;

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
        out.push(format!("| {} | {} | {} | {} |", f.id, f.why, f.expires, with.join(", ")));
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
        out.push(format!("| {} | {} | {} | {} |", w.name(), w.meaning(), w.says(), users.join(", ")));
    }
    let ids: Vec<&str> = p.futures.iter().map(|f| f.id.as_str()).collect();
    out.extend([
        String::new(),
        "## 組み合わせ（行の差分の後に列の差分を当てる。○ = 両方の期待が成り立つ、× = 失敗、! = 衝突を宣言）"
            .to_owned(),
        String::new(),
        format!("| | {} |", ids.join(" | ")),
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
            let problems = checker.compose(a, b);
            cells.push(format!("{}{}", if problems.is_empty() { "○" } else { "×" }, if declared { "!" } else { "" }));
            if !problems.is_empty() && !declared {
                undeclared.push(format!("- {} → {}: {}", a.id, b.id, problems[0]));
            }
        }
        out.push(format!("| {} | {} |", a.id, cells.join(" | ")));
    }
    if !undeclared.is_empty() {
        out.extend([String::new(), "宣言していない組み合わせの失敗（最初の理由）:".to_owned(), String::new()]);
        out.extend(undeclared);
    }
    out.join("\n")
}
