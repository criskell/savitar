export interface RequestLine {
  method: string;
  path: string;
  bytes: number;
}

export interface Route {
  handler: string;
  timeout?: number;
}

export function methodAndPath(line: RequestLine): string {
  const { method, path } = line;
  return method + " " + path;
}

export function sizeOf(line: RequestLine): number {
  const { bytes: size } = line;
  return size;
}

export function timeoutOf(route: Route): number {
  const { timeout } = route;
  if (timeout === undefined) {
    return 30;
  }
  return timeout;
}

export interface Paging {
  limit?: number;
  offset?: number;
}

export interface Page {
  limit: number;
  offset: number;
}

export interface Retry {
  attempts?: number | null;
}

export function pageOf(paging: Paging): Page {
  const { limit = 50, offset = 0 } = paging;
  return { limit: limit, offset: offset };
}

export function firstHeader(values: readonly string[]): string {
  const [first = "none"] = values;
  return first;
}

export function attemptsOf(retry: Retry): number | null {
  const { attempts = 3 } = retry;
  return attempts;
}

export interface Header {
  name: string;
  value: string;
}

export function valueOfSecond(pair: [Header, Header]): string {
  const [, second] = pair;
  return second.value;
}

export interface Incoming {
  path: string;
  token: string;
  bytes: number;
}

export interface Loggable {
  path: string;
  bytes: number;
}

export function withoutToken(incoming: Incoming): Loggable {
  const { token, ...safe } = incoming;
  return safe;
}

export interface Envelope {
  line: RequestLine;
  route: Route;
}

export function targetOf(envelope: Envelope): string {
  const {
    line: { path },
    route: { handler },
  } = envelope;
  return path + " -> " + handler;
}

export function budgetOf(envelope: Envelope): number {
  const {
    route: { timeout = 30 },
  } = envelope;
  return timeout;
}

export function firstHeaderName(pair: [Header, Header]): string {
  const [{ name }] = pair;
  return name;
}

export function firstRouteHandler(routes: Route[], fallback: Route): string {
  const [{ handler } = fallback] = routes;
  return handler;
}

export interface Sample {
  label: string;
  millis: number;
}

export function slowest(samples: readonly Sample[]): Sample {
  let label = "";
  let millis = 0;
  for (const sample of samples) {
    if (sample.millis > millis) {
      ({ label, millis } = sample);
    }
  }
  return { label: label, millis: millis };
}

export function swapped(pair: [number, number]): [number, number] {
  let left = 0;
  let right = 0;
  [left, right] = pair;
  return [right, left];
}
