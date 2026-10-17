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

export abstract class Transport {
  protected readonly timeoutMillis: number;

  constructor(timeoutMillis: number) {
    this.timeoutMillis = timeoutMillis;
  }

  budget(): number {
    return this.timeoutMillis;
  }
}

export class HttpTransport extends Transport {
  private readonly origin: string;

  constructor(origin: string, timeoutMillis: number) {
    super(timeoutMillis);
    this.origin = origin;
  }

  target(path: string): string {
    return `${this.origin}${path}`;
  }
}

export class RetryingTransport extends Transport {
  private attempts: number;

  constructor(timeoutMillis: number, attempts: number) {
    super(timeoutMillis);
    this.attempts = attempts;
  }

  nextDelay(): number {
    this.attempts = this.attempts + 1;
    return this.budget() * this.attempts;
  }
}

export abstract class Encoder {
  readonly contentType: string;

  constructor(contentType: string) {
    this.contentType = contentType;
  }

  header(): string {
    return `content-type: ${this.contentType}`;
  }
}

export class JsonEncoder extends Encoder {
  declare readonly contentType: "application/json";

  constructor() {
    super("application/json");
  }

  framed(body: string): string {
    return `${this.header()}\n\n${body}`;
  }
}

export class RouteTable {
  private readonly mount: string;
  private hits: number;

  constructor(mount: string) {
    this.mount = mount;
    this.hits = 0;
  }

  record(route: Route): string {
    this.hits = this.hits + 1;
    return `${this.mount}${route.path}`;
  }

  counted(): number {
    return this.hits;
  }

  static rooted(): RouteTable {
    return new RouteTable("/");
  }
}

export abstract class Audit {
  static readonly channel: string;

  static stamped(at: number): number {
    return at;
  }
}

export class RouteAudit extends Audit {
  static declare readonly channel: "route";

  static override stamped(at: number): number {
    return at + 1;
  }

  static tableFor(mount: string): RouteTable {
    return new RouteTable(mount);
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

export function loggerFor(route: Route): Logger {
  return new Logger(route.path);
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
