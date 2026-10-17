export interface Shape {
  area(): number;
}

export type Id = string | number;

export enum Color {
  Red,
  Green,
}

export function plain(a: number): number {
  return a;
}

export async function later(): Promise<void> {}

function hidden() {}

function overload(a: string): string;
function overload(a: number): number;
function overload(a: any): any {
  return a;
}

@sealed
export class Widget {
  static make(): Widget {
    return new Widget(1);
  }

  constructor(private x: number) {}

  get size(): number {
    return this.x;
  }

  @bound
  render(): void {
    hidden();
  }
}

export const arrow = async (a: number) => a + 1;
export const expr = function (b: number) {
  return b;
};
export const value = 42;
export default function () {}
