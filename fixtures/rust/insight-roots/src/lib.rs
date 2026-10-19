//! Entry-point reasons: one symbol per rule.

use std::fmt;

pub fn public_api() {}

fn private_helper() {}

pub(crate) fn crate_visible() {}

#[test]
fn a_test() {}

#[cfg(test)]
mod tests {
    fn helper_in_tests() {}
}

#[no_mangle]
pub fn exported_by_name() {}

extern "C" fn abi_only() {}

pub struct Thing;

impl fmt::Display for Thing {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        Ok(())
    }
}
