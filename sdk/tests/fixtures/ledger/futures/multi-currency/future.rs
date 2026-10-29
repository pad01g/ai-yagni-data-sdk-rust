use ai_yagni_data_sdk::future::Future;

use crate::ledger::Ledger;

pub fn future(v: &Ledger) -> Future {
    Future::new("multi-currency")
        .why("ポイント以外の通貨（円・ドル）も扱う。既存の送金はすべてポイント（PT）として埋め戻せる")
        .reviewed("2026-10-28")
        .expires("2027-10")
        .requires([&v.amounts_are_whole_points, &v.balance_is_derivable, &v.transfers_refer_to_accounts])
        .then([&v.balances_per_unit_match_transfers])
        .accepts("usd_grant.sql", "ドル建ての付与と残高を持てる")
        .realized_if(&v.transfers_have_unit)
}
