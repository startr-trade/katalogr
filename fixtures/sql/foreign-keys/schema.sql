CREATE TABLE customers (id bigint PRIMARY KEY, email text NOT NULL);

CREATE TABLE orders (
  id bigint PRIMARY KEY,
  customer_id bigint NOT NULL REFERENCES customers (id),
  region_id bigint,
  CONSTRAINT fk_region FOREIGN KEY (region_id) REFERENCES regions (id)
);

ALTER TABLE orders ADD CONSTRAINT fk_audit FOREIGN KEY (customer_id) REFERENCES audit.customers (id);
