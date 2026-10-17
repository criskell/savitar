import { limit } from "./shared.ts";

export function under(count: number): boolean {
  return count < limit;
}
