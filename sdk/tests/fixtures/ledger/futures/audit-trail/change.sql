-- 送金を追記のみにし、口座の氏名・メールを書き換え不能にする（当事者が誰だったかも監査の対象）。
CREATE TRIGGER transfers_no_update BEFORE UPDATE ON transfers FOR EACH ROW
  SIGNAL SQLSTATE '45000' SET MESSAGE_TEXT = 'transfers are append-only';
CREATE TRIGGER transfers_no_delete BEFORE DELETE ON transfers FOR EACH ROW
  SIGNAL SQLSTATE '45000' SET MESSAGE_TEXT = 'transfers are append-only';
CREATE TRIGGER accounts_no_delete BEFORE DELETE ON accounts FOR EACH ROW
  SIGNAL SQLSTATE '45000' SET MESSAGE_TEXT = 'accounts are kept for the audit';

DELIMITER $$
CREATE TRIGGER accounts_identity_fixed BEFORE UPDATE ON accounts FOR EACH ROW
BEGIN
  IF NOT (NEW.name <=> OLD.name) OR NOT (NEW.email <=> OLD.email) THEN
    SIGNAL SQLSTATE '45000' SET MESSAGE_TEXT = 'account identity is kept for the audit';
  END IF;
END$$
DELIMITER ;
