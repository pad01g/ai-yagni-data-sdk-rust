-- 消去済みなのに個人情報が残っている口座（erasure の後のスキーマ）。
SELECT id FROM accounts WHERE erased_at IS NOT NULL AND (name IS NOT NULL OR email IS NOT NULL)
