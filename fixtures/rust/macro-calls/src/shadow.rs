//! A local `assert_eq` is not the standard macro, so its arguments are not read.

pub fn hidden() -> i32 {
    13
}

macro_rules! assert_eq {
    ($a:expr, $b:expr) => {
        $a + $b
    };
}

pub fn uses_shadow() {
    assert_eq!(hidden(), 1);
}
