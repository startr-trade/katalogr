pub mod traits;

use std::fmt;
use traits::Shape;

pub struct Circle {
    pub r: f64,
}

pub struct Wrapper<T>(pub T);

impl Circle {
    pub fn new(r: f64) -> Circle {
        Circle { r }
    }
}

impl Shape for Circle {
    fn area(&self) -> f64 {
        self.r * self.r * 3.14
    }
}

impl<T> Shape for Wrapper<T> {
    fn area(&self) -> f64 {
        0.0
    }
}

impl fmt::Display for Circle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "circle")
    }
}

impl crate::traits::Named for Circle {}
