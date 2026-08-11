// Shared between the Node CLI and the in-page engine (must stay platform
// neutral: no DOM or Node globals). The JSONL event contract and exit codes
// mirror the native reference client exactly (clients/native/README.md).

/** Exit codes (identical to the native client's `client.rs` constants). */
export const EXIT_SUCCESS = 0;
/** `--run-for-secs` elapsed with unmet success criteria. */
export const EXIT_CRITERIA_UNMET = 1;
/** The server broke the expected protocol flow (also the CLI-usage exit code). */
export const EXIT_PROTOCOL_ERROR = 2;
/** Transport-level failure (connect failed, socket died mid-session). */
export const EXIT_CONNECTION_ERROR = 3;
/** `--max-runtime-secs` watchdog fired (set by the CLI, never by the page). */
export const EXIT_HARD_TIMEOUT = 4;

/** Wire tokens for session topologies (CLI tokens are identical). */
export const TOPOLOGIES = ['relay', 'host', 'mesh'] as const;
/** Wire tokens for data-path transports (CLI tokens are identical). */
export const TRANSPORTS = ['relay', 'direct', 'webrtc'] as const;

export type Topology = (typeof TOPOLOGIES)[number];
export type Transport = (typeof TRANSPORTS)[number];

/**
 * The full run configuration handed from the CLI to the page engine via
 * `page.evaluate` (JSON-serializable by construction). Field semantics match
 * the native client flags one-to-one; `--max-runtime-secs` and
 * `--mdns-obfuscation` are CLI-side concerns and deliberately absent.
 */
export interface RunConfig {
  serverUrl: string;
  createRoom: boolean;
  joinCode: string | null;
  peers: number;
  expectTotalPeers: number | null;
  leaveOnGameStart: boolean;
  gameName: string;
  playerName: string;
  appId: string;
  platform: string;
  exchange: boolean;
  relayPayload: string | null;
  crippleIce: boolean;
  p2pTimeoutSecs: number;
  runForSecs: number;
  /** Hold a successful run open until the CLI's release-file bridge returns true. */
  successReleaseEnabled: boolean;
  protocolVersion: number;
  supportedTopologies: Topology[];
  supportedTransports: Transport[];
  sdkVersion: string;
  /**
   * Milliseconds the CLI spent before the page engine started (Chromium
   * launch + page setup), so the `--run-for-secs` soft window measures from
   * PROCESS start exactly like the native client's.
   */
  elapsedBeforeStartMs: number;
}

/** Distinct session members (self included) required for a successful exit. */
export function effectiveTotalPeers(config: RunConfig): number {
  return config.expectTotalPeers ?? config.peers;
}

/** Whether this run advertises protocol v3 (v2 omits all v3 fields). */
export function isV3(config: RunConfig): boolean {
  return config.protocolVersion >= 3;
}
