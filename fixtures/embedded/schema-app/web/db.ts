export function loadOrders() {
  return db.query("SELECT id FROM orders");
}
