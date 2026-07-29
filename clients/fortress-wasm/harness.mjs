import { createServer } from "node:http";
import { createRequire } from "node:module";
import { createHash, randomUUID } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { isAbsolute, join, normalize, resolve, sep } from "node:path";
import { spawn } from "node:child_process";
import net from "node:net";

const expectedThresholds = {
  target_confirmed_frames: 600,
  nominal_fps: 60,
  min_checksum_samples: 8,
  min_completed_messages_per_second: 120,
  max_pipeline_queue_depth: 64,
  max_oldest_queue_age_us: 500000,
  max_confirmation_lag: 8,
  max_rollback_depth: 8,
};
const reportKeys = [
  "schema_version", "status", "origin", "runtime_error", "role", "run_mode",
  "instance_nonce", "expected_remote_nonce", "player_id", "remote_player_id", "room_code",
  "browser_process_id", "browser_artifact", "build_sha", "signal_fish_client_version",
  "signal_fish_client_godot_version", "fortress_rollback_version", "godot_rust_version",
  "godot_runtime", "target", "target_os", "godot_threads",
  "worker_count", "callback_count", "poll_count", "active_callback_count", "callback_intervals",
  "acceptance_thresholds", "max_admissions_per_callback", "current_frame", "confirmed_frame",
  "game_frame", "game_checksum", "frames_advanced", "rollback_count", "max_rollback_depth",
  "stall_count", "wait_recommendations", "confirmation_lag_current", "confirmation_lag_max",
  "checksums_mismatched", "checksums_compared", "checksums_matched", "events_discarded_total",
  "client_game_data_sent", "client_game_data_sent_during_run", "client_game_data_received",
  "client_messages_undecodable", "final_pipeline_queue_depth", "peak_pipeline_queue_depth",
  "peak_oldest_queue_age_us", "relay_frames_enqueued", "relay_frames_enqueued_during_run",
  "relay_frames_received", "relay_malformed", "relay_wrong_destination", "relay_unknown_sender",
  "relay_outbound_overflow", "relay_inbound_overflow", "relay_encode_failures",
  "relay_completion_underflow", "relay_send_retries", "running_elapsed_ms",
  "relay_sent_sequence_count", "relay_sent_first_sequence", "relay_sent_last_sequence",
  "relay_sent_sequence_hash", "relay_received_sequence_count", "relay_received_first_sequence",
  "relay_received_last_sequence", "relay_received_sequence_hash",
];

const [mode, exportDirectoryArg, serverBinaryArg, artifactDirectoryArg, buildSha] =
  process.argv.slice(2);
if (!mode || !exportDirectoryArg || !serverBinaryArg || !artifactDirectoryArg || !buildSha) {
  throw new Error(
    "usage: node harness.mjs <released|negative> <export-dir> <server-bin> <artifacts-dir> <build-sha>",
  );
}
if (!new Set(["released", "negative"]).has(mode)) {
  throw new Error(`unsupported run mode: ${mode}`);
}
if (!isAbsolute(serverBinaryArg) || !existsSync(serverBinaryArg)) {
  throw new Error("server binary must be an existing absolute path");
}
if (!/^[0-9a-f]{40}$/.test(buildSha)) {
  throw new Error("build SHA must be a full lowercase Git object id");
}

const fixtureDirectory = resolve(import.meta.dirname);
const exportDirectory = resolve(exportDirectoryArg);
const artifactDirectory = resolve(artifactDirectoryArg, mode);
mkdirSync(artifactDirectory, { recursive: true });
const requireFromBrowser = createRequire(
  join(fixtureDirectory, "..", "browser", "package.json"),
);
const browserName = process.env.FORTRESS_WASM_BROWSER ?? "chromium";
if (!new Set(["chromium", "firefox"]).has(browserName)) {
  throw new Error(`unsupported browser: ${browserName}`);
}
const playwright = requireFromBrowser("playwright-core");
const browserType = playwright[browserName];
const browserExecutable = browserType.executablePath();
const browserStat = statSync(browserExecutable);
const browserArtifact = `${browserExecutable}:${browserStat.size}`;
const browserArtifactSha256 = createHash("sha256")
  .update(readFileSync(browserExecutable))
  .digest("hex");

const serverPort = await freePort();
const httpPort = await freePort();
const serverLog = join(artifactDirectory, "server.log");
const server = spawn(serverBinaryArg, [], {
  env: cleanServerEnvironment(serverPort),
  stdio: ["ignore", "pipe", "pipe"],
});
const serverChunks = [];
server.stdout.on("data", (chunk) => serverChunks.push(chunk));
server.stderr.on("data", (chunk) => serverChunks.push(chunk));
const httpServer = createServer((request, response) => {
  try {
    const pathname = new URL(request.url ?? "/", "http://fixture.invalid").pathname;
    if (pathname === "/favicon.ico") {
      response.writeHead(204, { "cache-control": "no-store" });
      response.end();
      return;
    }
    const relative = pathname === "/" ? "index.html" : pathname.slice(1);
    const candidate = normalize(join(exportDirectory, relative));
    if (!candidate.startsWith(`${exportDirectory}${sep}`) || !existsSync(candidate)) {
      response.writeHead(404, { "content-type": "text/plain" });
      response.end("not found");
      return;
    }
    response.writeHead(200, {
      "cache-control": "no-store",
      "content-type": contentType(candidate),
    });
    response.end(readFileSync(candidate));
  } catch (error) {
    response.writeHead(500, { "content-type": "text/plain" });
    response.end(String(error));
  }
});

let creator;
let joiner;
let creatorReport;
let joinerReport;
try {
  await waitForTcp(serverPort, 10_000);
  await new Promise((accept, reject) => {
    httpServer.once("error", reject);
    httpServer.listen(httpPort, "127.0.0.1", accept);
  });
  const pageUrl = `http://127.0.0.1:${httpPort}/index.html`;
  const creatorNonce = randomUUID();
  const joinerNonce = randomUUID();
  creator = await launchPeer({
    role: "creator",
    roomCode: null,
    instanceNonce: creatorNonce,
    expectedRemoteNonce: joinerNonce,
    pageUrl,
  });
  const room = await waitForGlobal(creator, "__FORTRESS_ROOM_READY", 15_000);
  assertExactKeys(room, ["schema_version", "role", "instance_nonce", "room_code"], "room-ready");
  assert(room.schema_version === 2, "room-ready schema mismatch");
  assert(room.role === "creator", "room-ready role mismatch");
  assert(room.instance_nonce === creatorNonce, "room-ready nonce mismatch");
  assert(typeof room.room_code === "string" && room.room_code.length > 0, "empty room code");

  joiner = await launchPeer({
    role: "joiner",
    roomCode: room.room_code,
    instanceNonce: joinerNonce,
    expectedRemoteNonce: creatorNonce,
    pageUrl,
  });

  [creatorReport, joinerReport] = await Promise.all([
    waitForGlobal(creator, "__FORTRESS_RESULT", 105_000),
    waitForGlobal(joiner, "__FORTRESS_RESULT", 105_000),
  ]);
  await new Promise((accept) => setTimeout(accept, 250));
  const creatorBrowser = await browserAttestation(creator);
  const joinerBrowser = await browserAttestation(joiner);
  writeFileSync(
    join(artifactDirectory, "creator-report.json"),
    `${JSON.stringify({ report: creatorReport, browser: creatorBrowser }, null, 2)}\n`,
  );
  writeFileSync(
    join(artifactDirectory, "joiner-report.json"),
    `${JSON.stringify({ report: joinerReport, browser: joinerBrowser }, null, 2)}\n`,
  );

  validateIdentityAndRuntime(
    creatorReport,
    joinerReport,
    creatorBrowser,
    joinerBrowser,
    room.room_code,
  );
  const peerHealth = [
    ["creator", creatorReport, healthViolations("creator", creatorReport)],
    ["joiner", joinerReport, healthViolations("joiner", joinerReport)],
  ];
  const healthyViolations = peerHealth.flatMap(([, , violations]) => violations);
  if (mode === "released") {
    for (const [name, report, violations] of peerHealth) {
      assert(
        report.max_admissions_per_callback > 1,
        `${name}: released graph never attempted multi-send admission`,
      );
      assert(
        report.callback_intervals.mean_us >= 8_000,
        `${name}: released graph observed a synthetic/too-fast callback mean`,
      );
      assert(
        violations.length === 0,
        `${name}: released graph failed P13 healthy gates:\n${violations.join("\n")}`,
      );
    }
    process.stdout.write(
      "HEALTHY fortress-wasm released-client interoperability\n",
    );
  } else {
    for (const [name, report, violations] of peerHealth) {
      assert(
        report.active_callback_count >= 600,
        `${name}: negative control did not run the healthy control's 600-callback active budget`,
      );
      assert(
        report.max_admissions_per_callback === 1,
        `${name}: negative control did not exercise exactly one maximum admission per callback`,
      );
      assert(
        report.callback_intervals.mean_us >= 8_000,
        `${name}: negative control observed a synthetic/too-fast callback mean`,
      );
      assert(violations.length > 0, `${name}: negative control unexpectedly satisfied every healthy gate`);
      assert(
        report.relay_frames_enqueued_during_run >= 600 &&
          report.client_game_data_sent_during_run >= 600,
        `${name}: negative control did not exercise a non-vacuous capped workload`,
      );
      assert(
        report.checksums_matched === report.checksums_compared,
        `${name}: negative control checksum accounting disagreed`,
      );
      assert(
        report.client_game_data_sent_during_run <= report.active_callback_count * 2,
        `${name}: negative control did not break the per-callback completion gate`,
      );
      assert(
        report.client_game_data_sent_during_run * 1_000 < report.running_elapsed_ms * 120,
        `${name}: negative control did not break the completed-rate gate`,
      );
      assert(
        violations.every((violation) =>
          new RegExp(
            "current_frame=|confirmed_frame=|insufficient Fortress advancement|" +
              "fewer than 1200|non-nominal callback mean|oldest queue age|" +
              "stall_count|wait_recommendations|checksum agreement gate failed|" +
              "confirmation lag exceeded|rollback depth=|active wall time|" +
              "completed rate|sends per callback",
          ).test(violation),
        ),
        `${name}: negative control developed an unrelated healthy-gate failure:\n${violations.join("\n")}`,
      );
    }
    process.stdout.write(
      `BUSTED fortress-wasm expected negative control (${healthyViolations.length} healthy-gate violations)\n`,
    );
  }
} catch (error) {
  process.stderr.write(`BUSTED fortress-wasm ${mode}: ${error.stack ?? error}\n`);
  process.exitCode = 1;
} finally {
  await persistPeerArtifacts(joiner, joinerReport);
  await persistPeerArtifacts(creator, creatorReport);
  await closePeer(joiner);
  await closePeer(creator);
  httpServer.close();
  server.kill("SIGTERM");
  await Promise.race([
    new Promise((accept) => server.once("exit", accept)),
    new Promise((accept) => setTimeout(accept, 2_000)),
  ]);
  if (server.exitCode === null) server.kill("SIGKILL");
  writeFileSync(serverLog, Buffer.concat(serverChunks));
}

async function launchPeer({ role, roomCode, instanceNonce, expectedRemoteNonce, pageUrl }) {
  const logs = [];
  const errors = [];
  const launchOptions = {
    headless: true,
    executablePath: browserExecutable,
  };
  if (browserName === "chromium") {
    launchOptions.args = [
      "--disable-background-timer-throttling",
      "--disable-backgrounding-occluded-windows",
      "--disable-frame-rate-limit",
      "--disable-gpu-vsync",
      "--disable-renderer-backgrounding",
      "--disable-features=CalculateNativeWinOcclusion",
      "--enable-webgl",
      "--ignore-gpu-blocklist",
    ];
  } else {
    // Firefox parity for the Chromium arguments above. A CI runner has no GPU,
    // and the Godot web export aborts at boot on a missing WebGL2 feature
    // rather than rendering, so Firefox has to be pushed onto a software GL
    // stack from both directions:
    //
    //   - `webgl.force-enabled` bypasses the blocklist that refuses WebGL when
    //     no accelerated adapter is present. On a host that already resolves a
    //     software GL driver this alone is enough (verified by pref bisection:
    //     `webgl.forbid-software`, `gfx.webrender.*`, and
    //     `disable-fail-if-major-performance-caveat` each fail on their own).
    //   - `gfx.webrender.software` / `.all` keep compositing off the GPU path.
    //   - `LIBGL_ALWAYS_SOFTWARE` / `GALLIUM_DRIVER` force Mesa to load
    //     llvmpipe. Bypassing the blocklist does nothing if the loader never
    //     resolves a driver: Playwright's Firefox dependency list ships no GL
    //     packages at all (unlike its Chromium and WebKit lists), so the
    //     workflow installs Mesa's software rasterizer alongside these.
    //
    // The timer prefs keep background throttling off so the measured 60 Hz
    // callback cadence stays comparable across browsers.
    launchOptions.firefoxUserPrefs = {
      "webgl.force-enabled": true,
      "webgl.forbid-software": false,
      "webgl.disable-fail-if-major-performance-caveat": true,
      "gfx.webrender.software": true,
      "gfx.webrender.all": true,
      "dom.min_background_timeout_value": 4,
      "dom.timeout.enable_budget_timer_throttling": false,
    };
    launchOptions.env = {
      ...process.env,
      LIBGL_ALWAYS_SOFTWARE: "1",
      GALLIUM_DRIVER: "llvmpipe",
    };
  }
  const server = await browserType.launchServer(launchOptions);
  const pid = server.process()?.pid;
  const peer = {
    role,
    pid,
    server,
    browser: null,
    context: null,
    page: null,
    logs,
    errors,
  };
  try {
    assert(Number.isInteger(pid) && pid > 0, `${role}: missing independent ${browserName} PID`);
    peer.browser = await browserType.connect(server.wsEndpoint());
    peer.context = await peer.browser.newContext();
    await peer.context.addInitScript(
    ({ config }) => {
      globalThis.__FORTRESS_CONFIG = config;
      globalThis.__FORTRESS_WORKER_CONSTRUCTIONS = 0;
      for (const name of ["Worker", "SharedWorker"]) {
        const Original = globalThis[name];
        if (typeof Original === "function") {
          globalThis[name] = new Proxy(Original, {
            construct(target, args, newTarget) {
              globalThis.__FORTRESS_WORKER_CONSTRUCTIONS += 1;
              return Reflect.construct(target, args, newTarget);
            },
          });
        }
      }
    },
    {
      config: {
        schema_version: 2,
        server_url: `ws://127.0.0.1:${serverPort}/v2/ws`,
        role,
        room_code: roomCode,
        instance_nonce: instanceNonce,
        expected_remote_nonce: expectedRemoteNonce,
        run_mode: mode === "released" ? "healthy" : "negative_one_admission_per_callback",
        build_sha: buildSha,
        browser_process_id: pid,
        browser_artifact: `${browserArtifact}:${browserArtifactSha256}`,
      },
    },
    );
    peer.page = await peer.context.newPage();
    peer.page.on("console", (message) => {
      const line = `${message.type()}: ${message.text()}`;
      logs.push(line);
      if (message.type() === "error") errors.push(line);
    });
    peer.page.on("pageerror", (error) => errors.push(`pageerror: ${error.stack ?? error}`));
    peer.page.on("crash", () => errors.push("page crashed"));
    await assertWebGl2Available(peer);
    await peer.page.goto(pageUrl, { waitUntil: "load", timeout: 15_000 });
    return peer;
  } catch (error) {
    errors.push(`launch: ${error.stack ?? error}`);
    await persistPeerArtifacts(peer);
    await closePeer(peer);
    throw error;
  }
}

/// Fail before loading the export if the browser cannot give it a WebGL2
/// context, reporting the browser's own reason.
///
/// The Godot web export needs WebGL2 and aborts at boot without it. Left to
/// itself that surfaces as a bare 15-second wait for a page global, which is
/// how the Firefox cell stayed unexplained across several runs. A canvas
/// records `webglcontextcreationerror` with a `statusMessage` naming the actual
/// obstacle (blocklisted adapter, no driver, failed context), so ask for it
/// directly and put it in the failure.
async function assertWebGl2Available(peer) {
  const probe = await peer.page.evaluate(() => {
    const canvas = document.createElement("canvas");
    let statusMessage = null;
    canvas.addEventListener(
      "webglcontextcreationerror",
      (event) => {
        statusMessage = event.statusMessage ?? "(no statusMessage)";
      },
      { once: true },
    );
    const gl = canvas.getContext("webgl2");
    if (!gl) return { available: false, statusMessage, renderer: null };
    const debug = gl.getExtension("WEBGL_debug_renderer_info");
    return {
      available: true,
      statusMessage,
      renderer: debug
        ? gl.getParameter(debug.UNMASKED_RENDERER_WEBGL)
        : gl.getParameter(gl.RENDERER),
    };
  });
  peer.logs.push(`webgl2: available=${probe.available} renderer=${probe.renderer}`);
  assert(
    probe.available,
    `${peer.role}: ${browserName} cannot create a WebGL2 context, which the Godot ` +
      `web export requires to boot: ${probe.statusMessage ?? "(no reason reported)"}`,
  );
}

async function browserAttestation(peer) {
  const values = await peer.page.evaluate(async () => ({
    crossOriginIsolated: globalThis.crossOriginIsolated,
    sharedArrayBufferType: typeof globalThis.SharedArrayBuffer,
    workerConstructions: globalThis.__FORTRESS_WORKER_CONSTRUCTIONS,
    serviceWorkerRegistrations:
      "serviceWorker" in navigator
        ? (await navigator.serviceWorker.getRegistrations()).length
        : 0,
    resultIdentity: globalThis.__FORTRESS_RESULT?.instance_nonce,
  }));
  return { ...values, browserName, browserPid: peer.pid, browserArtifact, browserArtifactSha256 };
}

function validateIdentityAndRuntime(creatorReport, joinerReport, creatorBrowser, joinerBrowser, roomCode) {
  for (const [name, report, browser] of [
    ["creator", creatorReport, creatorBrowser],
    ["joiner", joinerReport, joinerBrowser],
  ]) {
    assertExactKeys(report, reportKeys, `${name} report`);
    assert(report.schema_version === 2 && report.status === "complete", `${name}: incomplete schema`);
    assert(report.origin === "rust-gdextension", `${name}: report did not originate in Rust`);
    assert(report.runtime_error === null, `${name}: ${report.runtime_error}`);
    assert(report.role === name, `${name}: role mismatch`);
    assert(report.room_code === roomCode, `${name}: room mismatch`);
    assert(report.build_sha === buildSha, `${name}: current-checkout identity mismatch`);
    assert(report.signal_fish_client_version === "0.9.0", `${name}: client version drift`);
    assert(report.signal_fish_client_godot_version === "0.9.0", `${name}: Godot adapter version drift`);
    assert(report.fortress_rollback_version === "0.10.0", `${name}: Fortress version drift`);
    assert(report.godot_rust_version === "0.4.5", `${name}: godot-rust version drift`);
    assertExactKeys(
      report.godot_runtime,
      ["major", "minor", "patch", "status", "build", "hash", "string"],
      `${name} Godot runtime identity`,
    );
    assert(
      report.godot_runtime.major === 4 &&
        report.godot_runtime.minor === 5 &&
        report.godot_runtime.patch === 0 &&
        report.godot_runtime.status === "stable" &&
        report.godot_runtime.string === "4.5-stable (official)" &&
        report.godot_runtime.build === "official" &&
        typeof report.godot_runtime.hash === "string" &&
        report.godot_runtime.hash.length > 0,
      `${name}: Godot runtime identity drift`,
    );
    assert(report.target === "wasm32-unknown-emscripten" && report.target_os === "emscripten", `${name}: native fallback detected`);
    assert(report.godot_threads === false && report.worker_count === 0, `${name}: Rust thread/worker claim failed`);
    assert(report.callback_count === report.poll_count, `${name}: poll count differs from genuine Rust callbacks`);
    assertExactKeys(
      report.callback_intervals,
      ["samples", "min_us", "max_us", "mean_us", "p95_us", "p99_us"],
      `${name} callback intervals`,
    );
    assert(JSON.stringify(report.acceptance_thresholds) === JSON.stringify(expectedThresholds), `${name}: shared acceptance thresholds drifted`);
    assert(report.browser_process_id === browser.browserPid, `${name}: browser PID binding mismatch`);
    assert(browser.browserName === browserName, `${name}: browser name binding mismatch`);
    assert(report.instance_nonce === browser.resultIdentity, `${name}: page/report identity mismatch`);
    assert(report.browser_artifact === `${browserArtifact}:${browserArtifactSha256}`, `${name}: browser artifact mismatch`);
    assert(browser.crossOriginIsolated === false, `${name}: page unexpectedly cross-origin isolated`);
    assert(browser.sharedArrayBufferType === "undefined", `${name}: SharedArrayBuffer is exposed`);
    assert(browser.workerConstructions === 0 && browser.serviceWorkerRegistrations === 0, `${name}: worker use detected`);
  }
  assert(creatorReport.run_mode === (mode === "released" ? "healthy" : "negative_one_admission_per_callback"), "creator run-mode mismatch");
  assert(joinerReport.run_mode === creatorReport.run_mode, "peer run-mode mismatch");
  assert(creatorReport.instance_nonce !== joinerReport.instance_nonce, "duplicate instance nonces");
  assert(creatorReport.player_id !== joinerReport.player_id, "duplicate Signal Fish player ids");
  assert(creatorReport.browser_process_id !== joinerReport.browser_process_id, "peers share browser process");
  assert(creatorReport.expected_remote_nonce === joinerReport.instance_nonce, "creator expected-remote nonce mismatch");
  assert(joinerReport.expected_remote_nonce === creatorReport.instance_nonce, "joiner expected-remote nonce mismatch");
  assert(creatorReport.remote_player_id === joinerReport.player_id, "creator remote player mismatch");
  assert(joinerReport.remote_player_id === creatorReport.player_id, "joiner remote player mismatch");
  assert(creatorReport.relay_sent_sequence_count === joinerReport.relay_received_sequence_count, "creator->joiner sequence count mismatch");
  assert(creatorReport.relay_sent_first_sequence === joinerReport.relay_received_first_sequence, "creator->joiner first sequence mismatch");
  assert(creatorReport.relay_sent_last_sequence === joinerReport.relay_received_last_sequence, "creator->joiner last sequence mismatch");
  assert(creatorReport.relay_sent_sequence_hash === joinerReport.relay_received_sequence_hash, "creator->joiner sequence ledger mismatch");
  assert(joinerReport.relay_sent_sequence_count === creatorReport.relay_received_sequence_count, "joiner->creator sequence count mismatch");
  assert(joinerReport.relay_sent_first_sequence === creatorReport.relay_received_first_sequence, "joiner->creator first sequence mismatch");
  assert(joinerReport.relay_sent_last_sequence === creatorReport.relay_received_last_sequence, "joiner->creator last sequence mismatch");
  assert(joinerReport.relay_sent_sequence_hash === creatorReport.relay_received_sequence_hash, "joiner->creator sequence ledger mismatch");
  for (const peer of [creator, joiner]) {
    assert(peer.errors.length === 0, `${peer.role}: browser errors:\n${peer.errors.join("\n")}`);
    assert(peer.logs.filter((line) => line.includes("FORTRESS_WASM_RESULT ")).length === 1, `${peer.role}: expected exactly one report log`);
  }
}

function healthViolations(name, report) {
  const failures = [];
  const check = (condition, description) => {
    if (!condition) failures.push(`${name}: ${description}`);
  };
  check(report.current_frame >= 600, `current_frame=${report.current_frame}`);
  check(report.confirmed_frame >= 600, `confirmed_frame=${report.confirmed_frame}`);
  check(report.game_frame >= 600 && report.frames_advanced >= 600, "insufficient Fortress advancement");
  check(report.active_callback_count >= 600, `active callbacks=${report.active_callback_count}`);
  check(report.callback_intervals.samples >= 599, "missing callback interval samples");
  check(report.callback_intervals.mean_us >= 8_000 && report.callback_intervals.mean_us <= 35_000, `non-nominal callback mean=${report.callback_intervals.mean_us}us`);
  check(report.callback_intervals.p99_us <= 100_000, `callback p99=${report.callback_intervals.p99_us}us`);
  check(report.client_game_data_sent_during_run >= 1_200, "fewer than 1200 completed client sends");
  check(report.client_game_data_received >= 1_200, "fewer than 1200 received client messages");
  check(report.relay_frames_enqueued_during_run >= 1_200, "fewer than 1200 admitted Fortress messages");
  check(report.relay_frames_received >= 1_200, "fewer than 1200 received Fortress messages");
  check(report.relay_frames_enqueued === report.client_game_data_sent, "send conservation failed");
  check(report.relay_frames_received === report.client_game_data_received, "receive conservation failed");
  check(report.final_pipeline_queue_depth === 0, `final pipeline depth=${report.final_pipeline_queue_depth}`);
  check(report.peak_pipeline_queue_depth <= 64, `peak pipeline depth=${report.peak_pipeline_queue_depth}`);
  check(report.peak_oldest_queue_age_us <= 500_000, `oldest queue age=${report.peak_oldest_queue_age_us}us`);
  const forbidden = ["relay_malformed", "relay_wrong_destination", "relay_unknown_sender", "relay_outbound_overflow", "relay_inbound_overflow", "relay_encode_failures", "relay_completion_underflow", "client_messages_undecodable", "checksums_mismatched", "events_discarded_total", "stall_count", "wait_recommendations"];
  for (const field of forbidden) check(report[field] === 0, `${field}=${report[field]}`);
  check(report.relay_send_retries <= 8, `relay_send_retries=${report.relay_send_retries}`);
  check(report.confirmation_lag_current <= 8 && report.confirmation_lag_max <= 8, "confirmation lag exceeded eight frames");
  check(report.max_rollback_depth <= 8, `rollback depth=${report.max_rollback_depth}`);
  check(report.checksums_compared >= 8 && report.checksums_matched === report.checksums_compared, "checksum agreement gate failed");
  check(report.game_checksum !== 0, "zero game checksum");
  check(report.running_elapsed_ms >= 9_000 && report.running_elapsed_ms <= 20_000, `active wall time=${report.running_elapsed_ms}ms`);
  const rate = (report.client_game_data_sent_during_run * 1_000) / report.running_elapsed_ms;
  check(rate >= 120, `completed rate=${rate.toFixed(1)} messages/s`);
  check(report.client_game_data_sent_during_run > report.active_callback_count * 2, "no more than two sends per callback");
  return failures;
}

async function waitForGlobal(peer, key, timeout) {
  try {
    await peer.page.waitForFunction((name) => globalThis[name] !== undefined, key, { timeout });
  } catch (error) {
    // A bare Playwright timeout hides why the page never got there (a missing
    // browser feature, a WASM trap, a WebSocket close). Surface whatever the
    // page reported so the CI log alone explains the failure.
    const observed = peer.errors.length > 0 ? peer.errors.join("\n") : "(none captured)";
    throw new Error(
      `${peer.role}: ${key} never appeared within ${timeout}ms; page errors:\n${observed}`,
      { cause: error },
    );
  }
  return peer.page.evaluate((name) => globalThis[name], key);
}

async function persistPeerArtifacts(peer, knownReport) {
  if (!peer) return;
  safeWriteArtifact(`${peer.role}-browser.log`, `${peer.logs.join("\n")}\n`);
  safeWriteArtifact(`${peer.role}-browser-errors.log`, `${peer.errors.join("\n")}\n`);

  let pageSnapshot = null;
  if (peer.page && !peer.page.isClosed()) {
    let snapshotTimer;
    try {
      const snapshotPromise = peer.page.evaluate(() => ({
        url: globalThis.location.href,
        readyState: globalThis.document.readyState,
        roomReady: globalThis.__FORTRESS_ROOM_READY,
        result: globalThis.__FORTRESS_RESULT,
      }));
      const snapshotTimeout = new Promise((_, reject) => {
        snapshotTimer = setTimeout(
          () => reject(new Error(`${peer.role}: page snapshot timed out`)),
          1_000,
        );
      });
      pageSnapshot = await Promise.race([snapshotPromise, snapshotTimeout]);
    } catch (error) {
      peer.errors.push(`artifact snapshot: ${error.stack ?? error}`);
    } finally {
      clearTimeout(snapshotTimer);
    }
  }
  const partialReport = knownReport ?? pageSnapshot?.result;
  safeWriteArtifact(`${peer.role}-browser-errors.log`, `${peer.errors.join("\n")}\n`);
  safeWriteArtifact(
    `${peer.role}-browser-diagnostics.json`,
    `${JSON.stringify({ role: peer.role, browserPid: peer.pid, page: pageSnapshot }, null, 2)}\n`,
  );
  if (partialReport !== undefined && partialReport !== null) {
    safeWriteArtifact(
      `${peer.role}-partial-report.json`,
      `${JSON.stringify(partialReport, null, 2)}\n`,
    );
  }
}

function safeWriteArtifact(name, contents) {
  try {
    writeFileSync(join(artifactDirectory, name), contents);
  } catch (error) {
    process.stderr.write(`failed to persist ${name}: ${error.stack ?? error}\n`);
  }
}

async function closePeer(peer) {
  if (!peer) return;
  await peer.context?.close().catch(() => {});
  await peer.browser?.close().catch(() => {});
  await peer.server?.close().catch(() => {});
}

function cleanServerEnvironment(port) {
  const environment = {};
  for (const [key, value] of Object.entries(process.env)) {
    if (!key.startsWith("SIGNAL_FISH")) environment[key] = value;
  }
  return {
    ...environment,
    SIGNAL_FISH__PORT: String(port),
    SIGNAL_FISH__LOGGING__LEVEL: "warn",
    SIGNAL_FISH__LOGGING__ENABLE_FILE_LOGGING: "false",
    SIGNAL_FISH__TURN__ENABLED: "false",
    SIGNAL_FISH__SECURITY__REQUIRE_METRICS_AUTH: "false",
    SIGNAL_FISH__SECURITY__REQUIRE_WEBSOCKET_AUTH: "false",
    SIGNAL_FISH__PROTOCOL__SDK_COMPATIBILITY__ENFORCE: "false",
  };
}

function freePort() {
  return new Promise((accept, reject) => {
    const socket = net.createServer();
    socket.once("error", reject);
    socket.listen(0, "127.0.0.1", () => {
      const address = socket.address();
      const port = typeof address === "object" && address ? address.port : 0;
      socket.close((error) => (error ? reject(error) : accept(port)));
    });
  });
}

async function waitForTcp(port, timeout) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const connected = await new Promise((accept) => {
      const socket = net.connect({ host: "127.0.0.1", port });
      socket.once("connect", () => {
        socket.destroy();
        accept(true);
      });
      socket.once("error", () => accept(false));
    });
    if (connected) return;
    await new Promise((accept) => setTimeout(accept, 25));
  }
  throw new Error(`server did not bind 127.0.0.1:${port}`);
}

function contentType(path) {
  if (path.endsWith(".html")) return "text/html; charset=utf-8";
  if (path.endsWith(".js")) return "text/javascript; charset=utf-8";
  if (path.endsWith(".wasm")) return "application/wasm";
  if (path.endsWith(".pck")) return "application/octet-stream";
  return "application/octet-stream";
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function assertExactKeys(value, expected, label) {
  assert(value && typeof value === "object" && !Array.isArray(value), `${label}: expected object`);
  const actual = Object.keys(value).sort();
  const wanted = [...expected].sort();
  assert(JSON.stringify(actual) === JSON.stringify(wanted), `${label}: schema keys differ\nactual=${actual}\nexpected=${wanted}`);
}
