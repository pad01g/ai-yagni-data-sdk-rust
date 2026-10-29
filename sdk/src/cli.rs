//! 層3を走らせるコマンド。利用者の crate の小さな bin から呼ぶ。
//!
//! ```text
//! <bin> check  [--today YYYY-MM-DD] [--changed 変わったファイルの一覧 --repo リポジトリの根]
//! <bin> report
//! ```
//!
//! 変わったファイルの一覧は、リポジトリの根（--repo、省略するとプロジェクトの根）からの相対パス。
//! check は CI で走らせる。失敗があれば終了コード 1。report は対応表を出す（失敗しない）。
//! サーバーは `AI_YAGNI_MYSQL` のものを使い、なければ立てて終わりに止める。
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::check::{Checker, Options};
use crate::mysql::Server;
use crate::project::Project;
use crate::report;

/// 今日の日付（UTC、YYYY-MM-DD）。
pub fn today() -> String {
    let days = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() / 86_400) as i64;
    // Howard Hinnant の civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn option(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

/// vocabulary_uses は、未来の宣言が use してよい語彙のモジュール（`crate::shop` など）。
pub fn main(project: Project, vocabulary_uses: &[&str]) -> ! {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str);
    if !matches!(command, Some("check" | "report")) {
        eprintln!("usage: check [--today YYYY-MM-DD] [--changed FILE --repo DIR] | report");
        std::process::exit(2);
    }
    let server = match Server::obtain() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot start mysql: {e}");
            std::process::exit(2);
        }
    };
    let title = project.name.clone();
    let root = project.root.clone();
    let checker = Checker::new(&server, project);
    let code = if command == Some("report") {
        println!("{}", report::markdown(&checker, &title));
        0
    } else {
        let changed = option(&args, "--changed").map(|file| {
            std::fs::read_to_string(&file)
                .unwrap_or_else(|e| panic!("cannot read {file}: {e}"))
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect()
        });
        let opts = Options {
            today: option(&args, "--today").unwrap_or_else(today),
            changed,
            repo: option(&args, "--repo").map_or(root, PathBuf::from),
            vocabulary_uses: vocabulary_uses.iter().map(|s| (*s).to_owned()).collect(),
        };
        let r = checker.run(&opts);
        println!("== {title}");
        for s in &r.sections {
            println!("{} {}", if s.problems.is_empty() { "ok  " } else { "FAIL" }, s.name);
            for p in &s.problems {
                println!("       {p}");
            }
        }
        i32::from(!r.ok())
    };
    drop(checker);
    drop(server);
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    #[test]
    fn today_has_the_shape_of_a_day() {
        let t = super::today();
        assert_eq!(t.len(), 10);
        assert!(t.as_str() > "2026-01-01");
    }
}
