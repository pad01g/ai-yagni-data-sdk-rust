//! 層3を走らせるコマンド。利用者の crate の小さな bin から呼ぶ。
//!
//! ```text
//! <bin> check  [--today YYYY-MM-DD] [--changed 変わったファイルの一覧 [--repo リポジトリの根]]
//! <bin> report
//! ```
//!
//! 変わったファイルの一覧は、リポジトリの根（--repo、省略するとプロジェクトの根）からの相対パス。
//! `git diff --name-only --no-renames BASE...HEAD` で作る（`--no-renames` がないと、名前を変えたファイルの元の名前が
//! 出ず、それを見張る未来が見落とされる）。サブモジュールは、そのディレクトリの名前で出てよい。
//! check は CI で走らせる。失敗があれば終了コード 1。report は対応表を出す（失敗しない）。
//! 知らないフラグ、値のないフラグ、2 度書いたフラグ、正しくない日付は使い方の誤り（終了コード 2）。
//! サーバーは `AI_YAGNI_MYSQL` のものを使い、なければ立てて終わりに止める（SIGINT・SIGTERM でも止める）。
use std::path::PathBuf;

use crate::check::{Checker, Options};
use crate::date;
use crate::mysql::{Server, cleanup_on_signals};
use crate::project::Project;
use crate::report;

const USAGE: &str = "usage: check [--today YYYY-MM-DD] [--changed FILE [--repo DIR]] | report";

/// 今日の日付（UTC、YYYY-MM-DD）。
pub fn today() -> String {
    date::today()
}

/// コマンドラインの引数（プログラム名を除く）を読んだもの。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Args {
    Check { today: Option<String>, changed: Option<PathBuf>, repo: Option<PathBuf> },
    Report,
}

/// 引数を厳密に読む。`--flag value` と `--flag=value` の形を受け付ける。
pub fn parse(args: &[String]) -> Result<Args, String> {
    let (command, rest) = args.split_first().ok_or("no command")?;
    let mut flags: Vec<(&str, String)> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let arg = rest[i].as_str();
        let (name, value) = match arg.split_once('=') {
            Some((name, value)) => (name, value.to_owned()),
            None => {
                i += 1;
                match rest.get(i) {
                    Some(v) if !v.starts_with("--") => (arg, v.clone()),
                    _ => return Err(format!("{arg} needs a value")),
                }
            }
        };
        if !matches!(name, "--today" | "--changed" | "--repo") {
            return Err(format!("unknown argument {arg}"));
        }
        if value.is_empty() {
            return Err(format!("{name} needs a value"));
        }
        if flags.iter().any(|(n, _)| *n == name) {
            return Err(format!("{name} is given twice"));
        }
        flags.push((name, value));
        i += 1;
    }
    let take = |name: &str| flags.iter().find(|(n, _)| *n == name).map(|(_, v)| v.clone());
    match command.as_str() {
        "report" if flags.is_empty() => Ok(Args::Report),
        "report" => Err("report takes no flags".to_owned()),
        "check" => {
            let today = take("--today");
            if let Some(t) = &today
                && date::day(t).is_none()
            {
                return Err(format!("--today must be a date YYYY-MM-DD: {t}"));
            }
            let (changed, repo) = (take("--changed").map(PathBuf::from), take("--repo").map(PathBuf::from));
            if repo.is_some() && changed.is_none() {
                return Err("--repo is only used with --changed".to_owned());
            }
            Ok(Args::Check { today, changed, repo })
        }
        other => Err(format!("unknown command {other}")),
    }
}

/// vocabulary_uses は、未来の宣言が use してよい語彙のモジュール（`crate::shop` など）。
pub fn main(project: Project, vocabulary_uses: &[&str]) -> ! {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            std::process::exit(2);
        }
    };
    let changed = match &args {
        Args::Check { changed: Some(file), .. } => match std::fs::read_to_string(file) {
            Ok(s) => Some(s.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect()),
            Err(e) => {
                eprintln!("cannot read {}: {e}", file.display());
                std::process::exit(2);
            }
        },
        _ => None,
    };
    cleanup_on_signals();
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
    let code = match args {
        Args::Report => {
            println!("{}", report::markdown(&checker, &title));
            0
        }
        Args::Check { today: day, repo, .. } => {
            let opts = Options {
                today: day.unwrap_or_else(today),
                changed,
                repo: repo.unwrap_or(root),
                vocabulary_uses: vocabulary_uses.iter().map(|s| (*s).to_owned()).collect(),
            };
            let r = checker.run(&opts);
            println!("== {title}");
            for w in &r.warnings {
                println!("warn {w}");
            }
            for s in &r.sections {
                println!("{} {}", if s.problems.is_empty() { "ok  " } else { "FAIL" }, s.name);
                for p in &s.problems {
                    println!("       {p}");
                }
            }
            i32::from(!r.ok())
        }
    };
    drop(checker);
    drop(server);
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn today_has_the_shape_of_a_day() {
        assert!(date::day(&today()).is_some());
    }

    #[test]
    fn flags_are_read_strictly() {
        assert_eq!(parse(&args("report")), Ok(Args::Report));
        assert_eq!(
            parse(&args("check --today 2026-10-30 --changed c.txt --repo /r")),
            Ok(Args::Check {
                today: Some("2026-10-30".to_owned()),
                changed: Some(PathBuf::from("c.txt")),
                repo: Some(PathBuf::from("/r"))
            })
        );
        assert_eq!(
            parse(&args("check --changed=c.txt")),
            Ok(Args::Check { today: None, changed: Some(PathBuf::from("c.txt")), repo: None })
        );
        assert!(parse(&args("check --chnaged c.txt")).unwrap_err().contains("unknown argument"));
        assert!(parse(&args("check --today --changed c.txt")).unwrap_err().contains("--today needs a value"));
        assert!(parse(&args("check --changed")).is_err());
        assert!(parse(&args("check --today 2026-02-31")).unwrap_err().contains("--today must be a date"));
        assert!(parse(&args("check --today 2026-10-30 --today 2026-10-31")).unwrap_err().contains("twice"));
        assert!(parse(&args("check --repo /r")).is_err());
        assert!(parse(&args("check extra")).is_err());
        assert!(parse(&args("report --today 2026-10-30")).is_err());
        assert!(parse(&args("verify")).is_err());
        assert!(parse(&[]).is_err());
    }
}
