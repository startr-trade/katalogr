pub fn load_orders() -> String {
    sqlx::query("SELECT order_id, amount FROM orders")
}
