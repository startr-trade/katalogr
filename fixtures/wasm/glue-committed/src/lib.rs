use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn greet() -> String {
    String::from("hello")
}

#[wasm_bindgen(js_name = sayHi)]
pub fn say_hi() -> String {
    String::from("hi")
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub fn tick() -> u32 {
    1
}

#[wasm_bindgen(start)]
fn boot() {}

#[wasm_bindgen]
pub fn unused_export() {}

#[wasm_bindgen]
pub struct Counter {
    value: u32,
}

#[wasm_bindgen]
impl Counter {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Counter {
        Counter { value: 0 }
    }

    pub fn increment(&mut self) {
        self.value += 1;
    }

    #[wasm_bindgen(getter)]
    pub fn value(&self) -> u32 {
        self.value
    }

    #[wasm_bindgen(setter)]
    pub fn set_value(&mut self, value: u32) {
        self.value = value;
    }

    fn helper(&self) {}
}

impl Default for Counter {
    fn default() -> Self {
        Counter::new()
    }
}

#[wasm_bindgen]
pub enum Color {
    Red,
    Green,
}
