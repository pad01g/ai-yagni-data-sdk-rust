-- 残高 0 の口座は、送金の記録なしに作れる（残高は送金の畳み込みと一致したまま）。
INSERT INTO accounts (id, name, email, balance, created_at)
  VALUES ('00000000-0000-4000-8000-0000000000a4', 'Carol', 'carol@example.com', 0, '2026-01-03 00:00:00.000');
