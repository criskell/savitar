export interface LogLine {
  level: string;
  message: string;
  millis: number;
}

export interface Window {
  count: number;
  millis: number;
}

export function firstError(lines: readonly LogLine[]): string {
  let found = "";
  for (const line of lines) {
    if (line.level !== "error") {
      continue;
    }
    found = line.message;
    break;
  }
  return found;
}

export function withinBudget(lines: readonly LogLine[], limit: number): Window {
  let count = 0;
  let millis = 0;
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    if (line === undefined) {
      continue;
    }
    if (millis + line.millis > limit) {
      break;
    }
    count += 1;
    millis += line.millis;
  }
  return { count: count, millis: millis };
}

export function drain(queue: readonly string[], budget: number): number {
  let handled = 0;
  let index = 0;
  while (index < queue.length) {
    const next = queue[index];
    index += 1;
    if (next === undefined) {
      continue;
    }
    if (handled >= budget) {
      break;
    }
    handled += 1;
  }
  return handled;
}

export function backoff(attempts: number, ceiling: number): number {
  let waited = 1;
  let tried = 0;
  do {
    tried += 1;
    waited = waited * 2;
    if (waited > ceiling) {
      waited = ceiling;
      break;
    }
  } while (tried < attempts);
  return waited;
}

export interface Route {
  method: string;
  segments: readonly string[];
}

export function matchRoute(
  routes: readonly Route[],
  method: string,
  path: readonly string[]
): number {
  let matched = -1;
  let index = 0;
  candidates: for (const route of routes) {
    if (route.method !== method) {
      index += 1;
      continue candidates;
    }
    if (route.segments.length !== path.length) {
      index += 1;
      continue candidates;
    }
    for (let at = 0; at < route.segments.length; at += 1) {
      const wanted = route.segments[at];
      const given = path[at];
      if (wanted === undefined || given === undefined) {
        index += 1;
        continue candidates;
      }
      if (wanted !== given && wanted !== "*") {
        index += 1;
        continue candidates;
      }
    }
    matched = index;
    break candidates;
  }
  return matched;
}

export function statusFor(line: LogLine, fallback: number): number {
  let status = fallback;
  decide: {
    if (line.level === "error") {
      status = 500;
      break decide;
    }
    if (line.level === "warn") {
      status = 429;
      break decide;
    }
    status = 200;
  }
  return status;
}

export function retryDelay(attempts: readonly number[], ceiling: number): number {
  let delay = 1;
  for (const attempt of attempts) {
    if (attempt < 0) {
      const delay = ceiling * 2;
      if (delay > ceiling) {
        break;
      }
    }
    delay = delay * 2;
    if (delay >= ceiling) {
      delay = ceiling;
      break;
    }
  }
  return delay;
}

export function drainUntil(queue: readonly string[], stop: string): number {
  let index = 0;
  for (;;) {
    const next = queue[index];
    if (next === undefined) {
      break;
    }
    index += 1;
    if (next === stop) {
      break;
    }
  }
  return index;
}

export function nextSlot(used: readonly boolean[], from: number): number {
  let at = from;
  for (;;) {
    if (at >= used.length) {
      return -1;
    }
    if (used[at] === false) {
      return at;
    }
    at += 1;
  }
}
