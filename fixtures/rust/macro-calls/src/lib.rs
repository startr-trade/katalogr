//! Calls inside macro arguments (FEAT-120).

mod shadow;

pub fn record(log: &mut Vec<i32>, n: i32) -> i32 {
    log.push(n);
    1
}

pub fn first() -> i32 {
    1
}

pub fn second() -> bool {
    true
}

pub fn third() -> i32 {
    3
}

pub fn fourth() -> i32 {
    4
}

pub fn fifth() -> i32 {
    5
}

pub fn sixth() -> i32 {
    6
}

pub fn seventh() -> i32 {
    7
}

pub fn eighth() -> i32 {
    8
}

pub fn spread() -> i32 {
    9
}

pub fn wrapped() -> i32 {
    10
}

pub fn json_arg() -> i32 {
    11
}

pub fn arrow_arg() -> i32 {
    12
}

pub struct Ledger {
    pub total: i32,
}

impl Ledger {
    pub fn settle(&self) -> i32 {
        self.total
    }
}

macro_rules! wrap {
    ($e:expr) => {
        $e
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t_assert_eq() {
        let mut l = Vec::new();
        assert_eq!(record(&mut l, 5), 1);
    }

    #[test]
    fn t_assert() {
        assert!(second());
    }

    #[test]
    fn t_vec() {
        let _v = vec![third(), 2];
    }

    #[test]
    fn t_format() {
        let _s = format!("{}", fourth());
    }

    #[test]
    fn t_println() {
        println!("{}", fifth());
    }

    #[test]
    fn t_matches() {
        let _ = matches!(sixth(), 6);
    }

    #[test]
    fn t_dbg() {
        dbg!(seventh());
    }

    #[test]
    fn t_method() {
        let g = Ledger { total: 1 };
        assert_eq!(g.settle(), 1);
    }

    #[test]
    fn t_nested() {
        assert_eq!(vec![eighth()], vec![8]);
    }

    #[test]
    fn t_multiline() {
        // Ünïcode before the call: ça va
        assert_eq!(
            1,
            spread()
        );
    }

    #[test]
    fn t_user_macro() {
        wrap!(wrapped());
    }

    #[test]
    fn t_unparsable() {
        let _ = matches!(1, 1 if second());
        let _j = json!({"a": json_arg()});
        let _a = dsl!(a => arrow_arg());
    }
}
