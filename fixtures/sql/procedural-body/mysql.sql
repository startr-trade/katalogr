CREATE PROCEDURE recount()
BEGIN
  UPDATE customers SET orders = (SELECT COUNT(*) FROM orders WHERE orders.customer_id = customers.id);
END;
