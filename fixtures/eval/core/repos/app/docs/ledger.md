# Ledger

The ledger records every posted entry and reports the balance.

## Posting entries

An entry is accepted when the rate limiter admits it. Rejected entries are not recorded.

## Closing a period

Closing a period returns the final balance and clears the posted entries.
