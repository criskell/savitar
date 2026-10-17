export interface ServerConfig {
  host: string;
  port: number;
  tls: boolean;
  label?: string;
}

export const defaultHost = "0.0.0.0";
export const defaultPort = 8080;

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
