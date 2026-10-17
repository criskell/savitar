export interface CommandRegistry {
  ping: { kind: "ping"; sentAtMillis: number };
  publish: { kind: "publish"; topic: string; body: string };
}

export interface CommandRegistry {
  drain: { kind: "drain"; graceMillis: number };
}

export type Command = CommandRegistry[keyof CommandRegistry];

export function route(command: Command): string {
  switch (command.kind) {
    case "ping":
      return `pong ${command.sentAtMillis}`;
    case "publish":
      return `${command.topic}/${command.body}`;
    case "drain":
      return `drain ${command.graceMillis}`;
  }
}

export function budgetMillis(command: Command): number {
  switch (command.kind) {
    case "ping":
      return 50;
    case "publish":
      return 500;
    case "drain":
      return command.graceMillis;
  }
}

export function isUrgent(command: Command): boolean {
  switch (command.kind) {
    case "drain":
      return true;
    default:
      return false;
  }
}

export function describeAll(commands: readonly Command[]): string[] {
  const lines: string[] = [];
  for (const command of commands) {
    lines.push(route(command));
  }
  return lines;
}
