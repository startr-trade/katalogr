CREATE TABLE orders (id bigint PRIMARY KEY, total numeric(12, 2));
CREATE TABLE public.audit (id bigint PRIMARY KEY);
CREATE VIEW order_ids AS SELECT id FROM orders;
