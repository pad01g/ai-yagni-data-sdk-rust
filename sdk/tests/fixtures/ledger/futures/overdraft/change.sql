-- 下限 0 の制約を外し、口座ごとの与信枠までのマイナスを許す。
ALTER TABLE accounts
  DROP CHECK accounts_balance_nonnegative,
  ADD COLUMN credit_limit BIGINT NOT NULL DEFAULT 0,
  ADD CONSTRAINT accounts_within_credit CHECK (balance >= -credit_limit);
