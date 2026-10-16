export type HeaderValue = string | number;

export type TextFrame = { kind: "text"; text: string };

export type BinaryFrame = { kind: "binary"; size: number };

export type Frame = TextFrame | BinaryFrame;

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

export function textSize(frame: TextFrame): number {
  return frame.text.length;
}

export function binarySize(frame: BinaryFrame): number {
  return frame.size;
}

export function frameBytes(frame: Frame): number {
  if (frame.kind === "binary") {
    return binarySize(frame);
  }
  return textSize(frame);
}

export function frameLabel(frame: Frame): string {
  if (frame.kind !== "text") {
    return `binary ${binarySize(frame)}`;
  }
  return frame.text;
}

export function frameWeight(frame: Frame, extra: number): number {
  return frame.kind === "text" ? textSize(frame) + extra : binarySize(frame);
}

export function binaryCount(frames: readonly Frame[], limit: number): number {
  let seen = 0;
  for (const frame of frames) {
    if (frame.kind === "binary" && binarySize(frame) < limit) {
      seen = seen + 1;
    }
  }
  return seen;
}

export function textBudget(frames: readonly Frame[], budget: number): number {
  let left = budget;
  for (const frame of frames) {
    if (frame.kind === "binary" || textSize(frame) > left) {
      continue;
    }
    left = left - 1;
  }
  return left;
}
