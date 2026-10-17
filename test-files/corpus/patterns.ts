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

export interface Window {
  from: number;
  to: number;
}

export function slide(window: Window, step: [number, number]): number {
  [window.from, window.to] = step;
  return window.to - window.from;
}

export function recentre(window: Window, moved: Window): number {
  ({ from: window.from, to: window.to } = moved);
  return window.to - window.from;
}

export function shiftFirst(slots: number[], step: [number]): number {
  [slots[0]] = step;
  return slots.length;
}

export function swapped(pair: [number, number]): [number, number] {
  let left = 0;
  let right = 0;
  [left, right] = pair;
  return [right, left];
}

export function routeOf({ method, path }: RequestLine): string {
  return method + " " + path;
}

export function totalBytes(lines: readonly RequestLine[]): number {
  let sum = 0;
  for (const { bytes } of lines) {
    sum = sum + bytes;
  }
  return sum;
}

export function totalTimeout(routes: readonly Route[]): number {
  let sum = 0;
  for (const { timeout = 30 } of routes) {
    sum = sum + timeout;
  }
  return sum;
}

export function labelsOf(samples: readonly Sample[]): string {
  let joined = "";
  for (const { label, millis } of samples) {
    if (millis > 100) {
      joined = joined + label;
    }
  }
  return joined;
}

export type Signal =
  | { kind: "started"; at: number }
  | { kind: "stopped"; at: number };

export function signalKind(signal: Signal): string {
  const { kind } = signal;
  return kind;
}

export function signalLabel(signal: Signal): string {
  const { kind, at } = signal;
  if (kind === "started") {
    return `up ${at}`;
  }
  return `down ${at}`;
}

export function hostOf(env: Record<string, string>): string {
  const { HOST } = env;
  if (HOST === undefined) {
    return "localhost";
  }
  return HOST;
}

export function retriesOf(settings: Record<string, number>): number {
  const { retries = 3 } = settings;
  return retries;
}

export type Incident =
  | { kind: "timeout"; route: string; afterMillis: number }
  | { kind: "crash"; route: string; message: string };

export type IncidentBody =
  | { route: string; afterMillis: number }
  | { route: string; message: string };

export function bodyOf(incident: Incident): IncidentBody {
  const { kind, ...body } = incident;
  return body;
}

export function routeTimeout(
  { timeout = 30 }: Route = { handler: "none" },
): number {
  return timeout;
}
