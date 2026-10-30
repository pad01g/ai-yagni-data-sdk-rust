//! データ（SQL）の未来の変更を宣言し、その未来を閉じる変更を CI で止める SDK。
//!
//! 層は 3 つある。この crate は層0・層1と検査を持つ。
//!
//! - 層0 [`mysql`]: 本物の MySQL を docker で立て、コンテナの中の mysql CLI で話す。
//! - 層1 [`vocab`]: information_schema に写る事実と、「違反する行を返す SELECT が空」だけで書ける言明。
//! - 層2（この crate の外）: 層1を業務の形で拡張したフレームワーク。[`vocab::define`] で名前を付けた言明（語）を作る。
//! - 層3（この crate の外）: 未来の宣言（[`future::Future`]）。層2の語と差分 SQL で書く。
//!
//! 検査（[`check`]）は、今のスキーマと代表データが、宣言された未来へ向かえる状態であり続けることを確かめる。
//! [`cli::main`] を、利用者の crate の小さな bin から呼ぶ。
pub mod check;
pub mod cli;
pub mod date;
pub mod future;
pub mod lint;
pub mod mysql;
pub mod project;
pub mod report;
pub mod vocab;

/// 順序を保って重複を除く（最初に現れたものを残す）。
pub(crate) fn unique<T: PartialEq>(xs: impl IntoIterator<Item = T>) -> Vec<T> {
    let mut out = Vec::new();
    for x in xs {
        if !out.contains(&x) {
            out.push(x);
        }
    }
    out
}
