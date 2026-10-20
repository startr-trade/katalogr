use wasm_bindgen::prelude::*;

/// Compute a price quote for a quantity of items, applying the volume discount.
#[wasm_bindgen]
pub fn price_quote(quantity: u32) -> u32 {
    quantity * 100 - discount(quantity)
}

/// The volume discount granted for a quantity.
fn discount(quantity: u32) -> u32 {
    if quantity > 10 { quantity * 10 } else { 0 }
}
