CREATE TABLE customers (
  id bigserial PRIMARY KEY,
  email text NOT NULL,
  name varchar(80)
);

CREATE TABLE public.orders (
  id bigserial PRIMARY KEY,
  customer_id bigint NOT NULL,
  total numeric(12, 2) DEFAULT 0,
  placed_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX orders_customer_idx ON orders (customer_id);
COMMENT ON TABLE orders IS 'Customer orders';
COMMENT ON COLUMN orders.total IS 'Order total';
