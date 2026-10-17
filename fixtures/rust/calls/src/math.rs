pub fn square(x: u32) -> u32 {
    x * x
}

pub fn cube(x: u32) -> u32 {
    x * square(x)
}
