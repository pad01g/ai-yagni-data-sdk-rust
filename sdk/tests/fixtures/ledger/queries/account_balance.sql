-- 口座の残高を読む（アプリの固定クエリ）。
SELECT id, balance FROM accounts WHERE id = ?
