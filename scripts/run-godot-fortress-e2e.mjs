import { createReadStream } from "node:fs";
import { realpath, stat, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { extname, isAbsolute, join, normalize, relative, resolve, sep } from "node:path";
import { pipeline } from "node:stream/promises";
import { chromium } from "playwright";

const [exportDirectoryArgument] = process.argv.slice(2);
if (!exportDirectoryArgument) {
  throw new Error("usage: node scripts/run-godot-fortress-e2e.mjs <export-directory>");
}
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
  const response = await fetch("http://127.0.0.1:3536/metrics/prom", {
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
  });
  page.on("pageerror", (error) => {
    peer.pageErrors.push(error.message);
    process.stderr.write(`[fortress:${role}:pageerror] ${error.stack ?? error.message}\n`);
  });
  const query = new URLSearchParams({ fortress_role: role });
  if (requestedRoomCode) query.set("room_code", requestedRoomCode);
  await page.goto(`http://127.0.0.1:${address.port}/index.html?${query}`, {
    waitUntil: "domcontentloaded",
    timeout: 30_000,
  });
  return peer;
}

async function waitFor(peer, predicate, description, timeoutMilliseconds = 60_000) {
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
  const decimal = /^\d+$/;
  if (
    !summary?.passed || summary.target_frames !== 600 || summary.settlement_frame_limit !== 620 ||
    summary.session_timeout_ms !== 40_000 ||
    summary.game_frame < 600 || summary.game_frame > 620 || summary.checksum_through !== 600 ||
    !decimal.test(summary.confirmed_input_checksum) || !decimal.test(summary.target_state_checksum) ||
    !summary.game_ready || !summary.sync_in_sync
  ) {
    throw new Error(`Fortress peer ${peer.role} failed its state oracle: ${JSON.stringify(summary)}`);
  }
  if (
    !Number.isFinite(summary.simulation_elapsed_ms) || summary.simulation_elapsed_ms <= 0 ||
    summary.simulation_elapsed_ms >= summary.session_timeout_ms || summary.max_poll_us >= 50_000
  ) {
    throw new Error(`Fortress peer ${peer.role} missed timing bounds: ${JSON.stringify(summary)}`);
  }
  if (
    !summary.multi_frame_poll || summary.peak_queue_depth > 64 || summary.queue_depth !== 0 ||
    summary.relay_inbound_depth !== 0 || summary.relay_outbound_depth !== 0
  ) {
    throw new Error(`Fortress peer ${peer.role} missed queue assertions: ${JSON.stringify(summary)}`);
  }
  if (
    summary.relay_dropped !== 0 || summary.relay_malformed !== 0 ||
    summary.backend_capacity_hits !== 0 || summary.checksums_compared < 10 ||
    summary.checksums_matched !== summary.checksums_compared || summary.checksums_mismatched !== 0 ||
    summary.events_discarded !== 0 || summary.desync_events !== 0 ||
    summary.frames_advanced !== summary.visual_frames + summary.resimulated_frames
  ) {
    throw new Error(`Fortress peer ${peer.role} reported integrity loss: ${JSON.stringify(summary)}`);
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
  if (
    first.summary.target_state_checksum !== second.summary.target_state_checksum ||
    first.summary.confirmed_input_checksum !== second.summary.confirmed_input_checksum
  ) {
    throw new Error(`Fortress convergence mismatch: ${JSON.stringify(peers.map((peer) => peer.summary))}`);
  }
  if (
    !second.summary.impairment_activated || !second.summary.impairment_released ||
    first.summary.pre_impairment_rollback_count !== 0 ||
    first.summary.pre_impairment_resimulated_frames !== 0 ||
    first.summary.pre_impairment_prediction_miss_count !== 0 ||
    first.summary.rollback_count <= first.summary.pre_impairment_rollback_count ||
    first.summary.resimulated_frames <= first.summary.pre_impairment_resimulated_frames ||
    first.summary.prediction_miss_count <= first.summary.pre_impairment_prediction_miss_count ||
    first.summary.max_rollback_depth < 4 || first.summary.load_requests <= 0 ||
    !first.summary.peer_left_observed || first.summary.peer_left_epoch <= 0 ||
    first.summary.peer_left_final_seq <= 0
  ) {
    throw new Error(`Fortress rollback/teardown proof absent: ${JSON.stringify(peers.map((peer) => peer.summary))}`);
  }
  if (
    first.summary.relay_encoded !== second.summary.relay_decoded ||
    second.summary.relay_encoded !== first.summary.relay_decoded
  ) {
    throw new Error(`Fortress relay conservation failed: ${JSON.stringify(peers.map((peer) => peer.summary))}`);
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
  if (
    gameDataForwarded !== expectedGameData || reliableAttempted !== expectedGameData ||
    reliableDelivered !== expectedGameData || deliveryAttempts !== deliveryTerminals ||
    harmfulDeltas.length > 0
  ) {
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
          pageErrors: peer.pageErrors,
          consoleTypes: peer.console.map(({ type }) => type),
        })),
        serverMetricDeltas,
      }, null, 2)}\n`,
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
