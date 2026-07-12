import { createReadStream } from "node:fs";
import { stat } from "node:fs/promises";
import { createServer } from "node:http";
import { extname, join, normalize, resolve } from "node:path";
import { chromium } from "playwright";

const [exportDirectoryArgument, modeArgument] = process.argv.slice(2);
if (!exportDirectoryArgument || !modeArgument) {
  throw new Error(
    "usage: node scripts/run-godot-web-smoke.mjs <export-directory> <server-pid|--expect-raw-emscripten-link-failure>",
  );
}

const exportDirectory = resolve(exportDirectoryArgument);
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

const httpServer = createServer(async (request, response) => {
  try {
    const requestUrl = new URL(request.url ?? "/", "http://127.0.0.1");
    const relativePath = requestUrl.pathname === "/" ? "index.html" : requestUrl.pathname.slice(1);
    const normalizedPath = normalize(relativePath);
    const filePath = resolve(join(exportDirectory, normalizedPath));
    if (!filePath.startsWith(`${exportDirectory}/`)) {
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
    createReadStream(filePath).pipe(response);
  } catch (error) {
    response.writeHead(404).end(`not found: ${error}`);
  }
});

await new Promise((resolveListening) => httpServer.listen(0, "127.0.0.1", resolveListening));
const address = httpServer.address();
if (!address || typeof address === "string") {
  throw new Error("failed to determine smoke-test HTTP address");
}

const browser = await chromium.launch({ headless: true });
const page = await browser.newPage();
const consoleLines = [];
const pageErrors = [];
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
    if (consoleLines.some((line) => line.includes(marker))) {
      return;
    }
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 50));
  }
  throw new Error(`timed out waiting for browser marker: ${marker}`);
}

try {
  await page.goto(`http://127.0.0.1:${address.port}/index.html`, {
    waitUntil: "domcontentloaded",
    timeout: 30_000,
  });
  if (expectRawEmscriptenLinkFailure) {
    await waitForMarker("undefined symbol 'emscripten_websocket_new'");
    process.stdout.write("SIGNAL_FISH_SMOKE raw-emscripten-rejected\n");
  } else {
    for (const marker of [
      "SIGNAL_FISH_SMOKE json-pong-ok",
      "SIGNAL_FISH_SMOKE binary-pong-ok",
      "SIGNAL_FISH_SMOKE text-relay-ok",
      "SIGNAL_FISH_SMOKE binary-relay-ok",
      "SIGNAL_FISH_SMOKE json-shutdown-ok",
      "SIGNAL_FISH_SMOKE binary-ready-for-server-close",
    ]) {
      await waitForMarker(marker);
    }

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
} finally {
  await browser.close();
  await new Promise((resolveClose) => httpServer.close(resolveClose));
}
