INSERT INTO orders (customer_id) SELECT id FROM customers;
UPDATE orders SET total = 0 WHERE id = 1;
DELETE FROM audit WHERE id = 2;
SELECT o.id FROM orders o JOIN customers c ON c.id = o.customer_id;
CALL app.purge();
