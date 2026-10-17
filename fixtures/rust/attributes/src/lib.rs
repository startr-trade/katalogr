#![allow(dead_code)]

#[derive(Debug, Clone)]
pub struct Config {
    pub name: String,
}

#[derive(Debug)]
#[repr(u8)]
pub enum Mode {
    #[default]
    Fast,
    #[deprecated(note = "slow")]
    Slow,
}

#[inline]
#[must_use]
pub fn compute() -> u32 {
    1
}

#[cfg(feature = "extra")]
#[route(GET, "/health")]
pub fn health() {}

#[cfg(test)]
mod tests {
    #[test]
    fn works() {}
}
