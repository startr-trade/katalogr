use crate::ledger::Ledger;

/// Summarise the audit trail of a ledger as its current balance.
pub fn audit_trail(ledger: &Ledger) -> i64 {
    ledger.balance()
}
