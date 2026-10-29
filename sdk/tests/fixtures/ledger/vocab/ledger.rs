//! 層2: 試料の語彙（素の define() で書く）。業務の言葉を、層1の言明（スキーマの事実と、違反を探す SQL ファイル）の
//! 合成として定義する。
use ai_yagni_data_sdk::vocab::{
    Event, Kind, SqlFile, Word, all, check_exists, column_exists, column_is, columns_named_only_in, define,
    foreign_key, guarded_against, shape, sql_empty, unique_on,
};

const BALANCE_MISMATCH: SqlFile = SqlFile::new("balance_mismatch.sql", include_str!("balance_mismatch.sql"));
const UNIT_BALANCE_MISMATCH: SqlFile =
    SqlFile::new("unit_balance_mismatch.sql", include_str!("unit_balance_mismatch.sql"));
const ERASED_WITH_PII: SqlFile = SqlFile::new("erased_with_pii.sql", include_str!("erased_with_pii.sql"));

pub struct Ledger {
    // 今
    pub amounts_are_whole_points: Word,
    pub balance_is_derivable: Word,
    pub transfers_are_keyed_by_request: Word,
    pub transfers_refer_to_accounts: Word,
    pub pii_only_in_accounts: Word,
    pub balance_floor_is_named: Word,
    // 未来
    pub transfers_have_unit: Word,
    pub balances_per_unit_match_transfers: Word,
    pub transfers_are_append_only: Word,
    pub accounts_can_be_erased: Word,
    pub erased_accounts_hold_no_pii: Word,
    pub accounts_have_credit_limit: Word,
}

impl Ledger {
    pub fn new() -> Self {
        Self {
            amounts_are_whole_points: define(
                "amounts_are_whole_points",
                "金額と残高は整数で持つ",
                all([
                    column_is("transfers", "amount", shape().kind(Kind::Integer).not_null()),
                    column_is("accounts", "balance", shape().kind(Kind::Integer).not_null()),
                ]),
            ),
            balance_is_derivable: define(
                "balance_is_derivable",
                "残高は送金の記録の畳み込みと一致する",
                sql_empty(BALANCE_MISMATCH, &[]),
            ),
            transfers_are_keyed_by_request: define(
                "transfers_are_keyed_by_request",
                "送金は依頼の冪等キーで一意",
                unique_on("transfers", &["id"]),
            ),
            transfers_refer_to_accounts: define(
                "transfers_refer_to_accounts",
                "送金は口座を ID で指す",
                all([
                    foreign_key("transfers", &["from_id"], "accounts", &["id"]),
                    foreign_key("transfers", &["to_id"], "accounts", &["id"]),
                ]),
            ),
            pii_only_in_accounts: define(
                "pii_only_in_accounts",
                "氏名とメールは accounts にだけある",
                columns_named_only_in(&["name", "email"], &["accounts"]),
            ),
            balance_floor_is_named: define(
                "balance_floor_is_named",
                "残高の下限は名前の付いた CHECK 制約で守られている",
                check_exists("accounts", "accounts_balance_nonnegative"),
            ),
            transfers_have_unit: define(
                "transfers_have_unit",
                "送金が通貨の単位を持つ",
                column_exists("transfers", "unit"),
            ),
            balances_per_unit_match_transfers: define(
                "balances_per_unit_match_transfers",
                "通貨ごとの残高が、その通貨の送金の畳み込みと一致する",
                all([unique_on("balances", &["account_id", "unit"]), sql_empty(UNIT_BALANCE_MISMATCH, &[])]),
            ),
            transfers_are_append_only: define(
                "transfers_are_append_only",
                "送金の記録は書き換えも削除もできない",
                all([guarded_against("transfers", Event::Update), guarded_against("transfers", Event::Delete)]),
            ),
            accounts_can_be_erased: define(
                "accounts_can_be_erased",
                "口座の個人情報を消しても、口座は残せる",
                all([
                    column_is("accounts", "name", shape().nullable()),
                    column_is("accounts", "email", shape().nullable()),
                    column_exists("accounts", "erased_at"),
                ]),
            ),
            erased_accounts_hold_no_pii: define(
                "erased_accounts_hold_no_pii",
                "消去済みの口座に個人情報が残っていない",
                sql_empty(ERASED_WITH_PII, &[]),
            ),
            accounts_have_credit_limit: define(
                "accounts_have_credit_limit",
                "口座ごとの与信枠がある",
                column_is("accounts", "credit_limit", shape().kind(Kind::Integer).not_null()),
            ),
        }
    }

    pub fn all(&self) -> Vec<Word> {
        [
            &self.amounts_are_whole_points,
            &self.balance_is_derivable,
            &self.transfers_are_keyed_by_request,
            &self.transfers_refer_to_accounts,
            &self.pii_only_in_accounts,
            &self.balance_floor_is_named,
            &self.transfers_have_unit,
            &self.balances_per_unit_match_transfers,
            &self.transfers_are_append_only,
            &self.accounts_can_be_erased,
            &self.erased_accounts_hold_no_pii,
            &self.accounts_have_credit_limit,
        ]
        .into_iter()
        .cloned()
        .collect()
    }
}
