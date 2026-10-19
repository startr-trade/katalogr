export function greet(): string;
export function sayHi(): string;
export function tick(): number;
export function unused_export(): void;

export enum Color {
  Red = 0,
  Green = 1,
}

export class Counter {
  free(): void;
  constructor();
  increment(): void;
  value: number;
}

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
  readonly memory: WebAssembly.Memory;
}

export function initSync(module: BufferSource | WebAssembly.Module): InitOutput;

export default function __wbg_init(input?: InitInput): Promise<InitOutput>;
