export type Budget = () => number;

export type Measure = (line: string) => number;

export type Header = string | undefined;

export function remaining(limit: number, used: number): number {
  const left: Budget = () => limit - used;
  return left();
}

export function runBudget(budget: Budget): number {
  return budget();
}

export function spent(limit: number): number {
  return runBudget(() => limit);
}

export function budgetOf(limit: number): Budget {
  return () => limit;
}

export function headerSize(header: Header, fallback: number): number {
  if (header === undefined) {
    return fallback;
  }
  const size: Budget = () => header.length;
  return size();
}

export function widthOf(text: string): number {
  const measure: Measure = (line) => line.length;
  return measure(text);
}

export function doubled(base: number): number {
  const twice: Budget = () => {
    const sum = base + base;
    return sum;
  };
  return twice();
}

export function tripled(base: number): number {
  const thrice: Budget = function () {
    return base * 3;
  };
  return thrice();
}

export function totalOf(sizes: readonly number[]): number {
  let total = 0;
  for (const size of sizes) {
    const weigh: Budget = () => size * 2;
    total = total + weigh();
  }
  return total;
}

export function offsetOf(base: number): number {
  const shifted = (step: number): number => base + step;
  return shifted(2);
}

export function counter(): Budget {
  let seen = 0;
  const next: Budget = () => {
    seen = seen + 1;
    return seen;
  };
  return next;
}

export class Window {
  private readonly size: number;

  constructor(size: number) {
    this.size = size;
  }

  half(): number {
    const halved: Budget = () => this.size / 2;
    return halved();
  }
}
