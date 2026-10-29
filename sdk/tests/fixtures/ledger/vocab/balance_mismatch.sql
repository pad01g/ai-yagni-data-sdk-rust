-- 残高が送金の記録の畳み込みと一致しない口座。
SELECT a.id, a.balance,
       COALESCE((SELECT SUM(t.amount) FROM transfers t WHERE t.to_id = a.id), 0)
     - COALESCE((SELECT SUM(t.amount) FROM transfers t WHERE t.from_id = a.id), 0) AS expected
FROM accounts a
HAVING a.balance <> expected
