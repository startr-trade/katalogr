/** Format an amount in cents as a decimal string. */
export function formatAmount(cents: number): string {
  return (cents / 100).toFixed(2);
}

/** Format a balance in cents. */
export function balance(cents: number): string {
  return formatAmount(cents);
}
