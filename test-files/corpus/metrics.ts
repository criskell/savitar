export interface Sample {
  name: string;
  millis: number;
}

export function total(samples: readonly Sample[]): number {
  let sum = 0;
  for (const sample of samples) {
    sum = sum + sample.millis;
  }
  return sum;
}

export function slowest(samples: readonly Sample[]): Sample | undefined {
  let best: Sample | undefined = undefined;
  for (const sample of samples) {
    if (best === undefined || sample.millis > best.millis) {
      best = sample;
    }
  }
  return best;
}

export function byName(samples: readonly Sample[]): Record<string, number> {
  const out: Record<string, number> = {};
  for (const sample of samples) {
    out[sample.name] = sample.millis;
  }
  return out;
}
