/// The kind of a bookkeeping entry.
pub enum EntryKind {
    Debit,
    Credit,
}

/// One posted line of the ledger.
pub struct Entry {
    pub kind: EntryKind,
    pub amount: i64,
}
