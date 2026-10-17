//! A tour of the item kinds.

pub mod shapes {
    pub struct Point {
        pub x: i32,
        pub y: i32,
    }

    pub fn origin() -> Point {
        Point { x: 0, y: 0 }
    }
}

pub struct Graph {
    nodes: Vec<u32>,
}

pub enum Kind {
    Leaf,
    Branch(u32),
    Pair { left: u32, right: u32 },
}

pub union Bits {
    int: u32,
    float: f32,
}

pub trait Visit {
    fn visit(&self) -> u32;
    fn name(&self) -> &'static str {
        "visit"
    }
}

impl Graph {
    pub fn add_node(&mut self, n: u32) {
        self.nodes.push(n);
    }
}

impl Visit for Graph {
    fn visit(&self) -> u32 {
        self.nodes.len() as u32
    }
}

pub const LIMIT: usize = 64;

pub static GREETING: &str = "hi";

pub type Id = u32;

macro_rules! twice {
    ($e:expr) => {
        $e + $e
    };
}

pub(crate) async fn fetch() {}

pub unsafe fn raw(p: *const u8) -> u8 {
    unsafe { *p }
}

fn helper<T>(value: T) -> T
where
    T: Copy,
{
    value
}

#[cfg(unix)]
fn platform() {}

#[cfg(windows)]
fn platform() {}
