-- 通貨ごとの残高が、その通貨の送金の畳み込みと一致しないもの（multi-currency の後のスキーマ）。
SELECT b.account_id, b.unit, b.balance,
       COALESCE((SELECT SUM(t.amount) FROM transfers t WHERE t.to_id = b.account_id AND t.unit = b.unit), 0)
     - COALESCE((SELECT SUM(t.amount) FROM transfers t WHERE t.from_id = b.account_id AND t.unit = b.unit), 0) AS expected
FROM balances b
HAVING b.balance <> expected
