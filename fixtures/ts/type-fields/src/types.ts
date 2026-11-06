import { Line } from "./model";
import * as m from "./model";
import { Moment } from "moment";

/** An order. */
export interface Order {
  /** The identifier. */
  id: string;
  note?: string;
  lines: Line[];
  first: m.Line;
  when: Moment;
  local: Tag;
  blob: Uint8Array;
  kind: "a" | "b";
  maybe: number | null;
  tags: Record<string, string>;
  count: number;
  readonly stamp: Date;
  describe(): string;
  [key: string]: unknown;
}

interface Tag {
  label: string;
}

export class Cart {
  items: Order[] = [];
  #secret: string = "";
  static total = 0;
  note?: string;
  constructor(private owner: string) {}
  sum(): number {
    return 0;
  }
}

export type LineBox = { sku: string; qty: number };

export type Both = Tag & { extra: boolean };

export type Choice = Tag | LineBox;
