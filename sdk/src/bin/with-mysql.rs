//! MySQL を 1 つ立て、`AI_YAGNI_MYSQL` にそのコンテナ名を入れてコマンドを走らせ、終わったら止める。
//! テスト全体で 1 つのサーバーを共有するために使う（`with-mysql cargo test`）。
use std::process::Command;

use ai_yagni_data_sdk::mysql::{ENV, Server};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((cmd, rest)) = args.split_first() else {
        eprintln!("usage: with-mysql <command> [args...]");
        std::process::exit(2);
    };
    let server = Server::start().unwrap_or_else(|e| {
        eprintln!("cannot start mysql: {e}");
        std::process::exit(2);
    });
    let status = Command::new(cmd).args(rest).env(ENV, server.container()).status();
    drop(server);
    match status {
        Ok(s) => std::process::exit(s.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("{cmd}: {e}");
            std::process::exit(2);
        }
    }
}
