import init, { price_quote } from "pricing-wasm";

/** Show the total of the shopping cart using the quote service. */
export async function cartTotal(quantity: number): Promise<number> {
  await init();
  return price_quote(quantity);
}

/** Render the quote for a quantity as text. */
export async function renderQuote(quantity: number): Promise<string> {
  const total = await cartTotal(quantity);
  return `Total: ${total}`;
}
