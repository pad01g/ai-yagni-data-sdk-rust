-- 試料: ポイント台帳。スキーマの正本（宣言的）。
-- 時刻はすべて UTC の DATETIME(3)。

CREATE TABLE accounts (
  id         CHAR(36)     NOT NULL,
  name       VARCHAR(100) NOT NULL,
  email      VARCHAR(255) NOT NULL,
  balance    BIGINT       NOT NULL DEFAULT 0,
  created_at DATETIME(3)  NOT NULL,
  PRIMARY KEY (id),
  CONSTRAINT accounts_balance_nonnegative CHECK (balance >= 0)
);

-- 送金と付与（from_id が NULL）。id は依頼の冪等キー。
CREATE TABLE transfers (
  id         CHAR(36)    NOT NULL,
  from_id    CHAR(36)    NULL,
  to_id      CHAR(36)    NOT NULL,
  amount     BIGINT      NOT NULL,
  created_at DATETIME(3) NOT NULL,
  PRIMARY KEY (id),
  CONSTRAINT transfers_from FOREIGN KEY (from_id) REFERENCES accounts (id),
  CONSTRAINT transfers_to FOREIGN KEY (to_id) REFERENCES accounts (id),
  CONSTRAINT transfers_amount_positive CHECK (amount > 0)
);

-- 日次集計（使い捨て。transfers から作り直せる）。
CREATE TABLE daily_stats (
  day       DATE   NOT NULL,
  transfers INT    NOT NULL,
  volume    BIGINT NOT NULL,
  PRIMARY KEY (day)
);
