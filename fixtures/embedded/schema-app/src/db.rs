pub fn load_orders() -> String {
    sqlx::query("SELECT id FROM orders")
}
