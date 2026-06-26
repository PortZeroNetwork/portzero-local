/// <reference types="node" />

export interface TunnelListenOptions {
  /** Network interface to bind on. Default: "0.0.0.0" */
  host?: string;
  /** Label shown in log output. Default: "app" */
  serviceName?: string;
}

/**
 * Listen on port 0 (OS-assigned ephemeral port) and log the PORT_ZERO value.
 *
 * Works with any object that exposes `listen(port, host, callback)`:
 * Node.js `http.Server`, Express app, Fastify instance, etc.
 *
 * PORT_ZERO must be set BEFORE starting the process (via direnv, shell
 * export, or docker -e). This function cannot set it for daemon discovery —
 * the daemon reads /proc/<pid>/environ which is frozen at execve() time.
 *
 * @returns The actual port number assigned by the OS.
 */
export function listenWithTunnel(
  server: {
    listen(
      port: number,
      host: string,
      cb: (err?: Error) => void
    ): void;
    address(): { port: number } | string | null;
  },
  options?: TunnelListenOptions
): Promise<number>;

export interface ReservedPort {
  port: number;
  close(): Promise<void>;
}

/**
 * Reserve an ephemeral port by binding a bare TCP server to port 0.
 * Useful when you need a port number before creating the HTTP server.
 *
 * PORT_ZERO must be set BEFORE starting the process.
 */
export function reservePort(options?: TunnelListenOptions): Promise<ReservedPort>;

export interface PortMapping {
  /** Local port this process is listening on. */
  localPort: number;
  /** Target domain to route traffic to (e.g. "api.alice.tunnel.portzero.cloud"). */
  domain: string;
}

/**
 * Declare this process's port-to-domain mappings with the portzero daemon.
 *
 * Call this instead of setting PZ_TUNNEL_PORTS when your process listens on
 * multiple ports. The daemon identifies this process by TCP source port — no
 * authentication is required. Each call replaces any prior registration.
 *
 * @rejects if the daemon is unreachable or rejects a port claim.
 */
export function registerPorts(mappings: PortMapping[]): Promise<void>;
