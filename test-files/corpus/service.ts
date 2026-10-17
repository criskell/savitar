import { type ServerConfig, address, defaultPort } from "./config.ts";
import type { Sample } from "./metrics.ts";

export interface Health {
  reachable: boolean;
  endpoint: string;
}

export function probe(config: ServerConfig): Health {
  return { reachable: true, endpoint: address(config) };
}

export function fallbackPort(): number {
  return defaultPort;
}

export function label(sample: Sample): string {
  return sample.name;
}
