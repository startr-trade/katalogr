DELIMITER //
CREATE PROCEDURE settle(IN cid INT)
BEGIN
  IF cid > 0 THEN
    UPDATE orders SET total = 0 WHERE customer_id = cid;
  END IF;
  CASE cid
    WHEN 1 THEN INSERT INTO audit (note) VALUES ('one');
    ELSE INSERT INTO audit (note) VALUES ('other');
  END CASE;
END//
DELIMITER ;
SELECT 1 FROM customers;
