export interface Budget {
  spent: number;
  limit: number;
}

export interface Attempt {
  tries: number;
  waited: number;
}

export interface Session {
  token: string;
  user: string;
}

export type Outcome = "ok" | "slow" | "over";

export function spin(flag: boolean, limit: number): boolean {
  let done = flag;
  let seen = 0;
  while (seen < limit) {
    done = false;
    seen = seen + 1;
  }
  return done;
}

export function classify(budget: Budget, taken: readonly number[]): Outcome {
  let outcome: Outcome = "ok";
  for (const millis of taken) {
    budget.spent = budget.spent + millis;
    if (budget.spent > budget.limit) {
      outcome = "over";
      break;
    }
    if (millis > 100) {
      outcome = "slow";
    }
  }
  return outcome;
}

export function retry(ceiling: number, attempts: number): Attempt {
  let waited: number = 1;
  let tries: number = 0;
  while (tries < attempts) {
    tries = tries + 1;
    waited = waited * 2;
    if (waited >= ceiling) {
      waited = ceiling;
      break;
    }
  }
  return { tries: tries, waited: waited };
}

export function tokenOf(
  sessions: readonly Session[],
  user: string,
): string | undefined {
  let found: string | undefined = undefined;
  for (const session of sessions) {
    if (session.user !== user) {
      continue;
    }
    found = session.token;
    break;
  }
  return found;
}
