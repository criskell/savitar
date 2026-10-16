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
