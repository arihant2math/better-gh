/**
 * Server-Sent Events over `transport().fetch` (not `EventSource`, so the
 * in-browser mock backend and tests can serve the stream), plus the job log
 * stream client with reconnect + backoff.
 */
import { transport } from '../../../api/transport';

export interface SseEvent {
  event: string;
  data: string;
  id?: string;
}

/**
 * Incremental `text/event-stream` parser (WHATWG HTML §9.2.6). Feed decoded
 * text in arbitrary pieces; complete events are emitted as soon as their
 * terminating blank line arrives. Runs in O(input) even when one event (a
 * whole step's log) spans thousands of chunks.
 */
export class SseParser {
  /** Pieces of the current, not yet terminated line. */
  private pending: string[] = [];
  /** The previous chunk ended with `\r`: a leading `\n` belongs to it. */
  private skipLf = false;
  private event = '';
  private data: string[] = [];
  private hasData = false;
  private id: string | undefined;

  constructor(private readonly onEvent: (ev: SseEvent) => void) {}

  push(chunk: string): void {
    let start = 0;
    if (this.skipLf) {
      this.skipLf = false;
      if (chunk.charCodeAt(0) === 10) start = 1;
    }
    const n = chunk.length;
    for (let i = start; i < n; i++) {
      const c = chunk.charCodeAt(i);
      if (c !== 10 && c !== 13) continue;
      let line = chunk.slice(start, i);
      if (this.pending.length) {
        this.pending.push(line);
        line = this.pending.join('');
        this.pending = [];
      }
      if (c === 13) {
        if (i + 1 < n) {
          if (chunk.charCodeAt(i + 1) === 10) i++;
        } else {
          this.skipLf = true;
        }
      }
      start = i + 1;
      this.line(line);
    }
    if (start < n) this.pending.push(chunk.slice(start));
  }

  private line(line: string): void {
    if (line === '') {
      this.dispatch();
      return;
    }
    if (line.charCodeAt(0) === 58 /* : */) return; // comment / keep-alive
    const colon = line.indexOf(':');
    const field = colon === -1 ? line : line.slice(0, colon);
    let value = colon === -1 ? '' : line.slice(colon + 1);
    if (value.charCodeAt(0) === 32) value = value.slice(1);
    switch (field) {
      case 'event':
        this.event = value;
        break;
      case 'data':
        this.data.push(value);
        this.hasData = true;
        break;
      case 'id':
        if (!value.includes('\0')) this.id = value;
        break;
      // `retry` and unknown fields are ignored.
    }
  }

  private dispatch(): void {
    // Per spec an event without data is dropped; we also deliver named
    // events without data (a bare `event: done` still means done).
    if (this.hasData || this.event) {
      this.onEvent({ event: this.event || 'message', data: this.data.join('\n'), id: this.id });
    }
    this.event = '';
    this.data = [];
    this.hasData = false;
  }
}

export interface LogStreamHandlers {
  /** A (re)connection succeeded: the server resends everything, so drop what you have. */
  onReset(): void;
  onLog(step: number, text: string): void;
  onDone(): void;
  /** The stream can't be read (permissions, gone). No retry follows. */
  onError?(status: number): void;
}

const MAX_BACKOFF = 15_000;

/** Delay before reconnect attempt `n` (1-based): 0.5s, 1s, 2s, … ≤ 15s, ±20% jitter. */
export function backoff(n: number): number {
  const base = Math.min(MAX_BACKOFF, 500 * 2 ** Math.max(0, n - 1));
  return Math.round(base * (0.8 + Math.random() * 0.4));
}

function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    const t = setTimeout(done, ms);
    function done() {
      clearTimeout(t);
      signal.removeEventListener('abort', done);
      resolve();
    }
    signal.addEventListener('abort', done, { once: true });
  });
}

/**
 * Follow a job log SSE stream until `done` or `signal` aborts. Reconnects
 * with backoff when the stream drops before `done`.
 */
export async function streamJobLog(path: string, handlers: LogStreamHandlers, signal: AbortSignal): Promise<void> {
  let attempt = 0;
  while (!signal.aborted) {
    let finished = false;
    try {
      const res = await transport().fetch(path, { headers: { Accept: 'text/event-stream' }, signal });
      if (res.status >= 400 && res.status < 500 && res.status !== 408 && res.status !== 429) {
        handlers.onError?.(res.status);
        return;
      }
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      handlers.onReset();
      const parser = new SseParser((ev) => {
        if (finished) return;
        if (ev.event === 'log') {
          let msg: { step?: unknown; text?: unknown };
          try {
            msg = JSON.parse(ev.data) as typeof msg;
          } catch {
            return;
          }
          if (typeof msg.step === 'number' && typeof msg.text === 'string') {
            attempt = 0;
            handlers.onLog(msg.step, msg.text);
          }
        } else if (ev.event === 'done') {
          finished = true;
          handlers.onDone();
        }
      });
      if (!res.body) {
        parser.push(await res.text());
      } else {
        const reader = res.body.pipeThrough(new TextDecoderStream()).getReader();
        try {
          while (!finished) {
            const { value, done } = await reader.read();
            if (done) break;
            parser.push(value);
          }
        } finally {
          void reader.cancel().catch(() => undefined);
        }
      }
      if (finished) return;
    } catch {
      if (signal.aborted) return;
    }
    if (finished) return;
    attempt++;
    await sleep(backoff(attempt), signal);
  }
}
