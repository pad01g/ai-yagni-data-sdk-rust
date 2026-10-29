-- 送金に通貨の単位を足し、既存はすべて PT とする。残高は口座 × 通貨の表に移す（accounts.balance は PT の残高として残す）。
ALTER TABLE transfers ADD COLUMN unit CHAR(3) NOT NULL DEFAULT 'PT';

CREATE TABLE balances (
  account_id CHAR(36) NOT NULL,
  unit       CHAR(3)  NOT NULL,
  balance    BIGINT   NOT NULL,
  PRIMARY KEY (account_id, unit),
  CONSTRAINT balances_account FOREIGN KEY (account_id) REFERENCES accounts (id),
  CONSTRAINT balances_nonnegative CHECK (balance >= 0)
);

INSERT INTO balances (account_id, unit, balance) SELECT id, 'PT', balance FROM accounts;
