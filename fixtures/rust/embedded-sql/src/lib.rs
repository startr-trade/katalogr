pub fn load_orders(customer: i64) -> String {
    sqlx::query("SELECT id, total FROM orders WHERE customer_id = $1")
}

pub fn purge() {
    run("CALL app.purge()");
}

pub fn orders_url() -> String {
    http::get("/orders")
}

pub fn greeting() -> &'static str {
    "select an option from the list"
}
