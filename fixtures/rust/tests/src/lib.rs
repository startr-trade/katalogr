//! Test marks: cases, suites and support code.

#[test]
fn a() {}

#[tokio::test]
async fn b() {}

#[cfg(not(test))]
mod live {
    pub fn run() {}
}

#[cfg(all(test, unix))]
mod tests {
    fn helper() {}

    #[test]
    fn c() {
        helper();
    }
}
