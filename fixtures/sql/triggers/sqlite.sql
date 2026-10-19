CREATE TRIGGER orders_log AFTER INSERT ON orders
BEGIN
  INSERT INTO audit (order_id) VALUES (NEW.id);
END;
