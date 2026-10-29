use ai_yagni_data_sdk::future::Future;

use crate::ledger::Ledger;

pub fn future(v: &Ledger) -> Future {
    Future::new("erasure")
        .why("退会者の個人情報を完全に消す。送金の記録（金額と相手の口座 ID）は残す")
        .reviewed("2026-10-28")
        .expires("2027-10")
        .requires([&v.pii_only_in_accounts, &v.transfers_refer_to_accounts])
        .then([&v.erased_accounts_hold_no_pii])
        .accepts("erase_hanako.sql", "送金の記録がある口座でも、氏名とメールを消せる")
        .realized_if(&v.accounts_can_be_erased)
        .conflicts_with("audit-trail", "監査は当事者の氏名とメールを 7 年残し、消去はそれを消す")
}
