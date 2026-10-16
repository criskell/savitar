export type JsonBody = { path: string; size: number };

export type Payload = string | JsonBody | null;

export type Resolver = () => number;

export function bodySize(body: JsonBody | null): number {
  if (body === null) {
    return 0;
  }
  return body.size;
}

export function textLength(text: string): number {
  return text.length;
}

export function payloadSize(payload: Payload): number {
  if (typeof payload === "object") {
    return bodySize(payload);
  }
  return textLength(payload);
}

export function resolved(resolver: Resolver): number {
  return resolver();
}

export function portOf(setting: number | Resolver): number {
  if (typeof setting === "function") {
    return resolved(setting);
  }
  return setting;
}

export type GetRequest = { method: "get"; query: string };

export type PostRequest = { method: "post"; body: JsonBody };

export type DeleteRequest = { method: "delete"; target: number };

export type Request = GetRequest | PostRequest | DeleteRequest;

export function getCost(request: GetRequest): number {
  return request.query.length;
}

export function postCost(request: PostRequest): number {
  return request.body.size;
}

export function deleteCost(request: DeleteRequest): number {
  return request.target;
}

export function requestCost(request: Request): number {
  switch (request.method) {
    case "get":
      return getCost(request);
    case "post":
      return postCost(request);
    default:
      return deleteCost(request);
  }
}

export type Verb = "get" | "head" | "post";

export function cachedCost(verb: "get" | "head", base: number): number {
  return verb === "head" ? base : base + 1;
}

export function writtenCost(verb: "post", base: number): number {
  return base + 2;
}

export function verbCost(verb: Verb, base: number): number {
  switch (verb) {
    case "get":
    case "head":
      return cachedCost(verb, base);
    default:
      return writtenCost(verb, base);
  }
}

export type Mode = "" | "fast" | "safe";

export function hostOf(host: string | undefined, fallback: string): string {
  if (host) {
    return host;
  }
  return fallback;
}

export function pathLength(path: string | undefined): number {
  if (!path) {
    return 0;
  }
  return textLength(path);
}

export function retries(limit: number | undefined, fallback: number): number {
  return limit ? limit : fallback;
}

export function modeOf(mode: Mode, fallback: "fast" | "safe"): "fast" | "safe" {
  if (mode) {
    return mode;
  }
  return fallback;
}

export type Ok = { status: number; body: JsonBody };

export type Failure = { status: number; reason: string };

export type Reply = Ok | Failure;

export function okSize(reply: Ok): number {
  return reply.body.size;
}

export function failureSize(reply: Failure): number {
  return reply.reason.length;
}

export function failureText(reply: Failure): string {
  return reply.reason;
}

export function replySize(reply: Reply): number {
  if ("body" in reply) {
    return okSize(reply);
  }
  return failureSize(reply);
}

export function replyReason(reply: Reply, fallback: string): string {
  if (!("reason" in reply)) {
    return fallback;
  }
  return failureText(reply);
}

export function replyStatus(reply: Reply): number {
  return reply.status;
}

export type TextLog = { level: "info" | "warn"; line: string };

export type ErrorLog = { level: "error"; line: string; code: number };

export type LogEntry = TextLog | ErrorLog;

export function logLevel(entry: LogEntry): string {
  return entry.level;
}

export function logWidth(entry: LogEntry): number {
  return textLength(entry.line);
}
