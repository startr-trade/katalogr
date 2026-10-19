import init, { greet, sayHi, Counter, Color } from "demo-wasm";

export async function main(): Promise<Color> {
  await init();
  greet();
  sayHi();
  const c = new Counter();
  c.increment();
  return Color.Red;
}
