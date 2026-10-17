use crate::util::inner::{self, helper};
use super::util::Marker as M;

pub struct Circle;

pub fn draw() {
    helper();
    inner::other();
    M::mark();
}
