mod math;

use math::square;

macro_rules! trace {
    ($e:expr) => {
        $e
    };
}

pub struct Counter {
    n: u32,
}

impl Counter {
    pub fn new() -> Counter {
        Counter { n: 0 }
    }

    pub fn bump(&mut self) {
        self.n += 1;
    }

    pub fn run(&mut self) -> u32 {
        self.bump();
        Self::new();
        Counter::new();
        let s = square(self.n);
        trace!(s);
        math::cube(s) + local()
    }
}

fn local() -> u32 {
    crate::math::square(2)
}
