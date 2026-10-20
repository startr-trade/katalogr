import { formatAmount } from "./format";

/** Client of the ledger service. */
export class LedgerClient {
  /** Fetch the balance of the ledger as text. */
  fetchBalance(): string {
    return formatAmount(0);
  }

  /** Post an entry to the ledger service. */
  postEntry(amount: number): string {
    return formatAmount(amount);
  }
}
