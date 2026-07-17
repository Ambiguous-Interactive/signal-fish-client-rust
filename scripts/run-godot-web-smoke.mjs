import { createReadStream } from "node:fs";
import { realpath, stat, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { extname, isAbsolute, join, normalize, relative, resolve, sep } from "node:path";
import { pipeline } from "node:stream/promises";
import { chromium } from "playwright";
import { validateLoadSummary, validateServerConservation } from "./godot-e2e-validators.mjs";

const [exportDirectoryArgument, modeArgument] = process.argv.slice(2);
if (!exportDirectoryArgument || !modeArgument) {
  throw new Error(
    "usage: node scripts/run-godot-web-smoke.mjs <export-directory> <server-pid|--expect-raw-emscripten-link-failure>",
  );
}

const exportDirectory = await realpath(resolve(exportDirectoryArgument));
const expectRawEmscriptenLinkFailure = modeArgument === "--expect-raw-emscripten-link-failure";
const serverPid = expectRawEmscriptenLinkFailure ? undefined : Number.parseInt(modeArgument, 10);
if (!expectRawEmscriptenLinkFailure && (!Number.isSafeInteger(serverPid) || serverPid <= 0)) {
  throw new Error(`invalid server PID: ${modeArgument}`);
}

const mimeTypes = new Map([
  [".html", "text/html; charset=utf-8"],
  [".js", "text/javascript; charset=utf-8"],
  [".wasm", "application/wasm"],
  [".pck", "application/octet-stream"],
  [".png", "image/png"],
]);

function isWithinDirectory(directory, candidate) {
  const relativePath = relative(directory, candidate);
  return (
    relativePath === "" ||
    (relativePath !== ".." && !relativePath.startsWith(`..${sep}`) && !isAbsolute(relativePath))
  );
}

const httpServer = createServer(async (request, response) => {
  try {
    const requestUrl = new URL(request.url ?? "/", "http://127.0.0.1");
    const relativePath =
      requestUrl.pathname === "/" ? "index.html" : decodeURIComponent(requestUrl.pathname.slice(1));
    const normalizedPath = normalize(relativePath);
    const requestedPath = resolve(join(exportDirectory, normalizedPath));
    if (!isWithinDirectory(exportDirectory, requestedPath)) {
      response.writeHead(403).end("forbidden");
      return;
    }
    const filePath = await realpath(requestedPath);
    if (!isWithinDirectory(exportDirectory, filePath)) {
      response.writeHead(403).end("forbidden");
      return;
    }
    const fileStat = await stat(filePath);
    if (!fileStat.isFile()) {
      response.writeHead(404).end("not found");
      return;
    }
    response.setHeader("Content-Type", mimeTypes.get(extname(filePath)) ?? "application/octet-stream");
    response.setHeader("Cross-Origin-Opener-Policy", "same-origin");
    response.setHeader("Cross-Origin-Embedder-Policy", "require-corp");
    response.writeHead(200);
    await pipeline(createReadStream(filePath), response);
  } catch (error) {
    if (!response.headersSent) {
      response.writeHead(404).end(`not found: ${error}`);
    } else {
      response.destroy(error instanceof Error ? error : undefined);
    }
  }
});

await new Promise((resolveListening) => httpServer.listen(0, "127.0.0.1", resolveListening));
const address = httpServer.address();
if (!address || typeof address === "string") {
  throw new Error("failed to determine smoke-test HTTP address");
}

let browser;
let page;
try {
  browser = await chromium.launch({ headless: true });
  page = await browser.newPage();
} catch (error) {
  try {
    if (browser) await browser.close();
  } finally {
    await new Promise((resolveClose) => httpServer.close(resolveClose));
  }
  throw error;
}
const consoleLines = [];
const pageErrors = [];
let metricsBefore = "";
let metricsAfter = "";
let loadSummary;
page.on("console", (message) => {
  const line = message.text();
  consoleLines.push(line);
  process.stdout.write(`[browser:${message.type()}] ${line}\n`);
});
page.on("pageerror", (error) => {
  pageErrors.push(error.message);
  process.stderr.write(`[browser:pageerror] ${error.stack ?? error.message}\n`);
});

async function waitForMarker(marker, timeoutMilliseconds = 30_000) {
  const deadline = Date.now() + timeoutMilliseconds;
  while (Date.now() < deadline) {
    if (consoleLines.some((line) => {
      const index = line.indexOf(marker);
      if (index < 0) return false;
      const following = line.at(index + marker.length);
      return following === undefined || /\s/.test(following);
    })) {
      return;
    }
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 50));
  }
  throw new Error(`timed out waiting for browser marker: ${marker}`);
}

async function fetchMetrics() {
  const response = await fetch("http://127.0.0.1:3536/metrics/prom", {
    signal: AbortSignal.timeout(5_000),
  });
  if (!response.ok) {
    throw new Error(`metrics request failed with HTTP ${response.status}`);
  }
  return response.text();
}

function parseMetricTotals(text) {
  const totals = new Map();
  for (const line of text.split("\n")) {
    if (!line || line.startsWith("#")) continue;
    const match = /^([a-zA-Z_:][a-zA-Z0-9_:]*)(?:\{[^}]*\})?\s+([^\s]+)$/.exec(line);
    if (!match) continue;
    const value = Number.parseFloat(match[2]);
    if (Number.isFinite(value)) {
      totals.set(match[1], (totals.get(match[1]) ?? 0) + value);
    }
  }
  return totals;
}

function metricDeltas(beforeText, afterText) {
  const before = parseMetricTotals(beforeText);
  const after = parseMetricTotals(afterText);
  return Object.fromEntries(
    [...after.entries()].map(([name, value]) => [name, value - (before.get(name) ?? 0)]),
  );
}

function metricSeriesTotal(text, metricName, requiredLabels = []) {
  let total = 0;
  for (const line of text.split("\n")) {
    if (!line.startsWith(metricName)) continue;
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

function loadSamples() {
  return consoleLines
    .filter((line) => line.includes("SIGNAL_FISH_LOAD sample "))
    .map((line) => JSON.parse(line.slice(line.indexOf("SIGNAL_FISH_LOAD sample ") + 24)));
}

async function writeThroughputArtifacts() {
  const samples = loadSamples();
  const deltas = metricDeltas(metricsBefore, metricsAfter);
  const report = { loadSummary, samples, serverMetricDeltas: deltas };
  await writeFile("godot-throughput.json", `${JSON.stringify(report, null, 2)}\n`);
  const columns = [
    "elapsed_ms", "command_depth", "peak_depth", "current_queue_age_ms",
    "peak_queue_age_ms", "buffered_bytes",
    "accepted_frames", "received_frames", "accepted_per_second",
    "received_per_second", "offered_frames", "poll_max_us", "poll_work_frames",
    "poll_work_bytes", "poll_receive_frames", "poll_count",
    "send_budget_exhaustions", "receive_budget_exhaustions", "latest_latency_us",
  ];
  const csv = [columns.join(","), ...samples.map((sample) =>
    columns.map((column) => sample[column] ?? "").join(","))].join("\n");
  await writeFile("godot-throughput.csv", `${csv}\n`);
  await writeFile("metrics-before.prom", metricsBefore);
  await writeFile("metrics-after.prom", metricsAfter);
}

let runError;
try {
  if (!expectRawEmscriptenLinkFailure) {
    metricsBefore = await fetchMetrics();
  }
  await page.goto(`http://127.0.0.1:${address.port}/index.html`, {
    waitUntil: "domcontentloaded",
    timeout: 30_000,
  });
  if (expectRawEmscriptenLinkFailure) {
    await waitForMarker("undefined symbol 'emscripten_websocket_new'.");
    process.stdout.write("SIGNAL_FISH_SMOKE raw-emscripten-rejected\n");
  } else {
    for (const marker of [
      "SIGNAL_FISH_SMOKE json-pong-ok",
      "SIGNAL_FISH_SMOKE binary-pong-ok",
      "SIGNAL_FISH_SMOKE text-relay-ok",
      "SIGNAL_FISH_SMOKE binary-relay-ok",
      "SIGNAL_FISH_SMOKE binary-four-one-poll",
      "SIGNAL_FISH_SMOKE load-summary",
      "SIGNAL_FISH_SMOKE binary-ready-for-server-close",
    ]) {
      await waitForMarker(marker);
    }

    const summaryLine = consoleLines.find((line) => line.includes("SIGNAL_FISH_SMOKE load-summary "));
    loadSummary = JSON.parse(summaryLine.slice(summaryLine.indexOf("SIGNAL_FISH_SMOKE load-summary ") + 31));
    const samples = loadSamples();
    const loadValidation = validateLoadSummary(loadSummary, samples);
    loadSummary.queue_depth_slope_per_ms = loadValidation.depthSlope;
    loadSummary.queue_age_slope_ms_per_ms = loadValidation.ageSlope;
    if (!loadValidation.ok) {
      throw new Error(`Godot throughput assertions failed: ${JSON.stringify(loadValidation)}`);
    }
    metricsAfter = await fetchMetrics();
    const deltas = metricDeltas(metricsBefore, metricsAfter);
    for (const metric of [
      "signal_fish_websocket_messages_dropped_total",
      "signal_fish_websocket_slow_consumer_disconnects_total",
      "signal_fish_websocket_delivery_attempts_total",
      "signal_fish_websocket_deliveries_enqueued_total",
      "signal_fish_websocket_deliveries_channel_closed_total",
      "signal_fish_websocket_deliveries_canceled_total",
    ]) {
      if (!metricsAfter.includes(metric)) {
        throw new Error(`required server metric is absent: ${metric}`);
      }
    }
    if (
      (deltas.signal_fish_websocket_messages_dropped_total ?? 0) !== 0 ||
      (deltas.signal_fish_websocket_slow_consumer_disconnects_total ?? 0) !== 0
    ) {
      throw new Error(`server reported drops/slow consumers: ${JSON.stringify(deltas)}`);
    }
    const harmfulDeltas = Object.entries(deltas).filter(
      ([name, delta]) => delta > 0 && /(drop|slow_consumer.*disconnect)/i.test(name),
    );
    if (harmfulDeltas.length > 0) {
      throw new Error(`server reported drops/slow consumers: ${JSON.stringify(harmfulDeltas)}`);
    }
    const expectedGameData = loadSummary.offered_per_client.reduce((sum, value) => sum + value, 0) + 5;
    const gameDataForwarded = deltas.signal_fish_game_data_messages_total ?? 0;
    const reliableAttempted = metricSeriesDelta(
      "signal_fish_websocket_delivery_class_outcomes_total",
      ['class="reliable"', 'outcome="attempted"'],
    );
    const reliableDelivered = metricSeriesDelta(
      "signal_fish_websocket_delivery_class_outcomes_total",
      ['class="reliable"', 'outcome="delivered"'],
    );
    const deliveryAttempts = deltas.signal_fish_websocket_delivery_attempts_total ?? 0;
    const deliveryTerminals =
      (deltas.signal_fish_websocket_deliveries_enqueued_total ?? 0) +
      (deltas.signal_fish_websocket_deliveries_channel_closed_total ?? 0) +
      (deltas.signal_fish_websocket_deliveries_canceled_total ?? 0);
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

    await waitForMarker("SIGNAL_FISH_SMOKE json-shutdown-ok");
    process.kill(serverPid, "SIGTERM");
    await waitForMarker("SIGNAL_FISH_SMOKE close-attribution-ok");
    await waitForMarker("SIGNAL_FISH_SMOKE complete");

    const failures = consoleLines.filter(
      (line) => line.includes("-error") || line.includes("unexpected-disconnect"),
    );
    if (pageErrors.length > 0 || failures.length > 0) {
      throw new Error(
        `browser reported failures: ${JSON.stringify({ pageErrors, failures })}`,
      );
    }
  }
} catch (error) {
  runError = error;
  throw error;
} finally {
  try {
    if (!expectRawEmscriptenLinkFailure) {
      if (!metricsAfter) {
        try {
          metricsAfter = await fetchMetrics();
        } catch (metricsError) {
          process.stderr.write(`failed to capture final metrics: ${metricsError}\n`);
        }
      }
      try {
        await writeThroughputArtifacts();
      } catch (artifactError) {
        if (!runError) throw artifactError;
        process.stderr.write(`failed to write throughput artifacts: ${artifactError}\n`);
      }
    }
  } finally {
    try {
      await browser.close();
    } finally {
      await new Promise((resolveClose) => httpServer.close(resolveClose));
    }
  }
}
