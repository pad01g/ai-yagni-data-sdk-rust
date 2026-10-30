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
  - `use ... as 別名` の木は葉ごとに見ます。
  - `use` を書かずにコードの中でパスを書き切る（`ai_yagni_data_sdk::vocab::table_exists(..)`）のも、同じ規則で止めます。

## 層1の言明

| 言明 | 意味 |
|---|---|
| `table_exists` / `column_exists` | 表や列がある |
| `column_is(t, c, shape().kind(Kind::Integer).not_null())` | 列の型の大分類、COLUMN_TYPE、NULL を許すか |
| `columns_named_only_in(names, tables)` | その名前（か `_名前` で終わる名前）の列が、tables の外にない。大文字と小文字は区別しない |
| `unique_on` | 列の並びにちょうど一致する一意索引（主キーを含む）がある。式や列の先頭だけ（`email(10)`）を含む索引、ほかの列も含む索引は数えない |
| `foreign_key` | 外部キーがある（同じデータベースの中の表を指すものだけ） |
| `references_to(t, &[OnDelete::Cascade])` | t を指す外部キーが、どれも削除時にそのどれかの動きをする |
| `check_exists` / `guarded_against` | 名前の付いた CHECK 制約や、BEFORE トリガーがある |
| `sql_empty(SqlFile, names)` | SELECT（違反する行を返すように書く）が空。`{{名前}}` に表名・列名を差し込める。ファイルはちょうど 1 つの SELECT（`WITH` で始まってもよい）で、読み取りだけのトランザクションの中で流して巻き戻す |
| `all` / `any` / `not` | 合成。空の `all` と `any` は書き間違い |
| `define(name, meaning, fact)` | 層2: 業務の言葉に名前を付ける（語） |

言明を確かめた結果（`Verdict`）は 3 つのどれかです。

| 結果 | 意味 |
|---|---|
| `Pass` | 成り立つ |
| `Fail(理由)` | 成り立たない |
| `Error(理由)` | 確かめられなかった（クエリの失敗、docker の失敗、言明の書き間違い） |

`Error` は、成り立つとも成り立たないとも数えません。

- `not` で裏返しても `Error` のままです。
- `all` と `any` は、どれかが `Error` なら `Error` です（ほかが成り立っても）。
- 検査では、`Error` は失敗として報告します。宣言した衝突が現れたとは数えません。

組み立てたときに分かる書き間違い（`{{名前}}` の渡し忘れ、SELECT でない SQL、2 つ以上の文、空の合成）は、
確かめると `Error` を返し、検査の規則の節にも 1 度だけ出ます（`Fact::flaws`）。

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
   - 日付の形と範囲（`2026-02-31` や `2026-13` は通らない）、ファイルの有無、衝突の対称性が正しい。
   - 最後に読み直した日（`reviewed`）が今日より後でない。
   - 使う語が語彙に載っていて、語に書き間違いがない。
   - 層3のソースが規則に従っている。
2. schema.sql と fixtures が当たり、不変条件が成り立つ。
3. 固定クエリ（任意）が、今のスキーマで `PREPARE` できる。
4. 各未来について、期限・実現済み・前提・差分・then・accepts・rejects を確かめる。
   - probe（accepts と rejects）は 1 つずつ、差分を当てた新しい複製の上で流す。probe の DDL や COMMIT が、ほかの probe に漏れない。
   - accepts を流した後も、その複製の上で then が成り立たなければならない（埋め戻しただけのデータで then が素通りしないように）。
5. 宣言した衝突が、データの上で実際に現れる。
   - 確かめられなかったこと（docker の失敗、「今」が作れない、語が確かめられない）は、衝突が現れたとは数えない。
   - 組み合わせの結果は 1 度だけ計算し、`report` の表と衝突の検査で使い回す。
6. 共変更（`--changed` を渡したとき）。
   - 未来が見張るファイルが変わったのに、未来のディレクトリが変わっていなければ失敗する。
   - 語を使う未来は、語彙のファイルを暗黙に見張る。
   - パスは正規化して比べる（`..` や symlink を含む `--repo` でもよい）。プロジェクトの根・未来のディレクトリ・語彙・見張るファイルが `--repo` の下になければ失敗する。

利用者の crate では、小さな bin から `cli::main` を呼びます。

```rust
fn main() {
    let v = shop::Shop::new();
    ai_yagni_data_sdk::cli::main(project(&v), &["crate::shop"]);
}
```

```sh
datafutures check [--today YYYY-MM-DD] [--changed FILE [--repo DIR]]
datafutures report    # 未来の一覧、語彙の対応表、未来どうしの組み合わせの表
```

引数は厳密に読みます。知らないフラグ、値のないフラグ、2 度書いたフラグ、正しくない日付、`--changed` のない `--repo` は、
使い方の誤り（終了コード 2）です。`--flag=value` の形も使えます。

## MySQL のコンテナ

| 環境変数 | 意味 |
|---|---|
| `AI_YAGNI_MYSQL` | 既に立っているコンテナの名前。あればそれを使い、立ても止めもしない |
| `AI_YAGNI_MYSQL_TIMEOUT` | docker のコマンド 1 回の時間の上限（秒、既定 300）。過ぎたら止めてエラーにする（`Server::timeout` でも変えられる） |
| `AI_YAGNI_MYSQL_STALE_MINUTES` | 新しく立てるとき、これより古い（分、既定 120）コンテナを片付ける |

- 立てたコンテナには、持ち主（ホスト名とプロセス ID）と立てた時刻のラベルを付けます。
- 新しく立てるときに、持ち主のプロセスがもういないもの（同じホストのものだけ分かる）と、古すぎるものを片付けます。
  `AI_YAGNI_MYSQL` のコンテナは片付けません。
- `with-mysql`（`mysql::with_server`）と `cli::main` は、SIGINT と SIGTERM を受けたら、立てたコンテナを止めてから終わります。

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
ai-yagni-data-sdk = { git = "https://github.com/pad01g/ai-yagni-data-sdk-rust", tag = "v0.2.0" }
```

## ライセンス

MIT
