export type Method = "GET" | "POST" | "DELETE";

export type Outcome =
  | { kind: "ok"; body: string }
  | { kind: "error"; status: number; detail: string };

export interface Route {
  method: Method;
  path: string;
}

export abstract class Middleware {
  abstract name(): string;

  describe(): string {
    return `middleware ${this.name()}`;
  }
}

export class Logger extends Middleware {
  private readonly prefix: string;

  constructor(prefix: string) {
    super();
    this.prefix = prefix;
  }

  override name(): string {
    return this.prefix;
  }
}

export function describe(outcome: Outcome): string {
  switch (outcome.kind) {
    case "ok":
      return outcome.body;
    case "error":
      return `${outcome.status}: ${outcome.detail}`;
  }
}

export async function fetchBody(route: Route): Promise<Outcome> {
  if (route.method === "GET") {
    return { kind: "ok", body: route.path };
  }
  return { kind: "error", status: 405, detail: route.method };
}

export async function collect(routes: readonly Route[]): Promise<string[]> {
  const out: string[] = [];
  for (const route of routes) {
    const outcome = await fetchBody(route);
    out.push(describe(outcome));
  }
  return out;
}

export function split(route: Route): { head: string; rest: readonly string[] } {
  const parts = route.path.split("/");
  const [head, ...rest] = parts;
  return { head: head ?? "", rest };
}

export function tally(values: readonly number[]): Map<number, number> {
  const seen = new Map<number, number>();
  for (const value of values) {
    const current = seen.get(value);
    seen.set(value, current === undefined ? 1 : current + 1);
  }
  return seen;
}
