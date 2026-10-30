-- アプリの DB の利用者（'app'@'%'）の権限。固定クエリ（queries/*.sql）が使う分だけを、表と列の単位で許す。
GRANT SELECT (id, balance) ON ledger.accounts TO 'app'@'%';
GRANT INSERT ON ledger.transfers TO 'app'@'%';
GRANT SELECT ON ledger.daily_stats TO 'app'@'%';
