// Command-line argument parsing for the browser reference client.
//
// The flag surface mirrors the native client's (clients/native/src/cli.rs)
// one-to-one, plus the browser-specific `--mdns-obfuscation`. Usage errors
// exit with code 2, matching clap's CLI-usage exit code (documented in the
// native README); on that path no `exiting` event is emitted.

import {
  TOPOLOGIES,
  TRANSPORTS,
  type RunConfig,
  type Topology,
  type Transport,
} from '../shared/types.js';

/** A CLI-usage failure (maps to exit code 2). */
export class UsageError extends Error {}

/** Everything `main` needs: the page config plus the CLI-side knobs. */
export interface CliOptions {
  config: RunConfig;
  /** Hard watchdog: abort with exit code 4 after this many seconds. */
  maxRuntimeSecs: number;
  /** Test-harness barrier path; null preserves the normal bounded exit. */
  successReleaseFile: string | null;
  /**
   * Leave Chromium's default mDNS candidate obfuscation ON (the `.local`
   * trap). Default OFF: Chromium is launched with
   * `--disable-features=WebRtcHideLocalIpsWithMdns` for deterministic
   * loopback host candidates.
   */
  mdnsObfuscation: boolean;
}

/** Build-time constant injected by esbuild (the package version). */
declare const SDK_VERSION: string;

export const USAGE = `signal-fish-reference-browser ${SDK_VERSION}
Browser reference client for the Signal Fish protocol v3 (real Chromium
RTCPeerConnection via playwright-core). stdout is a machine interface: one
JSON event object per line. All logging goes to stderr.

Usage: node cli.js --server-url <URL> (--create-room | --join-code <CODE>) [options]

Options:
  --server-url <URL>            Full WebSocket URL of the signaling endpoint
                                (e.g. ws://127.0.0.1:3536/v3/ws) [required]
  --create-room                 Create a new room (exclusive with --join-code)
  --join-code <CODE>            Join an existing room by code
  --peers <N>                   Expected member count incl. self [default: 2]
  --expect-total-peers <N>      Distinct members observed before a successful
                                exit [default: --peers]
  --leave-on-game-start         Exit 0 after GameStarting + plan, without pairing
  --game-name <NAME>            Game name [default: reference-browser]
  --player-name <NAME>          Display name [default: RefBrowser]
  --app-id <ID>                 Authenticate app_id [default: reference-browser-app]
  --platform <P>                Authenticate platform [default: reference-browser]
  --exchange                    Send one message per channel per open pair
  --relay-payload <TEXT>        Send GameData {"relay_msg": TEXT} after GameStarting
  --cripple-ice                 Drop ALL outbound and inbound IceCandidate signals
  --p2p-timeout-secs <S>        WebRTC establishment window [default: 15]
  --run-for-secs <S>            Soft cap: exit 1 if criteria unmet [default: 30]
  --max-runtime-secs <S>        Hard watchdog: abort with exit 4 [default: 60]
  --success-release-file <PATH> Test harness only: after success criteria hold,
                                emit success_criteria_met and wait for PATH
  --protocol-version <V>        2 omits every v3 Authenticate field [default: 3]
  --supported-topologies <LIST> Comma-separated topologies [default: relay,host,mesh]
  --supported-transports <LIST> Comma-separated transports [default: relay,webrtc]
  --mdns-obfuscation            Keep Chromium's mDNS candidate obfuscation ON
                                (default: disabled for deterministic loopback)
  --help                        Print this help
`;

/**
 * Parse `argv` (without the node/script prefix). Throws [`UsageError`] on any
 * usage problem; returns `null` when `--help` was requested.
 */
export function parseArgs(argv: string[]): CliOptions | null {
  const flags = new Map<string, string | true>();
  const valueFlags = new Set([
    '--server-url',
    '--join-code',
    '--peers',
    '--expect-total-peers',
    '--game-name',
    '--player-name',
    '--app-id',
    '--platform',
    '--relay-payload',
    '--p2p-timeout-secs',
    '--run-for-secs',
    '--max-runtime-secs',
    '--success-release-file',
    '--protocol-version',
    '--supported-topologies',
    '--supported-transports',
  ]);
  const booleanFlags = new Set([
    '--create-room',
    '--leave-on-game-start',
    '--exchange',
    '--cripple-ice',
    '--mdns-obfuscation',
    '--help',
  ]);

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === undefined) {
      break;
    }
    if (booleanFlags.has(arg)) {
      flags.set(arg, true);
      continue;
    }
    if (valueFlags.has(arg)) {
      const value = argv[index + 1];
      if (value === undefined) {
        throw new UsageError(`flag ${arg} requires a value`);
      }
      // clap parity: a known flag token never doubles as a value
      // (`--relay-payload --exchange` is a missing value, not the payload
      // "--exchange"). Tokens that merely start with `-` but are not known
      // flags (e.g. `--relay-payload -5`) remain legitimate values.
      if (valueFlags.has(value) || booleanFlags.has(value)) {
        throw new UsageError(`flag ${arg} requires a value, but got the flag ${value}`);
      }
      if (flags.has(arg)) {
        throw new UsageError(`flag ${arg} given more than once`);
      }
      flags.set(arg, value);
      index += 1;
      continue;
    }
    throw new UsageError(`unknown argument '${arg}'`);
  }

  if (flags.has('--help')) {
    return null;
  }

  const stringFlag = (flag: string, fallback: string): string => {
    const value = flags.get(flag);
    return typeof value === 'string' ? value : fallback;
  };
  const numberFlag = (flag: string, fallback: number): number => {
    const value = flags.get(flag);
    if (typeof value !== 'string') {
      return fallback;
    }
    const parsed = Number(value);
    if (!Number.isInteger(parsed) || parsed < 0) {
      throw new UsageError(`flag ${flag} requires a non-negative integer, got '${value}'`);
    }
    return parsed;
  };
  const listFlag = <T extends string>(
    flag: string,
    allowed: readonly T[],
    fallback: T[],
  ): T[] => {
    const value = flags.get(flag);
    if (typeof value !== 'string') {
      return fallback;
    }
    const tokens = value.split(',').map((token) => token.trim());
    for (const token of tokens) {
      if (!(allowed as readonly string[]).includes(token)) {
        throw new UsageError(
          `flag ${flag}: invalid value '${token}' (allowed: ${allowed.join(', ')})`,
        );
      }
    }
    return tokens as T[];
  };

  const serverUrl = flags.get('--server-url');
  if (typeof serverUrl !== 'string') {
    throw new UsageError('--server-url is required');
  }
  const createRoom = flags.get('--create-room') === true;
  const joinCode = flags.get('--join-code');
  if (createRoom === (typeof joinCode === 'string')) {
    throw new UsageError('exactly one of --create-room / --join-code is required');
  }

  const peers = numberFlag('--peers', 2);
  if (peers < 1 || peers > 255) {
    throw new UsageError(`--peers must be between 1 and 255, got ${peers}`);
  }

  const config: RunConfig = {
    serverUrl,
    createRoom,
    joinCode: typeof joinCode === 'string' ? joinCode : null,
    peers,
    expectTotalPeers: flags.has('--expect-total-peers')
      ? numberFlag('--expect-total-peers', 0)
      : null,
    leaveOnGameStart: flags.get('--leave-on-game-start') === true,
    gameName: stringFlag('--game-name', 'reference-browser'),
    playerName: stringFlag('--player-name', 'RefBrowser'),
    appId: stringFlag('--app-id', 'reference-browser-app'),
    platform: stringFlag('--platform', 'reference-browser'),
    exchange: flags.get('--exchange') === true,
    relayPayload: flags.has('--relay-payload') ? stringFlag('--relay-payload', '') : null,
    crippleIce: flags.get('--cripple-ice') === true,
    p2pTimeoutSecs: numberFlag('--p2p-timeout-secs', 15),
    runForSecs: numberFlag('--run-for-secs', 30),
    successReleaseEnabled: flags.has('--success-release-file'),
    protocolVersion: numberFlag('--protocol-version', 3),
    supportedTopologies: listFlag<Topology>('--supported-topologies', TOPOLOGIES, [
      'relay',
      'host',
      'mesh',
    ]),
    supportedTransports: listFlag<Transport>('--supported-transports', TRANSPORTS, [
      'relay',
      'webrtc',
    ]),
    sdkVersion: SDK_VERSION,
    // Re-measured by main right before the page engine starts.
    elapsedBeforeStartMs: 0,
  };

  return {
    config,
    maxRuntimeSecs: numberFlag('--max-runtime-secs', 60),
    successReleaseFile: flags.has('--success-release-file')
      ? stringFlag('--success-release-file', '')
      : null,
    mdnsObfuscation: flags.get('--mdns-obfuscation') === true,
  };
}
