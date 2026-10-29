-- 佐藤さんに 5 USD（セント単位で 500）を付与する。
INSERT INTO transfers (id, from_id, to_id, amount, created_at, unit)
  VALUES ('00000000-0000-4000-8000-000000000901', NULL, '00000000-0000-4000-8000-0000000000a3', 500, '2027-01-01 00:00:00.000', 'USD');
INSERT INTO balances (account_id, unit, balance) VALUES ('00000000-0000-4000-8000-0000000000a3', 'USD', 500);
