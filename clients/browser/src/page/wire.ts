// WebSocket wire layer: JSON text frames carrying the server's
// `{type, data}` message envelope (docs/protocol.md; canonical samples in
// .agents/skills/websocket-protocol/references/). Unlike the native client, which consumes the
// server crate's serde types via a path dependency, the browser client
// hand-models only the envelope fields it actually reads — drift is caught by
// the interop suite, which drives this client against the real server binary.

import type { DeliveryClass } from './accountability.js';

const MAX_U32 = 0xffff_ffff;
const MAX_SAFE_BIGINT = BigInt(Number.MAX_SAFE_INTEGER);
/** Internal marker for browser-exposed binary application frames. */
export const NON_TEXT_APPLICATION_FRAME = '\u0000non_text_application_frame';

/** One parsed server frame: the envelope tag plus its (optional) payload. */
export interface ServerFrame {
  type: string;
  data: Record<string, unknown>;
}

export type BinaryGameDataEncoding = 'json' | 'message_pack' | 'rkyv';

/** Decoded v3 metadata with the application payload left byte-for-byte opaque. */
export interface V3BinaryGameDataFrame {
  fromPlayer: string;
  encoding: BinaryGameDataEncoding;
  payload: Uint8Array;
  seq: number;
  epoch: number;
}

type JsonValue = null | string | boolean | number | JsonValue[] | { [key: string]: JsonValue };

/** Time allowed for the WebSocket connection to open (mirrors wire.rs). */
export const CONNECT_TIMEOUT_MS = 10_000;
/** Per-message ceiling during the sequential handshake phase (mirrors wire.rs). */
export const HANDSHAKE_TIMEOUT_MS = 20_000;

/** Build one client→server frame in the `{type, data}` envelope. */
export function clientFrame(type: string, data?: Record<string, unknown>): string {
  return JSON.stringify(data === undefined ? { type } : { type, data });
}

/** Send reliable GameData while preserving the legacy omitted class/key shape. */
export function sendGameData(sendFrame: (frame: string) => void, data: unknown): void {
  const normalized = normalizeOutgoingJsonValue(data);
  sendFrame(serializeOutgoingGameData({ data: normalized }));
}

/** Send classified GameData after rejecting an illegal pair before the wire callback. */
export function sendGameDataWithDelivery(
  sendFrame: (frame: string) => void,
  data: unknown,
  className: DeliveryClass,
  key?: number,
): void {
  const delivery = normalizeOutgoingDelivery(className, key);
  const normalized = normalizeOutgoingJsonValue(data);
  sendFrame(
    serializeOutgoingGameData({
      data: normalized,
      class: delivery.className,
      ...(delivery.key === undefined ? {} : { key: delivery.key }),
    }),
  );
}

function serializeOutgoingGameData(data: Record<string, unknown>): string {
  try {
    return clientFrame('GameData', data);
  } catch (error) {
    throw new Error(
      `invalid outgoing GameData JSON value: serialization failed: ${
        error instanceof Error ? error.message : String(error)
      }`,
    );
  }
}

function normalizeOutgoingJsonValue(value: unknown): JsonValue {
  try {
    return normalizeJsonValue(value, '$', new Set<object>());
  } catch (error) {
    if (error instanceof Error && error.message.startsWith('invalid outgoing GameData')) {
      throw error;
    }
    throw new Error(
      `invalid outgoing GameData JSON value: validation failed: ${
        error instanceof Error ? error.message : String(error)
      }`,
    );
  }
}

function normalizeJsonValue(value: unknown, path: string, ancestors: Set<object>): JsonValue {
  if (value === null || typeof value === 'string' || typeof value === 'boolean') {
    return value;
  }
  if (typeof value === 'number') {
    // JSON.stringify silently maps NaN/Infinity to null. Reject instead so
    // the wire value never changes meaning behind the caller's back.
    if (!Number.isFinite(value)) {
      throw new Error(`invalid outgoing GameData JSON value: ${path} is not a finite number`);
    }
    return value;
  }
  if (typeof value !== 'object') {
    throw new Error(
      `invalid outgoing GameData JSON value: ${path} has unsupported type ${typeof value}`,
    );
  }
  if (ancestors.has(value)) {
    throw new Error(`invalid outgoing GameData JSON value: ${path} contains a cycle`);
  }
  ancestors.add(value);
  try {
    if (Array.isArray(value)) {
      const descriptors = Object.getOwnPropertyDescriptors(value) as Record<
        string,
        PropertyDescriptor
      >;
      const lengthDescriptor = descriptors['length'];
      const lengthValue: unknown = lengthDescriptor?.value;
      if (
        typeof lengthValue !== 'number' ||
        !Number.isSafeInteger(lengthValue) ||
        lengthValue < 0 ||
        lengthValue > MAX_U32
      ) {
        throw new Error(`invalid outgoing GameData JSON value: ${path} has an invalid length`);
      }
      const length = lengthValue;
      if (Object.getOwnPropertySymbols(value).length !== 0) {
        throw new Error(`invalid outgoing GameData JSON value: ${path} has a symbol property`);
      }
      for (const [key, descriptor] of Object.entries(descriptors)) {
        if (key === 'length' || !descriptor.enumerable) {
          continue;
        }
        if (!/^(0|[1-9]\d*)$/.test(key) || Number(key) >= length) {
          throw new Error(
            `invalid outgoing GameData JSON value: ${path} has a non-index array property`,
          );
        }
      }
      const normalized = new Array<JsonValue>(length);
      for (let index = 0; index < length; index += 1) {
        const descriptor = descriptors[String(index)];
        if (descriptor === undefined) {
          throw new Error(`invalid outgoing GameData JSON value: ${path}[${index}] is sparse`);
        }
        if (!('value' in descriptor)) {
          throw new Error(
            `invalid outgoing GameData JSON value: ${path}[${index}] is an accessor`,
          );
        }
        normalized[index] = normalizeJsonValue(
          descriptor.value,
          `${path}[${index}]`,
          ancestors,
        );
      }
      Object.setPrototypeOf(normalized, null);
      return normalized;
    }

    const prototype = Object.getPrototypeOf(value);
    if (prototype !== Object.prototype && prototype !== null) {
      throw new Error(`invalid outgoing GameData JSON value: ${path} is not a plain object`);
    }
    if (Object.getOwnPropertySymbols(value).length !== 0) {
      throw new Error(`invalid outgoing GameData JSON value: ${path} has a symbol property`);
    }
    const normalized = Object.create(null) as { [key: string]: JsonValue };
    for (const [key, descriptor] of Object.entries(Object.getOwnPropertyDescriptors(value))) {
      if (!descriptor.enumerable) {
        continue;
      }
      if (!('value' in descriptor)) {
        throw new Error(`invalid outgoing GameData JSON value: ${path}.${key} is an accessor`);
      }
      normalized[key] = normalizeJsonValue(descriptor.value, `${path}.${key}`, ancestors);
    }
    return normalized;
  } finally {
    ancestors.delete(value);
  }
}

function normalizeOutgoingDelivery(
  classValue: unknown,
  keyValue: unknown,
): { className: DeliveryClass; key?: number } {
  if (classValue !== 'reliable' && classValue !== 'latest' && classValue !== 'volatile') {
    throw new Error(`invalid outgoing GameData delivery: unknown class ${String(classValue)}`);
  }
  let key: number | undefined;
  if (keyValue !== undefined) {
    if (
      typeof keyValue !== 'number' ||
      !Number.isSafeInteger(keyValue) ||
      keyValue < 0 ||
      keyValue > MAX_U32
    ) {
      throw new Error(
        `invalid outgoing GameData delivery: key must be an integer in 0..=${MAX_U32}`,
      );
    }
    key = keyValue;
  }
  if (classValue === 'latest' ? key === undefined : key !== undefined) {
    throw new Error(
      'invalid outgoing GameData delivery: latest requires a key; reliable and volatile forbid one',
    );
  }
  return { className: classValue, ...(key === undefined ? {} : { key }) };
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

/** Decode the mandatory MessagePack metadata envelope on a v3 binary frame. */
export function parseV3BinaryGameDataFrame(wire: ArrayBuffer | ArrayBufferView): ServerFrame {
  const bytes = ArrayBuffer.isView(wire)
    ? new Uint8Array(wire.buffer, wire.byteOffset, wire.byteLength)
    : new Uint8Array(wire);
  const reader = new MessagePackReader(bytes);
  const fieldCount = reader.readMapLength('envelope');
  if (fieldCount !== 5) {
    throw new Error(
      `v3 binary GameData envelope must contain exactly 5 fields, got ${fieldCount}`,
    );
  }

  let fromPlayer: string | undefined;
  let encodingValue: BinaryGameDataEncoding | undefined;
  let payload: Uint8Array | undefined;
  let seq: number | undefined;
  let epoch: number | undefined;
  const seenKeys = new Set<string>();

  for (let index = 0; index < fieldCount; index += 1) {
    const key = reader.readString('envelope key');
    if (seenKeys.has(key)) {
      throw new Error(`v3 binary GameData envelope contains duplicate key: ${key}`);
    }
    seenKeys.add(key);
    switch (key) {
      case 'from_player':
        fromPlayer = decodeUuidBytes(reader.readBinary('from_player'));
        break;
      case 'encoding': {
        const encoding = reader.readString('encoding');
        if (encoding !== 'json' && encoding !== 'message_pack' && encoding !== 'rkyv') {
          throw new Error('v3 binary GameData encoding is invalid');
        }
        encodingValue = encoding;
        break;
      }
      case 'payload':
        payload = reader.readBinary('payload');
        break;
      case 'seq':
        seq = reader.readPositiveInteger('seq', MAX_SAFE_BIGINT);
        break;
      case 'epoch':
        epoch = reader.readPositiveInteger('epoch', BigInt(MAX_U32));
        break;
      default:
        throw new Error(`v3 binary GameData envelope contains unknown field: ${key}`);
    }
  }
  reader.requireEnd();

  if (
    fromPlayer === undefined ||
    encodingValue === undefined ||
    payload === undefined ||
    seq === undefined ||
    epoch === undefined
  ) {
    throw new Error('v3 binary GameData envelope is missing a required field');
  }
  return {
    type: 'GameDataBinary',
    data: {
      from_player: fromPlayer,
      encoding: encodingValue,
      payload,
      seq,
      epoch,
    },
  };
}

/** Marker-aware reader for the five flat fields in the v3 binary envelope. */
class MessagePackReader {
  private offset = 0;
  private readonly view: DataView;

  constructor(private readonly bytes: Uint8Array) {
    this.view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  }

  readMapLength(field: string): number {
    const marker = this.readByte(field);
    if (marker >= 0x80 && marker <= 0x8f) {
      return marker & 0x0f;
    }
    if (marker === 0xde) {
      return this.readUnsigned(2, field);
    }
    if (marker === 0xdf) {
      return this.readUnsigned(4, field);
    }
    throw new Error(`v3 binary GameData ${field} is not a map`);
  }

  readString(field: string): string {
    const marker = this.readByte(field);
    let length: number;
    if (marker >= 0xa0 && marker <= 0xbf) {
      length = marker & 0x1f;
    } else if (marker === 0xd9) {
      length = this.readUnsigned(1, field);
    } else if (marker === 0xda) {
      length = this.readUnsigned(2, field);
    } else if (marker === 0xdb) {
      length = this.readUnsigned(4, field);
    } else {
      throw new Error(`v3 binary GameData ${field} is not a string`);
    }
    const value = this.readBytes(length, field);
    try {
      return new TextDecoder('utf-8', { fatal: true }).decode(value);
    } catch {
      throw new Error(`v3 binary GameData ${field} is not valid UTF-8`);
    }
  }

  readBinary(field: string): Uint8Array {
    const marker = this.readByte(field);
    let length: number;
    if (marker === 0xc4) {
      length = this.readUnsigned(1, field);
    } else if (marker === 0xc5) {
      length = this.readUnsigned(2, field);
    } else if (marker === 0xc6) {
      length = this.readUnsigned(4, field);
    } else {
      throw new Error(`v3 binary GameData ${field} is not binary data`);
    }
    return this.readBytes(length, field);
  }

  readPositiveInteger(field: string, maximum: bigint): number {
    const marker = this.readByte(field);
    let value: bigint;
    if (marker <= 0x7f) {
      value = BigInt(marker);
    } else {
      switch (marker) {
        case 0xcc:
          value = BigInt(this.readUnsigned(1, field));
          break;
        case 0xcd:
          value = BigInt(this.readUnsigned(2, field));
          break;
        case 0xce:
          value = BigInt(this.readUnsigned(4, field));
          break;
        case 0xcf:
          value = this.readUnsigned64(field);
          break;
        case 0xd0:
          value = BigInt(this.readSigned(1, field));
          break;
        case 0xd1:
          value = BigInt(this.readSigned(2, field));
          break;
        case 0xd2:
          value = BigInt(this.readSigned(4, field));
          break;
        case 0xd3:
          value = this.readSigned64(field);
          break;
        default:
          throw new Error(`v3 binary GameData ${field} is not an integer`);
      }
    }
    if (value <= 0n || value > maximum) {
      throw new Error(`v3 binary GameData ${field} is outside 1..=${maximum}`);
    }
    return Number(value);
  }

  requireEnd(): void {
    if (this.offset !== this.bytes.byteLength) {
      throw new Error('v3 binary GameData envelope contains trailing bytes');
    }
  }

  private readByte(field: string): number {
    this.requireAvailable(1, field);
    return this.bytes[this.offset++] as number;
  }

  private readBytes(length: number, field: string): Uint8Array {
    this.requireAvailable(length, field);
    const value = this.bytes.subarray(this.offset, this.offset + length);
    this.offset += length;
    return value;
  }

  private readUnsigned(width: 1 | 2 | 4, field: string): number {
    this.requireAvailable(width, field);
    const value =
      width === 1
        ? this.view.getUint8(this.offset)
        : width === 2
          ? this.view.getUint16(this.offset)
          : this.view.getUint32(this.offset);
    this.offset += width;
    return value;
  }

  private readSigned(width: 1 | 2 | 4, field: string): number {
    this.requireAvailable(width, field);
    const value =
      width === 1
        ? this.view.getInt8(this.offset)
        : width === 2
          ? this.view.getInt16(this.offset)
          : this.view.getInt32(this.offset);
    this.offset += width;
    return value;
  }

  private readUnsigned64(field: string): bigint {
    this.requireAvailable(8, field);
    const value = this.view.getBigUint64(this.offset);
    this.offset += 8;
    return value;
  }

  private readSigned64(field: string): bigint {
    this.requireAvailable(8, field);
    const value = this.view.getBigInt64(this.offset);
    this.offset += 8;
    return value;
  }

  private requireAvailable(length: number, field: string): void {
    if (length > this.bytes.byteLength - this.offset) {
      throw new Error(
        `v3 binary GameData ${field} is truncated: declared ${length} bytes, found ${this.bytes.byteLength - this.offset}`,
      );
    }
  }
}

function decodeUuidBytes(value: unknown): string {
  if (!(value instanceof Uint8Array) || value.length !== 16) {
    throw new Error('v3 binary GameData from_player is not a 16-byte UUID');
  }
  const hex = Array.from(value, (byte) => byte.toString(16).padStart(2, '0')).join('');
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(
    16,
    20,
  )}-${hex.slice(20)}`;
}

/** Classify browser WebSocket `message.data` without losing arrival order. */
export function classifyBrowserServerInput(data: unknown): ServerFrame {
  if (typeof data === 'string') {
    return parseServerFrame(data);
  }
  if (data instanceof ArrayBuffer || ArrayBuffer.isView(data)) {
    try {
      return parseV3BinaryGameDataFrame(data);
    } catch (error) {
      return {
        type: NON_TEXT_APPLICATION_FRAME,
        data: { error: error instanceof Error ? error.message : String(error) },
      };
    }
  }
  return { type: NON_TEXT_APPLICATION_FRAME, data: {} };
}

/** Parse the text-only application stream selected by `game_data_format=json`. */
export function classifyJsonNegotiatedServerInput(data: unknown): ServerFrame {
  if (typeof data !== 'string') {
    throw new Error(
      'received a non-text WebSocket frame while game_data_format=json was negotiated',
    );
  }
  const frame = parseServerFrame(data);
  if (frame.type === 'GameDataBinary') {
    throw new Error('received text GameDataBinary while game_data_format=json was negotiated');
  }
  return frame;
}

/** Extract the physical connection's negotiated mode from ProtocolInfo. */
export function negotiatedProtocolVersion(frame: ServerFrame, offeredVersion: number): number {
  if (frame.type !== 'ProtocolInfo') {
    throw new Error(`expected ProtocolInfo, got ${frame.type}`);
  }
  const value = frame.data['protocol_version'];
  if (value === undefined) {
    return 2;
  }
  if (
    typeof value !== 'number' ||
    !Number.isSafeInteger(value) ||
    value < 2 ||
    value > 3 ||
    value > offeredVersion
  ) {
    throw new Error(
      'ProtocolInfo.protocol_version must be an integer in 2..=3 no greater than the offered version',
    );
  }
  return value;
}

/**
 * Open the WebSocket and resolve once the connection is established.
 * Rejection means a transport-level failure (exit code 3 territory).
 */
export function connect(serverUrl: string): Promise<WebSocket> {
  return new Promise((resolve, reject) => {
    let settled = false;
    const ws = new WebSocket(serverUrl);
    // Normalize binary application messages to synchronous ArrayBuffer input;
    // Blob conversion would otherwise reorder them around later text frames.
    ws.binaryType = 'arraybuffer';
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
