CREATE VIEW open_orders AS
  WITH recent AS (SELECT * FROM orders WHERE placed_at > now())
  SELECT o.id, c.email
  FROM recent o JOIN customers c ON c.id = o.customer_id
  WHERE o.id NOT IN (SELECT table_name FROM information_schema.tables);

CREATE MATERIALIZED VIEW order_totals AS
  SELECT customer_id, sum(total) AS total FROM orders GROUP BY customer_id;
