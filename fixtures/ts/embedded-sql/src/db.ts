export function loadOrders(customer: number) {
  return db.query(`SELECT id, total FROM orders WHERE customer_id = ${customer}`);
}

export function ordersUrl() {
  return fetch("/orders");
}

export const label = "select an option from the list";
