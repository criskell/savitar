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
