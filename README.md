# ai-yagni-data-sdk-rust

データ（SQL）の未来の変更を宣言し、その未来を閉じる変更を CI で止める SDK です。

- 「必要とされるデータのあり方」の未来を予想します。ただし、その未来のためのデータを今は作りません。
- 代わりに、その未来へ向かえなくなる変更（スキーマでもデータでも）が入ったら、CI で失敗させます。
- 検査は、docker で立てた本物の MySQL（`mysql:8.4.6`）の上で、コンテナの中の mysql CLI を使って行います。

TypeScript 版の実験（`ai-yagni-data`）を Rust に移したものです。

## 層

| 層 | 置き場所 | 中身 |
|---|---|---|
| 0 | この crate の `mysql` | MySQL を docker で立て、`docker exec` の mysql CLI で話す。ポートは公開しない |
| 1 | この crate の `vocab` | 基礎語彙。information_schema に写る事実と、「違反する行を返す SELECT が空」だけで書ける言明 |
| 2 | 利用者（例: [ai-yagni-data-sdk-example-extension-rust](https://github.com/pad01g/ai-yagni-data-sdk-example-extension-rust)） | 層1を業務の形で拡張したフレームワーク。`define()` で名前を付けた言明（語）を作る |
| 3 | 利用者のプロジェクト（例: [ai-yagni-data-sdk-example-backend-rust](https://github.com/pad01g/ai-yagni-data-sdk-example-backend-rust)） | 未来の宣言。層2の語と差分 SQL で書く |

層の規則:

- 層3（`Future`）は層2の語（`Word`）しか受け取りません。層1の言明を直接使えないことは、型が守ります。
- 層2・層3のソースは、SQL を文字列で持ちません（`lint` が字句を読んで確かめます）。
  - 層2は `.sql` ファイルを `include_str!` で埋め込みます（`SqlFile`）。
  - 層3はファイル名で渡します。
- 層3のソース（`futures/<id>/future.rs`）が `use` してよいのは、`ai_yagni_data_sdk::future` と語彙のモジュールだけです。

## 層1の言明

| 言明 | 意味 |
|---|---|
| `table_exists` / `column_exists` | 表や列がある |
| `column_is(t, c, shape().kind(Kind::Integer).not_null())` | 列の型の大分類、COLUMN_TYPE、NULL を許すか |
| `columns_named_only_in(names, tables)` | その名前（か `_名前` で終わる名前）の列が、tables の外にない |
| `unique_on` | 列の並びに一意索引（主キーを含む）がある |
| `foreign_key` | 外部キーがある |
| `references_to(t, OnDelete::Cascade)` | t を指す外部キーが、どれも削除時にその動きをする |
| `check_exists` / `guarded_against` | 名前の付いた CHECK 制約や、BEFORE トリガーがある |
| `sql_empty(SqlFile, names)` | SELECT（違反する行を返すように書く）が空。`{{名前}}` に表名・列名を差し込める |
| `all` / `any` / `not` | 合成 |
| `define(name, meaning, fact)` | 層2: 業務の言葉に名前を付ける（語） |

## 未来の宣言

```rust
Future::new("multi-currency")
    .why("円以外の通貨も扱う")
    .reviewed("2026-10-29")       // 最後に読み直した日
    .expires("2027-10")           // この月を過ぎたら失敗する（残すか決め直す）
    .requires([&v.prices_are_whole_minor_units])  // 今、成り立っていなければならない前提
    .then([&v.prices_carry_currency])             // change.sql を当てた後に成り立つこと
    .accepts("usd_product.sql", "ドル建ての商品を登録できる")
    .rejects("bad_currency.sql", "3819", "通貨コードの形を守る")
    .realized_if(&v.prices_carry_currency)        // 今これが成り立てば、その未来はもう来ている
    .conflicts_with("other-future", "なぜ両立しないか")
```

未来どうしは矛盾してかまいません。各未来は、ほかの未来とは独立に「今」（schema.sql と fixtures）の複製へ当てて検査します。
衝突を宣言した組は、どちらの順で続けて当てても、どこかで失敗しなければなりません。

## 検査

[`check`](sdk/src/check.rs) が確かめることは次のとおりです。

1. 規則が守られている。
   - 未来のディレクトリと宣言が 1 対 1 になっている。
   - 日付の形、ファイルの有無、衝突の対称性が正しい。
   - 使う語が語彙に載っている。
   - 層3のソースが規則に従っている。
2. schema.sql と fixtures が当たり、不変条件が成り立つ。
3. 固定クエリ（任意）が、今のスキーマで `PREPARE` できる。
4. 各未来について、期限・実現済み・前提・差分・then・accepts・rejects を確かめる。
5. 宣言した衝突が、データの上で実際に現れる。
6. 共変更（`--changed` を渡したとき）。
   - 未来が見張るファイルが変わったのに、未来のディレクトリが変わっていなければ失敗する。
   - 語を使う未来は、語彙のファイルを暗黙に見張る。

利用者の crate では、小さな bin から `cli::main` を呼びます。

```rust
fn main() {
    let v = shop::Shop::new();
    ai_yagni_data_sdk::cli::main(project(&v), &["crate::shop"]);
}
```

```sh
datafutures check [--today YYYY-MM-DD] [--changed FILE --repo DIR]
datafutures report    # 未来の一覧、語彙の対応表、未来どうしの組み合わせの表
```

## Verus

[`verified/`](verified/src/lib.rs) は Verus で証明する crate です。
SQL に差し込む識別子の判定 `is_identifier` が、仕様（英字か `_` で始まり、英数字と `_` だけが続く、64 バイト以下）とちょうど一致することを証明しています。
通常の `cargo build` では `verus!` の中の仕様と証明が消え、実行コードだけが残ります。

## 動かし方（ホストには docker 以外何も入れない）

```sh
make test     # テスト（MySQL のコンテナを 1 つ立てて共有する）
make lint     # rustfmt と clippy
make verify   # Verus で証明を確かめる（amd64 のイメージ。arm64 のホストではエミュレーションで遅い）
make ci       # 全部
```

テストを走らせるコンテナには、MySQL のコンテナを立てるために、ホストの docker ソケットを渡します。

## 使い方（Cargo）

```toml
[dependencies]
ai-yagni-data-sdk = { git = "https://github.com/pad01g/ai-yagni-data-sdk-rust", tag = "v0.1.0" }
```

## ライセンス

MIT
