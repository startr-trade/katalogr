CREATE TRIGGER customers_ad AFTER DELETE ON customers
BEGIN
  DELETE FROM orders WHERE customer_id = OLD.id;
END;
