export type HeaderValue = string | number;

export function headerText(value: HeaderValue): string {
  if (typeof value === "number") {
    return `${value}`;
  }
  return value;
}

export function retryAfter(value: HeaderValue, fallback: number): number {
  if (typeof value === "number") {
    return value + fallback;
  }
  return fallback;
}

export function headerWeight(value: HeaderValue): number {
  return typeof value === "string" ? value.length : value + 1;
}
