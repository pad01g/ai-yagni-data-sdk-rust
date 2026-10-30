use ai_yagni_data_sdk::future::Future;

use crate::ledger::Ledger;

pub fn future(v: &Ledger) -> Future {
    Future::new("overdraft")
        .why("口座ごとの与信枠の範囲でマイナス残高を許す")
        .reviewed("2026-10-28")
        .expires("2027-04")
        .requires([&v.amounts_are_whole_points, &v.balance_floor_is_named])
        .then([&v.accounts_have_credit_limit])
        .accepts("within_credit.sql", "与信枠の範囲ならマイナスにできる")
        .rejects_with("beyond_credit.sql", "3819", "accounts_within_credit", "与信枠を超えるマイナスは拒む")
        .realized_if(&v.accounts_have_credit_limit)
        // 残高を読む固定クエリ。マイナスの残高を返すようになる
        .watches("queries/account_balance.sql")
}
