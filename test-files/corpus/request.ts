export type Method = "GET" | "POST";

export interface Request {
  method: Method;
  path: string;
  body: string;
}

export interface Response {
  status: number;
  body: string;
}

export function notFound(path: string): Response {
  return { status: 404, body: path };
}

export function isWrite(request: Request): boolean {
  return request.method === "POST";
}

export function paths(requests: readonly Request[]): string[] {
  const out: string[] = [];
  for (const request of requests) {
    out.push(request.path);
  }
  return out;
}

export function windowSize(total: number, limit: number = 50): number {
  if (total < limit) {
    return total;
  }
  return limit;
}

export function backoff(attempt: number, base: number = 100): number {
  return base * attempt;
}
