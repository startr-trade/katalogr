import { Widget } from "./widget";

class Local {}

export function build() {
  const a = new Local();
  const w = new Widget();
  return [a, w, new Widget()];
}
