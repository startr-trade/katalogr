import * as wasm from "./demo_wasm_bg.js";

export function greet() {
  return wasm.greet();
}

export function sayHi() {
  return wasm.sayHi();
}

export function tick() {
  return wasm.tick();
}

export function unused_export() {
  wasm.unused_export();
}

export const Color = Object.freeze({ Red: 0, 0: "Red", Green: 1, 1: "Green" });

export class Counter {
  free() {
    wasm.__wbg_counter_free(this.ptr);
  }

  constructor() {
    this.ptr = wasm.counter_new();
  }

  increment() {
    wasm.counter_increment(this.ptr);
  }

  get value() {
    return wasm.counter_value(this.ptr);
  }

  set value(v) {
    wasm.counter_set_value(this.ptr, v);
  }
}

export function initSync(module) {
  return module;
}

export default function __wbg_init(input) {
  return Promise.resolve(input);
}
