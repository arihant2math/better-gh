/**
 * The network boundary. Everything that talks to the server (REST client,
 * sync client) goes through the current `Transport`, so the in-browser mock
 * backend (src/mock) and tests can replace it wholesale.
 */

export interface SocketLike {
  readonly readyState: number;
  send(data: string): void;
  close(code?: number, reason?: string): void;
  onopen: ((ev: unknown) => void) | null;
  onmessage: ((ev: { data: unknown }) => void) | null;
  onclose: ((ev: { code: number; reason?: string }) => void) | null;
  onerror: ((ev: unknown) => void) | null;
}

export interface Transport {
  fetch(path: string, init?: RequestInit): Promise<Response>;
  socket(path: string): SocketLike;
}

function wsUrl(path: string): string {
  const { protocol, host } = window.location;
  return `${protocol === 'https:' ? 'wss:' : 'ws:'}//${host}${path}`;
}

export const browserTransport: Transport = {
  fetch: (path, init) => fetch(path, { credentials: 'same-origin', ...init }),
  socket: (path) => new WebSocket(wsUrl(path)) as unknown as SocketLike,
};

let current: Transport = browserTransport;

export function setTransport(t: Transport): void {
  current = t;
}

export function transport(): Transport {
  return current;
}
