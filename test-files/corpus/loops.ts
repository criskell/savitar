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
