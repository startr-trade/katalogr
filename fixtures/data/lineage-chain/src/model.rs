/// The order as the code holds it.
pub struct Order {
    pub id: String,
    pub total: f64,
    pub order_date: String,
    pub internal: bool,
    pub lines: Vec<Line>,
}

pub struct Line {
    pub sku: String,
}
