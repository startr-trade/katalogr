ALTER TABLE orders ENABLE ROW LEVEL SECURITY;

CREATE POLICY own_rows ON orders FOR SELECT TO app_user
  USING (customer_id = (SELECT id FROM customers WHERE email = current_user));
