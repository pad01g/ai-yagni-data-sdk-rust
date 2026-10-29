//! MySQL を 1 つ立て、`AI_YAGNI_MYSQL` にそのコンテナ名を入れてコマンドを走らせ、終わったら止める。
//! テスト全体で 1 つのサーバーを共有するために使う（`with-mysql cargo test`）。
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(ai_yagni_data_sdk::mysql::with_server(&args));
}
