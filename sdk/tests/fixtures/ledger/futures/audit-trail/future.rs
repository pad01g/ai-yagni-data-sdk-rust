use ai_yagni_data_sdk::future::Future;

use crate::ledger::Ledger;

pub fn future(v: &Ledger) -> Future {
    Future::new("audit-trail")
        .why("全変動を改ざんできない形で 7 年残し、誰と誰の間の送金だったかを答えられる")
        .reviewed("2026-10-28")
        .expires("2027-10")
        .requires([&v.balance_is_derivable, &v.transfers_are_keyed_by_request])
        .then([&v.transfers_are_append_only])
        .accepts("new_transfer.sql", "新しい送金は記録できる")
        .rejects("rewrite_amount.sql", "45000", "過去の送金の金額は書き換えられない")
        .rejects("rename_account.sql", "45000", "送金の当事者が誰だったかは書き換えられない")
        .realized_if(&v.transfers_are_append_only)
        .conflicts_with("erasure", "監査は当事者の氏名とメールを 7 年残し、消去はそれを消す")
}
