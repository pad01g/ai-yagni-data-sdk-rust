-- 個人情報を NULL にできるようにし、消した時刻を残す。口座の行（ID）は送金の相手として残す。
ALTER TABLE accounts
  MODIFY name VARCHAR(100) NULL,
  MODIFY email VARCHAR(255) NULL,
  ADD COLUMN erased_at DATETIME(3) NULL;
