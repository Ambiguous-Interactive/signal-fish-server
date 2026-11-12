import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { performance } from "node:perf_hooks";
import { encode, decode } from "@msgpack/msgpack";
import {
  create,
  fromBinary,
  toBinary,
  type DescMessage,
} from "@bufbuild/protobuf";
import { InputSchema, SnapshotSchema, DeltaSchema } from "./game_pb.js";

type Value = number | boolean | string | Value[] | { [key: string]: Value };
const corpus = new URL("../../../test-fixtures/encoding/", import.meta.url);
const manifest = JSON.parse(
  readFileSync(new URL("manifest.json", corpus), "utf8"),
);
const checkOnly = process.argv.includes("--check");
const seedText = process.argv.find((arg) => arg.startsWith("--seed="))?.slice(7) ?? "1524";
assert(/^[0-9]+$/.test(seedText), "seed must be an unsigned 32-bit integer");
const seed = Number(seedText);
assert(Number.isSafeInteger(seed) && seed <= 0xffffffff, "seed exceeds uint32");
const warmupMs = 1000;
const sampleCount = 10;
function bits(value: Value): string[] {
  if (Array.isArray(value)) return value.flatMap(bits);
  if (value !== null && typeof value === "object") {
    return Object.entries(value).flatMap(([key, item]) =>
      key === "position" || key === "velocity"
        ? (item as number[]).map((n) => {
            const b = Buffer.alloc(8);
            b.writeDoubleLE(n);
            return b.toString("hex");
          })
        : bits(item),
    );
  }
  return [];
}
function protobufInput(name: string, value: Value): Record<string, unknown> {
  if (name === "input-state") {
    const [tick, buttons, horizontal, vertical] = value as number[];
    return { tick, buttons, horizontal, vertical };
  }
  const object = value as Record<string, Value>;
  if (name.startsWith("snapshot")) return object;
  return {
    ...object,
    baseTick: object.base_tick,
    tiles: (object.tiles as number[][]).map(([x, y, tileType]) => ({
      x,
      y,
      tileType,
    })),
  };
}
function logical(name: string, value: any): Value {
  if (name === "input-state")
    return [value.tick, value.buttons, value.horizontal, value.vertical];
  const entity = (v: any) => ({
    id: v.id,
    position: v.position,
    velocity: v.velocity,
    health: v.health,
    ammo: v.ammo,
    active: v.active,
  });
  if (name.startsWith("snapshot"))
    return {
      tick: value.tick,
      entities: value.entities.map(entity),
      map: value.map,
    };
  return {
    tick: value.tick,
    base_tick: value.baseTick,
    spawned: value.spawned.map(entity),
    updated: value.updated.map((v: any) => ({
      id: v.id,
      position: v.position,
      health: v.health,
      animation: v.animation,
    })),
    removed: value.removed,
    tiles: value.tiles.map((v: any) => [v.x, v.y, v.tileType]),
  };
}
type Timing = {
  iterations: number;
  warmup_iterations: number;
  warmup_ns: number;
  samples_ns: number[];
  order_positions: number[];
  checksum: number;
};
function consume(value: any): number {
  return value.byteLength ?? value.tick ?? value[0] ?? 1;
}
function warmup(operation: () => unknown): Timing {
  let checksum = 0;
  let count = 0;
  const start = performance.now();
  do {
    checksum = (checksum + consume(operation())) >>> 0;
    count++;
  } while (performance.now() - start < warmupMs);
  return {
    iterations: Math.max(1, Math.floor(count / sampleCount)),
    warmup_iterations: count,
    warmup_ns: (performance.now() - start) * 1e6,
    samples_ns: [],
    order_positions: [],
    checksum,
  };
}
function sample(operation: () => unknown, timing: Timing, position: number): void {
  const start = performance.now();
  for (let i = 0; i < timing.iterations; i++)
    timing.checksum = (timing.checksum + consume(operation())) >>> 0;
  timing.samples_ns.push(((performance.now() - start) * 1e6) / timing.iterations);
  timing.order_positions.push(position);
}
// The library's default integer optimization loses negative zero. Keep this
// negative control and measure its documented all-number float64 mode.
assert(!Object.is(decode(encode(-0)), -0));
assert(Object.is(decode(encode(-0, { forceIntegerToFloat: true })), -0));
for (const [fixtureIndex, fixture] of manifest.fixtures.entries()) {
  const bytes = readFileSync(new URL(fixture.file, corpus));
  assert.equal(bytes.byteLength, fixture.bytes);
  assert.equal(
    createHash("sha256").update(bytes).digest("hex"),
    fixture.sha256,
  );
  const source = JSON.parse(bytes.toString("utf8")) as Value;
  const schema: DescMessage =
    fixture.name === "input-state"
      ? InputSchema
      : fixture.name.startsWith("snapshot")
        ? SnapshotSchema
        : DeltaSchema;
  const message = create(schema, protobufInput(fixture.name, source));
  const codecs = [
    {
      name: "msgpack-all-f64",
      encode: () => encode(source, { forceIntegerToFloat: true }),
      decode: (b: Uint8Array) => decode(b) as Value,
    },
    {
      name: "protobuf",
      encode: () => toBinary(schema, message),
      decode: (b: Uint8Array) => fromBinary(schema, b) as unknown as Value,
    },
  ];
  const prepared = codecs.map((codec) => {
    const wire = codec.encode();
    const rawDecoded = codec.decode(wire);
    const decoded =
      codec.name === "protobuf"
        ? logical(fixture.name, rawDecoded)
        : rawDecoded;
    assert.deepEqual(decoded, source);
    assert.deepEqual(bits(decoded), bits(source));
    assert.deepEqual(codec.encode(), wire);
    const record = {
      language: "typescript",
      runtime: process.version,
      fixture: fixture.name,
      codec: codec.name,
      bytes: wire.byteLength,
      wire_base64: Buffer.from(wire).toString("base64"),
      logical: decoded,
      float_bits: bits(decoded),
      encode: null as Timing | null,
      decode: null as Timing | null,
      benchmark: null as null | {
        method: string; seed: number; warmup_ms: number;
        warmup_order: string[]; sample_orders: string[][];
      },
    };
    return { codec, wire, record };
  });
  if (!checkOnly) {
    const benchmark = {
      method: "seeded-alternating", seed, warmup_ms: warmupMs,
      warmup_order: [] as string[], sample_orders: [] as string[][],
    };
    const ordered = [...prepared];
    if ((seed + fixtureIndex) % 2 !== 0) ordered.reverse();
    for (const item of ordered) {
      benchmark.warmup_order.push(item.codec.name);
      item.record.encode = warmup(item.codec.encode);
      item.record.decode = warmup(() => item.codec.decode(item.wire));
      item.record.benchmark = benchmark;
    }
    for (let index = 0; index < sampleCount; index++) {
      if (index !== 0) ordered.reverse();
      const executed: string[] = [];
      for (const [position, item] of ordered.entries()) {
        executed.push(item.codec.name);
        sample(item.codec.encode, item.record.encode!, position);
        sample(() => item.codec.decode(item.wire), item.record.decode!, position);
      }
      benchmark.sample_orders.push(executed);
    }
  }
  for (const item of prepared) console.log(JSON.stringify(item.record));
}
// Values outside the corpus pin Unicode, empty lists and floating-point bits.
for (const value of [
  -0,
  0.1,
  Number.MIN_VALUE,
  Number.MAX_VALUE,
  "空🎮",
  [],
  {},
]) {
  assert.deepEqual(decode(encode(value, { forceIntegerToFloat: true })), value);
}

const edgeMessage = create(SnapshotSchema, {
  tick: 0xffffffff,
  map: "空🎮",
  entities: [
    {
      id: 0xffffffff,
      position: [-0, 0.1, Number.MIN_VALUE, Number.MAX_VALUE],
      velocity: [],
      health: 0,
      ammo: 0,
      active: false,
    },
  ],
});
assert.deepEqual(
  fromBinary(SnapshotSchema, toBinary(SnapshotSchema, edgeMessage)),
  edgeMessage,
);
const edgeInput = create(InputSchema, {
  tick: 0xffffffff,
  buttons: 0xffffffff,
  horizontal: -2147483648,
  vertical: 2147483647,
});
assert.deepEqual(
  fromBinary(InputSchema, toBinary(InputSchema, edgeInput)),
  edgeInput,
);
