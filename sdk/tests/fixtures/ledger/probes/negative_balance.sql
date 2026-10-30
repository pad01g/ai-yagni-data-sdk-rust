-- 残高は 0 を下回らない（CHECK 制約の中身を今の上で確かめる）。
UPDATE accounts SET balance = -1 WHERE id = '00000000-0000-4000-8000-0000000000a3';
