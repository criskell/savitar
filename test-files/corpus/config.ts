export interface ServerConfig {
  host: string;
  port: number;
  tls: boolean;
  label?: string;
}

export interface Limits {
  readonly maxBodyBytes: number;
  readonly timeoutMs: number;
}

export const defaultHost = "0.0.0.0";
export const defaultPort = 8080;

export function limitsFor(environment: string): Limits {
  const atMost = (milliseconds: number): number =>
    milliseconds > 30000 ? 30000 : milliseconds;
  if (environment === "production") {
    return { maxBodyBytes: 1048576, timeoutMs: atMost(5000) };
  }
  return { maxBodyBytes: 8388608, timeoutMs: atMost(45000) };
}

export const limits = limitsFor("development");

export function withDefaults(host: string, port: number): ServerConfig {
  return { host, port, tls: false };
}

export function defaults(): ServerConfig {
  return withDefaults(defaultHost, defaultPort);
}

export function address(config: ServerConfig): string {
  const scheme = config.tls ? "https" : "http";
  return `${scheme}://${config.host}:${config.port}`;
}
