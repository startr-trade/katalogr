import init, { greet, Counter } from "./pkg/demo_wasm.js";

export async function main(): Promise<number> {
  await init();
  greet();
  const c = new Counter();
  c.increment();
  return c.value;
}
