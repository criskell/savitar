export function counted(limit: number): Generator<number> {
  return makeCounter(limit);
}

function* makeCounter(limit: number): Generator<number> {
  let seen = 0;
  while (seen < limit) {
    yield seen;
    seen = seen + 1;
  }
}
