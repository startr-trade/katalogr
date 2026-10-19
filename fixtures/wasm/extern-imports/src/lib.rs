use wasm_bindgen::prelude::*;

#[wasm_bindgen(module = "/js/host.js")]
extern "C" {
    fn now() -> f64;

    type Clock;

    #[wasm_bindgen(method)]
    fn tick(this: &Clock);
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn log(s: &str);

    fn alert(s: &str);
}

#[wasm_bindgen]
pub fn report() {
    log("report");
    alert("done");
}
