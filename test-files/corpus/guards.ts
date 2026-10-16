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
