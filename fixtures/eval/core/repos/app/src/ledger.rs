use crate::entry::{Entry, EntryKind};
use crate::rate_limit::RateLimiter;

/// A ledger holds posted entries and answers balance questions.
pub struct Ledger {
    entries: Vec<Entry>,
    limiter: RateLimiter,
}

impl Ledger {
    /// Post one entry to the ledger after the rate limiter admits it.
    pub fn post_entry(&mut self, entry: Entry) -> bool {
        if !self.limiter.try_acquire() {
            return false;
        }
        self.entries.push(entry);
        true
    }

    /// The running balance of all posted entries.
    pub fn balance(&self) -> i64 {
        self.entries
            .iter()
            .map(|e| match e.kind {
                EntryKind::Debit => -e.amount,
                EntryKind::Credit => e.amount,
            })
            .sum()
    }

    /// Close the accounting period and return the final balance.
    pub fn close_period(&mut self) -> i64 {
        let total = self.balance();
        self.entries.clear();
        total
    }
}
