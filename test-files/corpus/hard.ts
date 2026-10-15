export type Json =
  | string
  | number
  | boolean
  | null
  | readonly Json[]
  | { readonly [key: string]: Json };

export interface Pageable {
  limit: number;
  cursor?: string;
}

export class Counter {
  private value: number;

  constructor(start: number) {
    this.value = start;
  }

  increment(by: number): number {
    this.value = this.value + by;
    return this.value;
  }
}

export function pairOf(left: number, right: string): [number, string] {
  return [left, right];
}

export function firstOf<T>(items: readonly T[]): T | undefined {
  return items.length > 0 ? items[0] : undefined;
}

export function clampPage(page: Pageable): Pageable {
  const limit = page.limit > 100 ? 100 : page.limit;
  return { limit };
}
