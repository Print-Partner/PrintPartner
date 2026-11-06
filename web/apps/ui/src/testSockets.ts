/**
 * A WebSocket stand-in for jsdom tests. Every socket the UI opens is recorded in
 * `sockets`; a test opens it, sends frames and closes it by hand. Nothing touches the
 * network.
 */
export class FakeSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSED = 3;
  readyState = FakeSocket.CONNECTING;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;

  constructor(readonly url: string) {
    sockets.push(this);
  }

  open() {
    this.readyState = FakeSocket.OPEN;
    this.onopen?.();
  }

  send() {}

  /** Delivers one frame as the server would: JSON text. */
  frame(body: unknown) {
    this.onmessage?.({ data: typeof body === "string" ? body : JSON.stringify(body) });
  }

  close() {
    if (this.readyState === FakeSocket.CLOSED) return;
    this.readyState = FakeSocket.CLOSED;
    this.onclose?.();
  }
}

export const sockets: FakeSocket[] = [];

export function resetSockets() {
  sockets.length = 0;
}
