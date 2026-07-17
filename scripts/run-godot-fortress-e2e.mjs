import { createReadStream } from "node:fs";
import { realpath, stat, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { extname, isAbsolute, join, normalize, relative, resolve, sep } from "node:path";
import { pipeline } from "node:stream/promises";
import { chromium } from "playwright";
import {
  validateFinalSlope,
  validateFortressPair,
  validateFortressPeer,
  validateServerConservation,
} from "./godot-e2e-validators.mjs";

const [exportDirectoryArgument, ...scenarioArguments] = process.argv.slice(2);
if (!exportDirectoryArgument) {
  throw new Error(
    "usage: node scripts/run-godot-fortress-e2e.mjs <export-directory> " +
    "[--scenario clean|impaired|soak] [--server-url ws://host:port/v2/ws]",
  );
}
function option(name, fallback) {
  const index = scenarioArguments.indexOf(name);
  return index >= 0 ? scenarioArguments[index + 1] : fallback;
}
const scenario = option("--scenario", "clean");
if (!["clean", "impaired", "soak"].includes(scenario)) {
  throw new Error(`unsupported Fortress scenario: ${scenario}`);
}
const serverUrl = option("--server-url", "ws://127.0.0.1:3536/v2/ws");
const serverAddress = new URL(serverUrl);
const metricsUrl = `http://${serverAddress.host}/metrics/prom`;
const targetFrames = scenario === "soak" ? 3_600 : 600;
const lagLimit = scenario === "clean" ? 8 : 12;
const requireHitch = scenario !== "clean";
const sessionTimeoutMs = scenario === "soak" ? 240_000 : scenario === "clean" ? 40_000 : 60_000;
const exportDirectory = await realpath(resolve(exportDirectoryArgument));
const mimeTypes = new Map([
  [".html", "text/html; charset=utf-8"],
  [".js", "text/javascript; charset=utf-8"],
  [".wasm", "application/wasm"],
  [".pck", "application/octet-stream"],
]);

function isWithinDirectory(directory, candidate) {
  const relativePath = relative(directory, candidate);
  return relativePath === "" ||
    (relativePath !== ".." && !relativePath.startsWith(`..${sep}`) && !isAbsolute(relativePath));
}

const httpServer = createServer(async (request, response) => {
  try {
    const requestUrl = new URL(request.url ?? "/", "http://127.0.0.1");
    const relativePath = requestUrl.pathname === "/"
      ? "index.html"
      : decodeURIComponent(requestUrl.pathname.slice(1));
    const requestedPath = resolve(join(exportDirectory, normalize(relativePath)));
    if (!isWithinDirectory(exportDirectory, requestedPath)) {
      response.writeHead(403).end("forbidden");
      return;
    }
    const filePath = await realpath(requestedPath);
    const fileStat = await stat(filePath);
    if (!isWithinDirectory(exportDirectory, filePath) || !fileStat.isFile()) {
      response.writeHead(404).end("not found");
      return;
    }
    response.setHeader("Content-Type", mimeTypes.get(extname(filePath)) ?? "application/octet-stream");
    response.setHeader("Cross-Origin-Opener-Policy", "same-origin");
    response.setHeader("Cross-Origin-Embedder-Policy", "require-corp");
    response.writeHead(200);
    await pipeline(createReadStream(filePath), response);
  } catch (error) {
    if (!response.headersSent) response.writeHead(404).end(`not found: ${error}`);
    else response.destroy(error instanceof Error ? error : undefined);
  }
});

await new Promise((resolveListening) => httpServer.listen(0, "127.0.0.1", resolveListening));
const address = httpServer.address();
if (!address || typeof address === "string") throw new Error("failed to determine HTTP address");

const peers = [];
let metricsBefore = "";
let metricsAfter = "";
let serverMetricDeltas = {};
let roomCode;
let runError;
let passed = false;

async function fetchMetrics() {
  const response = await fetch(metricsUrl, {
    signal: AbortSignal.timeout(5_000),
  });
  if (!response.ok) throw new Error(`metrics request failed with HTTP ${response.status}`);
  return response.text();
}

function parseMetricTotals(text) {
  const totals = new Map();
  for (const line of text.split("\n")) {
    if (!line || line.startsWith("#")) continue;
    const match = /^([a-zA-Z_:][a-zA-Z0-9_:]*)(?:\{[^}]*\})?\s+([^\s]+)$/.exec(line);
    if (!match) continue;
    const value = Number.parseFloat(match[2]);
    if (Number.isFinite(value)) totals.set(match[1], (totals.get(match[1]) ?? 0) + value);
  }
  return totals;
}

function metricDeltas(beforeText, afterText) {
  const before = parseMetricTotals(beforeText);
  const after = parseMetricTotals(afterText);
  return Object.fromEntries(
    [...after].map(([name, value]) => [name, value - (before.get(name) ?? 0)]),
  );
}

function metricSeriesTotal(text, metricName, requiredLabels = []) {
  let total = 0;
  for (const line of text.split("\n")) {
    if (!line.startsWith(`${metricName}{`) && !line.startsWith(`${metricName} `)) continue;
    if (!requiredLabels.every((label) => line.includes(label))) continue;
    const value = Number.parseFloat(line.trim().split(/\s+/).at(-1));
    if (Number.isFinite(value)) total += value;
  }
  return total;
}

function metricSeriesDelta(metricName, requiredLabels = []) {
  return metricSeriesTotal(metricsAfter, metricName, requiredLabels) -
    metricSeriesTotal(metricsBefore, metricName, requiredLabels);
}

function hasMetricSample(text, metricName) {
  return text.split("\n").some(
    (line) => line.startsWith(`${metricName}{`) || line.startsWith(`${metricName} `),
  );
}

async function launchPeer(role, requestedRoomCode) {
  // Deliberately launch a new Chromium process for every peer. Browser contexts
  // or pages would not exercise the multi-process lifecycle from issue #61.
  const browser = await chromium.launch({ headless: true });
  const peer = {
    role,
    browser,
    page: undefined,
    lines: [],
    console: [],
    pageErrors: [],
    summary: undefined,
    samples: [],
  };
  // Register the browser immediately so a newPage/navigation failure cannot
  // leak its process or erase its partial diagnostics.
  peers.push(peer);
  const page = await browser.newPage();
  peer.page = page;
  page.on("console", (message) => {
    const line = message.text();
    peer.lines.push(line);
    peer.console.push({ type: message.type(), text: line });
    process.stdout.write(`[fortress:${role}:${message.type()}] ${line}\n`);
    const marker = "SIGNAL_FISH_FORTRESS summary ";
    const index = line.indexOf(marker);
    if (index >= 0) {
      try {
        peer.summary = JSON.parse(line.slice(index + marker.length));
      } catch (error) {
        peer.pageErrors.push(`invalid summary JSON: ${error}`);
      }
    }
    const sampleMarker = "SIGNAL_FISH_FORTRESS sample ";
    const sampleIndex = line.indexOf(sampleMarker);
    if (sampleIndex >= 0) {
      try {
        peer.samples.push(JSON.parse(line.slice(sampleIndex + sampleMarker.length)));
      } catch (error) {
        peer.pageErrors.push(`invalid time-series JSON: ${error}`);
      }
    }
  });
  page.on("pageerror", (error) => {
    peer.pageErrors.push(error.message);
    process.stderr.write(`[fortress:${role}:pageerror] ${error.stack ?? error.message}\n`);
  });
  const query = new URLSearchParams({ fortress_role: role });
  if (requestedRoomCode) query.set("room_code", requestedRoomCode);
  query.set("server_url", serverUrl);
  query.set("target_frames", String(targetFrames));
  query.set("session_timeout_ms", String(sessionTimeoutMs));
  if (requireHitch) query.set("poll_hitch_frame", "240");
  await page.goto(`http://127.0.0.1:${address.port}/index.html?${query}`, {
    waitUntil: "domcontentloaded",
    timeout: 30_000,
  });
  return peer;
}

async function waitFor(
  peer,
  predicate,
  description,
  timeoutMilliseconds = scenario === "soak" ? 300_000 : 90_000,
) {
  const deadline = Date.now() + timeoutMilliseconds;
  while (Date.now() < deadline) {
    const value = predicate();
    if (value) return value;
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 50));
  }
  throw new Error(`timed out waiting for ${description} from peer ${peer.role}`);
}

function assertPeerSummary(peer) {
  const summary = peer.summary;
  const validation = validateFortressPeer(summary, {
    targetFrames,
    lagLimit,
    requireHitch,
    sessionTimeoutMs,
  });
  if (!validation.ok) {
    throw new Error(
      `Fortress peer ${peer.role} failed its oracle: ${JSON.stringify({ validation, summary })}`,
    );
  }
  const finalAgeValidation = validateFinalSlope(peer.samples, "queue_age_ms");
  peer.finalAgeSlope = finalAgeValidation.slope;
  if (scenario === "soak" && !finalAgeValidation.ok) {
    throw new Error(
      `Fortress peer ${peer.role} ended the soak with a positive or unavailable queue-age slope: ` +
      JSON.stringify(finalAgeValidation),
    );
  }
  assertPeerDiagnostics(peer);
}

function assertPeerDiagnostics(peer) {
  const consoleErrors = peer.console.filter(({ type }) => type === "error");
  if (
    peer.pageErrors.length > 0 || consoleErrors.length > 0 ||
    peer.lines.some((line) => line.includes("startup-error"))
  ) {
    throw new Error(`Fortress peer ${peer.role} browser errors: ${JSON.stringify({
      pageErrors: peer.pageErrors,
      consoleErrors,
    })}`);
  }
}

try {
  metricsBefore = await fetchMetrics();
  await writeFile("fortress-metrics-before.prom", metricsBefore);

  const first = await launchPeer("a");
  const roomLine = await waitFor(
    first,
    () => first.lines.find((line) => line.includes("SIGNAL_FISH_FORTRESS room-joined ")),
    "room code",
  );
  const roomMatch = /\broom_code=([^\s]+)/.exec(roomLine);
  if (!roomMatch) throw new Error(`could not parse room code from: ${roomLine}`);
  roomCode = roomMatch[1];
  const second = await launchPeer("b", roomCode);
  await Promise.all([
    waitFor(first, () => first.summary, "summary"),
    waitFor(second, () => second.summary, "summary"),
  ]);

  for (const peer of peers) assertPeerSummary(peer);
  if (
    first.summary.joined_room_code !== roomCode || second.summary.joined_room_code !== roomCode ||
    second.summary.requested_room_code !== roomCode || first.summary.requested_room_code !== null ||
    first.summary.local_id !== second.summary.remote_id ||
    second.summary.local_id !== first.summary.remote_id
  ) {
    throw new Error(`Fortress room/roster mismatch: ${JSON.stringify(peers.map((peer) => peer.summary))}`);
  }
  const pairValidation = validateFortressPair(first.summary, second.summary);
  if (!pairValidation.ok) {
    throw new Error(
      `Fortress pair oracle failed: ${JSON.stringify({
        pairValidation,
        peers: peers.map((peer) => peer.summary),
      })}`,
    );
  }

  await Promise.all(peers.map((peer) => waitFor(
    peer,
    () => peer.lines.some((line) => line.includes("SIGNAL_FISH_SHELL game-complete")),
    "game completion",
  )));
  for (const peer of peers) assertPeerDiagnostics(peer);

  metricsAfter = await fetchMetrics();
  serverMetricDeltas = metricDeltas(metricsBefore, metricsAfter);
  for (const required of [
    "signal_fish_game_data_messages_total",
    "signal_fish_websocket_messages_dropped_total",
    "signal_fish_websocket_slow_consumer_disconnects_total",
    "signal_fish_websocket_delivery_attempts_total",
    "signal_fish_websocket_deliveries_enqueued_total",
    "signal_fish_websocket_deliveries_channel_closed_total",
    "signal_fish_websocket_deliveries_canceled_total",
    "signal_fish_websocket_delivery_class_outcomes_total",
  ]) {
    if (!hasMetricSample(metricsAfter, required)) {
      throw new Error(`required server metric sample is absent: ${required}`);
    }
  }
  const expectedGameData = first.summary.relay_encoded + second.summary.relay_encoded;
  const gameDataForwarded = serverMetricDeltas.signal_fish_game_data_messages_total ?? 0;
  const reliableAttempted = metricSeriesDelta(
    "signal_fish_websocket_delivery_class_outcomes_total",
    ['class="reliable"', 'outcome="attempted"'],
  );
  const reliableDelivered = metricSeriesDelta(
    "signal_fish_websocket_delivery_class_outcomes_total",
    ['class="reliable"', 'outcome="delivered"'],
  );
  const deliveryAttempts = serverMetricDeltas.signal_fish_websocket_delivery_attempts_total ?? 0;
  const deliveryTerminals =
    (serverMetricDeltas.signal_fish_websocket_deliveries_enqueued_total ?? 0) +
    (serverMetricDeltas.signal_fish_websocket_deliveries_channel_closed_total ?? 0) +
    (serverMetricDeltas.signal_fish_websocket_deliveries_canceled_total ?? 0);
  const harmfulDeltas = Object.entries(serverMetricDeltas).filter(
    ([name, delta]) => delta > 0 && /(drop|slow_consumer.*disconnect)/i.test(name),
  );
  const conservation = validateServerConservation({
    expectedGameData,
    gameDataForwarded,
    reliableAttempted,
    reliableDelivered,
    deliveryAttempts,
    deliveryTerminals,
    harmfulDeltas,
  });
  if (!conservation.ok) {
    throw new Error(`server delivery conservation failed: ${JSON.stringify({
      expectedGameData,
      gameDataForwarded,
      reliableAttempted,
      reliableDelivered,
      deliveryAttempts,
      deliveryTerminals,
      harmfulDeltas,
    })}`);
  }
  passed = true;
  process.stdout.write("SIGNAL_FISH_FORTRESS multiprocess-complete\n");
} catch (error) {
  runError = error instanceof Error ? error : new Error(String(error));
} finally {
  if (!metricsAfter) {
    try {
      metricsAfter = await fetchMetrics();
      serverMetricDeltas = metricDeltas(metricsBefore, metricsAfter);
    } catch (artifactError) {
      process.stderr.write(`Fortress final metrics failed: ${artifactError}\n`);
    }
  }
  const browserResults = await Promise.allSettled(peers.map((peer) => peer.browser.close()));
  for (const result of browserResults) {
    if (result.status === "rejected") {
      process.stderr.write(`Fortress browser cleanup failed: ${result.reason}\n`);
      if (!runError) runError = new Error(`Fortress browser cleanup failed: ${result.reason}`);
    }
  }
  const artifactWrites = [
    writeFile("fortress-metrics-before.prom", metricsBefore),
    writeFile("fortress-metrics-after.prom", metricsAfter),
    writeFile(
      "godot-fortress-summary.json",
      `${JSON.stringify({
        passed: passed && !runError,
        error: runError?.stack ?? runError?.message ?? null,
        roomCode: roomCode ?? null,
        peers: peers.map((peer) => ({
          role: peer.role,
          summary: peer.summary ?? null,
          finalAgeSlope: Number.isFinite(peer.finalAgeSlope) ? peer.finalAgeSlope : null,
          pageErrors: peer.pageErrors,
          consoleTypes: peer.console.map(({ type }) => type),
        })),
        serverMetricDeltas,
        scenario,
        serverUrl,
      }, null, 2)}\n`,
    ),
    writeFile(
      "godot-fortress-timeseries.json",
      `${JSON.stringify(peers.flatMap((peer) => peer.samples), null, 2)}\n`,
    ),
    writeFile(
      "godot-fortress-timeseries.csv",
      `${[
        "role,elapsed_ms,current_frame,confirmed_frame,confirmation_lag,queue_depth,queue_age_ms",
        ...peers.flatMap((peer) => peer.samples.map((sample) => [
          sample.role,
          sample.elapsed_ms,
          sample.current_frame,
          sample.confirmed_frame,
          sample.confirmation_lag,
          sample.queue_depth,
          sample.queue_age_ms,
        ].join(","))),
      ].join("\n")}\n`,
    ),
    ...peers.map((peer) =>
      writeFile(`godot-fortress-${peer.role}.log`, `${peer.lines.join("\n")}\n`)
    ),
  ];
  for (const result of await Promise.allSettled(artifactWrites)) {
    if (result.status === "rejected") {
      process.stderr.write(`Fortress artifact write failed: ${result.reason}\n`);
      if (!runError) runError = new Error(`Fortress artifact write failed: ${result.reason}`);
    }
  }
  await new Promise((resolveClose) => httpServer.close(resolveClose));
}

if (runError) throw runError;
