//! Test-only crash injection (FEAT-047 section 8). With the cargo feature `failpoints`,
//! `fail!("name")` aborts the process when `KATALOGR_FAILPOINT` equals `name`; without it the
//! macro expands to nothing. The store's two failpoints (`mid_renames`, `before_journal_clear`)
//! use the same variable.

#[cfg(feature = "failpoints")]
macro_rules! fail {
    ($name:expr) => {
        katalogr_store::failpoint::hit($name)
    };
}

#[cfg(not(feature = "failpoints"))]
macro_rules! fail {
    ($name:expr) => {};
}

pub(crate) use fail;
