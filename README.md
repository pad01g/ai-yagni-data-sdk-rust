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
  - 先頭の `::`（`::ai_yagni_data_sdk::future`）は落として読みます。`extern crate ai_yagni_data_sdk as s;` の `s::..` は、元の名前で読みます。
  - `super::` で始まるパスは使えません（`crate::` から書きます）。crate 全体を許す層2では使えます。
  - `#[path = ..]` と `include!` は使えません（読み込んだファイルを検査が見ないため）。未来のディレクトリの `.rs` はすべて見ます（`mod x;` で分けてもよい）。
  - 字句だけで見るので、マクロが組み立てるパスは見つけられません。

## 層1の言明

| 言明 | 意味 |
|---|---|
| `table_exists` / `column_exists` | 表や列がある |
| `column_is(t, c, shape().kind(Kind::Integer).not_null())` | 列の型の大分類、COLUMN_TYPE、NULL を許すか |
| `columns_named_only_in(names, tables)` | その名前（か `_名前` で終わる名前）の列が、tables の外にない。大文字と小文字は区別しない |
| `unique_on` | 列の集合にちょうど一致する一意索引（主キーを含む）がある。順序と大文字小文字は問わない。式や列の先頭だけ（`email(10)`）を含む索引、ほかの列も含む索引は数えない |
| `unique_within` | 列の一部（全部でもよい）だけでできた一意索引がある（つまり、その列の組は一意）。「同じ組を何行でも持てる」は `not(unique_within(..))` で書く（`not(unique_on(..))` は、もっと狭い一意索引を見落とす） |
| `unique_on_not_null` | `unique_on` に加えて、どの列も NULL を持てない（一意索引は NULL を何行でも許すため） |
| `foreign_key` | 外部キーがある（同じデータベースの中の表を指すものだけ） |
| `references_to(t, &[OnDelete::Cascade])` | t を指す外部キーが、どれも削除時にそのどれかの動きをする |
| `check_exists` / `guarded_against` | 名前の付いた、効いている（`NOT ENFORCED` でない）CHECK 制約や、BEFORE トリガーがある。中身は見ない（中身は probe で確かめる） |
| `sql_empty(SqlFile, names)` | SELECT（違反する行を返すように書く）が空。`{{名前}}` に表名・列名を差し込める。ファイルはちょうど 1 つの SELECT（`WITH` で始まってもよい）で、読み取りだけのトランザクションの中で流して巻き戻す。文は 16 進のリテラルにしてサーバーの `PREPARE` に渡すので、mysql CLI のコマンド（`\g`）や版のコメント（`/*!99999 */`）で 2 つ目の文を忍び込ませることはできない |
| `all` / `any` / `not` | 合成。空の `all` と `any` は書き間違い |
| `seq` | 前から順に確かめ、成り立たないもの（確かめられないものも）があればそこで止める。構造の言明（列がある）を前に、それを読む SQL を後ろに並べる |
| `Fact::composed(says, parts, check)` | 材料を持つ独自の合成。材料は語の依存（`words()`）と書き間違いの報告に残る（`Fact::new` では残らない） |
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

独自の言明で結果の行を読むときは `Row::try_get` を使います（列の名前の書き間違いが panic でなく `Error` になります）。

表・列・索引・制約・トリガー・外部キーの言明は、information_schema の写しを 1 回の問い合わせで読み、
そのデータベースへ次に何かを流すまで使い回します。

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
    // エラーの文が制約の名前（トリガーなら SIGNAL の文の一部）を含むことまで確かめる
    .rejects_with("negative_price.sql", "3819", "products_price_positive", "価格は正")
    .realized_if(&v.prices_carry_currency)        // 今これが成り立てば、その未来はもう来ている
    .conflicts_with("other-future", "なぜ両立しないか")
    .watches("submodules/proxy/queries/product_lock.sql")  // 見張るファイル。ディレクトリは dir/*
```

- `rejects` の期待は、エラーの番号か SQLSTATE です。多くのエラーが共有する `HY000` だけを期待すると、検査は警告を出します（番号か `rejects_with` を使う）。
- 見張るファイルはプロジェクトからの相対パスで、なければ規則の節で失敗します。ディレクトリは `dir/*` と書きます（`dir` や `dir/` は書き間違い）。
- 差分を当てると固定クエリが `PREPARE` できなくなる未来は、そのクエリ（か、そのディレクトリの `dir/*`）を見張らなければなりません。

未来どうしは矛盾してかまいません。各未来は、ほかの未来とは独立に「今」（schema.sql と fixtures）の複製へ当てて検査します。
衝突を宣言した組は、どちらの順で続けて当てても、どこかで失敗しなければなりません。

## プロジェクトの宣言

```rust
Project::new(root())
    .vocabulary("datafutures/src/shop.rs")
    .words(v.all())
    .invariants([&v.prices_are_whole_minor_units])
    .queries("submodules/proxy/queries")                              // 固定クエリ（1 ファイルに 1 文）
    .grants("submodules/proxy/grants.sql", "proxy", Some("shop"))    // 固定クエリの利用者の権限（任意）
    .accepts("probes/new_customer.sql", "今の上で受け入れる書き込み")   // 今の probe（任意）
    .rejects_with("probes/negative_stock.sql", "3819", "products_stock_nonnegative", "在庫は 0 を下回らない")
    .future(multi_currency::future(v))
```

- 固定クエリは、今のスキーマで `PREPARE` できなければなりません。各未来の差分を当てた後にも `PREPARE` し直します。
- 権限のファイル（GRANT の文だけ）を渡すと、今の上で、その権限だけを持つ使い捨ての利用者（乱数のパスワード）を作り、
  固定クエリをその利用者でも `PREPARE` します（列の権限の不足が分かります）。
  - ファイルの中の利用者（`'proxy'@'%'` の `proxy`。引用してもしなくてもよい）を、使い捨ての利用者に書き換えます。
  - 3 つ目の引数の名前で修飾した表（`shop.orders`、`` `shop`.* ``）の修飾を、使い捨てのデータベースの名前に書き換えます。
    ファイルに `{{database}}` と書いてもかまいません。
  - 利用者は検査の終わりに消します。
- 今の probe は、probe ごとに新しく作った今の複製の上で流します。CHECK 制約やトリガーの中身を、名前だけでなく今の上で確かめられます。
  accepts の後も不変条件が成り立たなければなりません。

## 検査

[`check`](sdk/src/check.rs) が確かめることは次のとおりです。

1. 規則が守られている。
   - 未来のディレクトリと宣言が 1 対 1 になっている。
   - 日付の形と範囲（`2026-02-31` や `2026-13` は通らない）、ファイルの有無、衝突の対称性が正しい。
   - 最後に読み直した日（`reviewed`）が今日より後でない。
   - 使う語が語彙に載っていて、語に書き間違いがない。
   - 層3のソースが規則に従っている。
   - 見張るファイルがあり、正しく書かれている。
   - SQL のファイル（スキーマ・代表データ・差分・probe）が、データベースの外を変える文を持たない
     （`SET GLOBAL`・`SET PERSIST`・`USE`・`CREATE USER`・`GRANT`・`DROP DATABASE` など）。検査はデータベースだけを分け、
     サーバーは分けないので、こうした文はほかの検査に漏れる。字句だけで見るので、ストアドプロシージャの本体の中までは見ない。
2. schema.sql と fixtures が当たり、不変条件が成り立つ。
3. 固定クエリ（任意）が、今のスキーマで `PREPARE` できる。権限のファイルがあれば、その利用者でも `PREPARE` できる。
4. 今の probe（任意）が期待どおりに受け入れ、拒む。
5. 各未来について、期限・実現済み・前提・差分・then・固定クエリ・accepts・rejects を確かめる。
   - probe（accepts と rejects）は 1 つずつ、差分を当てた新しい複製の上で流す。probe の DDL や COMMIT が、ほかの probe に漏れない。
   - accepts を流した後も、その複製の上で then が成り立たなければならない（埋め戻しただけのデータで then が素通りしないように）。
   - 差分の後に `PREPARE` できなくなった固定クエリは、その未来が見張っていなければならない。
6. 宣言した衝突が、データの上で実際に現れる。
   - 確かめられなかったこと（docker の失敗、「今」が作れない、語が確かめられない）は、衝突が現れたとは数えない。
   - 組み合わせの結果は 1 度だけ計算し、`report` の表と衝突の検査で使い回す。
7. 共変更（`--changed` を渡したとき）。
   - 未来が見張るファイルが変わったのに、未来のディレクトリが変わっていなければ失敗する。
   - 語を使う未来は、語彙のファイルを暗黙に見張る。
   - 変わったパスがディレクトリなら（`git diff` は、サブモジュールの中のファイルでなくサブモジュールのパスだけを出す）、
     その下の見張るファイルも変わったとみなす。
   - 変わったファイルの一覧は `git diff --name-only --no-renames BASE...HEAD` で作る。`--no-renames` がないと、
     名前を変えたファイルの元の名前が出ず、それを見張る未来が見落とされる。
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
`check` は警告（`warn` の行）も出しますが、警告だけでは失敗しません。

複製（データベースを作り、スキーマ・代表データ・差分・probe を流す）は 1 回の `docker exec` で作り、probe の複製はいくつかを並べて作ります。
スクリプトは 1 本ずつ別のコネクションで流すので、`SET FOREIGN_KEY_CHECKS = 0` のようなセッションの状態は次のファイルに漏れません。

## MySQL のコンテナ

| 環境変数 | 意味 |
|---|---|
| `AI_YAGNI_MYSQL` | 既に立っているコンテナの名前。あればそれを使い、立ても止めもしない |
| `AI_YAGNI_MYSQL_TIMEOUT` | docker のコマンド 1 回の時間の上限（秒、既定 300）。過ぎたら、サーバーの上のその問い合わせを `KILL` し、クライアントを止めてエラーにする（`Server::timeout` でも変えられる） |
| `AI_YAGNI_MYSQL_STALE_MINUTES` | 新しく立てるとき、持ち主が生きているか確かめられないコンテナは、これより古い（分、既定 720）ものだけを片付ける |

- mysql CLI は `--binary-mode` で走らせます。`\g`（送る）や `\!`（シェル）などの CLI のコマンドは、どのスクリプトでもエラーになります。
- 時刻帯は `--init-command` で UTC にします。エラーの行番号は、ファイルの行番号のままです（`change.sql:3: ...`）。
- 接続の失敗（2000〜2999 のクライアントのエラー、2013 Lost connection、2006 gone away など）は、SQL の判定でなく「確かめられなかった」と数えます。
- 時間切れの問い合わせは、`--init-command` でコネクションに付けた印を performance_schema で探し、`KILL` で止めます。
  データベースを消すときは、表の固定を短く（10 秒）しか待ちません。
- 立てたコンテナには、持ち主（ホスト名・プロセス ID・PID 名前空間・起動の ID）と立てた時刻のラベルを付けます。
  新しく立てるときに片付けるのは、次のものだけです（迷ったら残します）。
  - 持ち主が同じカーネルの同じ PID 名前空間にいて、そのプロセスがもういないもの（生きていれば古くても残す）。
  - 持ち主が生きているか確かめられず（別のホスト、別のコンテナ、古い版のラベル）、`AI_YAGNI_MYSQL_STALE_MINUTES` より古いもの。
  - ただし、このプロセスか、見えるほかのプロセスの `AI_YAGNI_MYSQL` が指すコンテナは片付けません。
- `with-mysql`（`mysql::with_server`）と `cli::main` は、SIGINT と SIGTERM を受けたら片付けてから終わります。
  - `with-mysql` は、受けたシグナルを走らせているコマンドに渡し、それが終わるのを待ちます（長くても 60 秒）。
  - 途中の `docker run` は別のプロセスグループで走らせ、それが終わるのを待ってからコンテナを消します（後から立ったコンテナを取り残さない）。
  - 既に立っているサーバーの上に作ったデータベースと利用者も消します。
  - 2 度目のシグナルでは待たずに終わります。
- 使い終えた複製（`Scratch`）は、いくつかたまったときと `Server` を drop したときに、まとめて消します。
- 複製を 1 回の `docker exec` で作るために、コンテナの中に `sh`・`sed`・`base64`・`mktemp` が要ります（公式の mysql のイメージにはあります）。

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
ai-yagni-data-sdk = { git = "https://github.com/pad01g/ai-yagni-data-sdk-rust", tag = "v0.3.0" }
```

## 0.2 から 0.3 への変更（互換でない点）

- `unique_on` は列の順序と大文字小文字を問わなくなりました（一意性は列の集合で決まるため）。
  「同じ組を何行でも持てる」は `not(unique_on(..))` でなく `not(unique_within(..))` で書いてください。
- `check_exists` は `NOT ENFORCED` の CHECK 制約を数えなくなりました。
- 構造体に公開のフィールドが増えました（構造体の式で作っていたコードは直す必要があります）。
  `SqlError::line`、`Rejection::naming`、`Report::warnings`、`Project::grants`・`accepts`・`rejects`。
  `Scratch` は非公開のフィールドを持つようになり、構造体の式では作れません（`Scratch::create` か `Scratch::build` を使う）。
- 規則の節で新しく失敗するもの: ない見張るファイル、`dir` や `dir/` と書いた見張り、データベースの外を変える SQL の文、
  層3の `super::`・`#[path]`・`include!`、未来のディレクトリの `future.rs` 以外の `.rs` の規則違反。
- 未来の節で新しく失敗するもの: 差分の後に `PREPARE` できなくなった、見張っていない固定クエリ。
- 2000〜2999 のエラー（と接続のエラー）を期待する rejects は、もう通りません（確かめられなかったと数えます）。
- `AI_YAGNI_MYSQL_STALE_MINUTES` の既定は 120 から 720 になり、生きている持ち主のコンテナは古くても片付けません。
- mysql CLI のコマンド（`\g`、`\!`、`\G` など）を含むスクリプトは、エラーになります。

## ライセンス

MIT
