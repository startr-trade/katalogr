pub trait Shape {
    fn area(&self) -> f64;
}

pub trait Named {
    fn label(&self) -> String {
        String::new()
    }
}
