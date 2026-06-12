// WebSocket wire layer: JSON text frames carrying the server's
// `{type, data}` message envelope (docs/protocol.md; canonical samples in
// .llm/code-samples/protocol/). Unlike the native client, which consumes the
// server crate's serde types via a path dependency, the browser client
// hand-models only the envelope fields it actually reads — drift is caught by
// the interop suite, which drives this client against the real server binary.

/** One parsed server frame: the envelope tag plus its (optional) payload. */
export interface ServerFrame {
  type: string;
  data: Record<string, unknown>;
}

/** Time allowed for the WebSocket connection to open (mirrors wire.rs). */
export const CONNECT_TIMEOUT_MS = 10_000;
/** Per-message ceiling during the sequential handshake phase (mirrors wire.rs). */
export const HANDSHAKE_TIMEOUT_MS = 20_000;

/** Build one client→server frame in the `{type, data}` envelope. */
export function clientFrame(type: string, data?: Record<string, unknown>): string {
  return JSON.stringify(data === undefined ? { type } : { type, data });
}

/** Parse one server frame; throws on a non-envelope payload. */
export function parseServerFrame(text: string): ServerFrame {
  const value: unknown = JSON.parse(text);
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    throw new Error(`server frame is not an object: ${text}`);
  }
  const envelope = value as Record<string, unknown>;
  const type = envelope['type'];
  if (typeof type !== 'string') {
    throw new Error(`server frame has no string \`type\`: ${text}`);
  }
  const data = envelope['data'];
  if (
    data !== undefined &&
    (typeof data !== 'object' || data === null || Array.isArray(data))
  ) {
    throw new Error(`server frame \`data\` is not an object: ${text}`);
  }
  return { type, data: (data as Record<string, unknown> | undefined) ?? {} };
}

/**
 * Open the WebSocket and resolve once the connection is established.
 * Rejection means a transport-level failure (exit code 3 territory).
 */
export function connect(serverUrl: string): Promise<WebSocket> {
  return new Promise((resolve, reject) => {
    let settled = false;
    const ws = new WebSocket(serverUrl);
    const timer = setTimeout(() => {
      if (!settled) {
        settled = true;
        ws.close();
        reject(new Error(`websocket connect to ${serverUrl} timed out`));
      }
    }, CONNECT_TIMEOUT_MS);
    ws.onopen = () => {
      if (!settled) {
        settled = true;
        clearTimeout(timer);
        resolve(ws);
      }
    };
    // The browser fires `error` then `close` on a failed connect; `error`
    // events carry no detail, so the close handler reports the code.
    ws.onclose = (event) => {
      if (!settled) {
        settled = true;
        clearTimeout(timer);
        reject(
          new Error(`websocket connect to ${serverUrl} failed (close code ${event.code})`),
        );
      }
    };
  });
}
