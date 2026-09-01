"use strict";
/*
 * On-target driver for the Emscripten WebSocket transport harness
 * (issue #194). Runs the wasm32-unknown-emscripten harness binary under
 * Node.js against a loopback WebSocket server.
 *
 * Usage: node run-harness.cjs <path-to-generated-harness.js>
 *
 * The driver owns three pieces:
 *   1. A minimal dependency-free RFC 6455 server used as the loopback peer.
 *   2. A browser-faithful WebSocket client polyfill installed as
 *      `globalThis.WebSocket` before the Emscripten module loads (Node 18
 *      has no global WebSocket; Emscripten's shim requires one).
 *   3. The scenario scheduler: it begins each scenario through the module's
 *      exported `sfh_begin`, then pumps one `sfh_step` per ~1ms timer tick
 *      so the JavaScript event loop can deliver browser events between
 *      steps, at a cadence resembling a polling game loop.
 *
 * Scenario scripts run server-side, keyed by the same mode value the driver
 * passes to `sfh_begin`:
 *   0 roundtrip: echo every data frame verbatim.
 *   1 send-after-close: close immediately after the client opens with
 *     code 4000 reason "draining".
 *   2 ledger-bound: flood six 50-byte text frames as soon as the client
 *     opens (the transport bound admits two, fuses on the third).
 *   3 abrupt-error: destroy the TCP socket right after the client opens so
 *     the browser reports onerror + onclose(1006, unclean).
 */

const crypto = require("node:crypto");
const net = require("node:net");
const path = require("node:path");

const WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const HARNESS_URL_PATH = "/harness";
const SCENARIO_COUNT = 4;
const DEADLINE_MS = 30_000;

function fail(message) {
  process.stderr.write(`harness FAIL: ${message}\n`);
  process.exit(1);
}

const DEBUG = process.env.SFH_DEBUG === "1";
function debug(message) {
  if (DEBUG) process.stderr.write(`[debug] ${message}\n`);
}

// ── WebSocket frame codec ───────────────────────────────────────────────────

const OP_TEXT = 0x1;
const OP_BINARY = 0x2;
const OP_CLOSE = 0x8;
const OP_PING = 0x9;
const OP_PONG = 0xa;

/** Encode one unmasked server-to-client frame. */
function encodeServerFrame(opcode, payload) {
  const length = payload.length;
  let header;
  if (length < 126) {
    header = Buffer.from([0x80 | opcode, length]);
  } else if (length <= 0xffff) {
    header = Buffer.alloc(4);
    header[0] = 0x80 | opcode;
    header[1] = 126;
    header.writeUInt16BE(length, 2);
  } else {
    header = Buffer.alloc(10);
    header[0] = 0x80 | opcode;
    header[1] = 127;
    header.writeBigUInt64BE(BigInt(length), 2);
  }
  return Buffer.concat([header, payload]);
}

/** Encode one masked client-to-server frame. */
function encodeClientFrame(opcode, payload) {
  const mask = crypto.randomBytes(4);
  const masked = Buffer.allocUnsafe(payload.length);
  for (let i = 0; i < payload.length; ++i) {
    masked[i] = payload[i] ^ mask[i & 3];
  }
  const length = payload.length;
  let header;
  if (length < 126) {
    header = Buffer.from([0x80 | opcode, 0x80 | length]);
  } else if (length <= 0xffff) {
    header = Buffer.alloc(4);
    header[0] = 0x80 | opcode;
    header[1] = 0x80 | 126;
    header.writeUInt16BE(length, 2);
  } else {
    header = Buffer.alloc(10);
    header[0] = 0x80 | opcode;
    header[1] = 0x80 | 127;
    header.writeBigUInt64BE(BigInt(length), 2);
  }
  return Buffer.concat([header, mask, masked]);
}

/**
 * Incremental frame parser. Feed raw chunks; yields complete frames.
 * Supports the single-frame unfragmented messages this harness uses and
 * rejects anything else (fragmentation, RSV bits, unknown opcodes,
 * control frames with a payload over 125 bytes, close frames with a
 * 1-byte body) the way a conformant peer must.
 */
class FrameParser {
  constructor(requireMasked = false) {
    this.buffer = Buffer.alloc(0);
    this.requireMasked = requireMasked;
  }

  /** Returns an array of {opcode, payload} frames completed by `chunk`. */
  push(chunk) {
    this.buffer = Buffer.concat([this.buffer, chunk]);
    const frames = [];
    for (;;) {
      if (this.buffer.length < 2) break;
      const first = this.buffer[0];
      const second = this.buffer[1];
      const fin = (first & 0x80) !== 0;
      const opcode = first & 0x0f;
      const masked = (second & 0x80) !== 0;
      let length = second & 0x7f;
      let offset = 2;
      if (length === 126) {
        if (this.buffer.length < offset + 2) break;
        length = this.buffer.readUInt16BE(offset);
        offset += 2;
      } else if (length === 127) {
        if (this.buffer.length < offset + 8) break;
        const big = this.buffer.readBigUInt64BE(offset);
        if (big > 0xffffffffn) {
          fail(`frame length ${big} exceeds the harness sanity bound`);
        }
        length = Number(big);
        offset += 8;
      }
      const maskKey = masked ? 4 : 0;
      if (this.buffer.length < offset + maskKey + length) break;
      if (!fin || (first & 0x70) !== 0) {
        fail(`harness received a fragmented/RSV frame (opcode ${opcode}); unsupported`);
      }
      if (this.requireMasked && !masked) {
        fail("harness received an unmasked client-to-server frame; RFC 6455 5.1 violation");
      }
      if (opcode !== OP_TEXT && opcode !== OP_BINARY && opcode !== OP_CLOSE
          && opcode !== OP_PING && opcode !== OP_PONG) {
        fail(`harness received unknown opcode ${opcode}`);
      }
      // RFC 6455 5.5: control frames must have a payload of 125 bytes or
      // less, and 5.5.1: a close body of exactly 1 byte is invalid. Both are
      // mandatory-fail protocol errors (1002); the shim must never emit them.
      const isControl = opcode >= OP_CLOSE && opcode <= OP_PONG;
      if (isControl && length > 125) {
        fail(`harness received a control frame (opcode ${opcode}) with a ${length}-byte payload; RFC 6455 5.5 requires failing the connection with 1002`);
      }
      if (opcode === OP_CLOSE && length === 1) {
        fail("harness received a 1-byte close body; RFC 6455 5.5.1 requires failing the connection with 1002");
      }
      let payload = this.buffer.subarray(offset + maskKey, offset + maskKey + length);
      if (masked) {
        const key = this.buffer.subarray(offset, offset + 4);
        const unmasked = Buffer.allocUnsafe(length);
        for (let i = 0; i < length; ++i) {
          unmasked[i] = payload[i] ^ key[i & 3];
        }
        payload = unmasked;
      } else {
        payload = Buffer.from(payload);
      }
      this.buffer = this.buffer.subarray(offset + maskKey + length);
      frames.push({ opcode, payload });
    }
    return frames;
  }
}

// ── Loopback RFC 6455 server ────────────────────────────────────────────────

class LoopbackServer {
  constructor() {
    this.script = null;
    // Optional delay before the 101 response, so scenarios that must
    // observe the CONNECTING window (pre-open send retention) get
    // deterministic scheduling steps instead of racing one event-loop turn.
    this.handshakeDelayMs = 0;
    this.sockets = new Set();
    this.server = net.createServer((socket) => this.onConnection(socket));
  }

  listen() {
    return new Promise((resolve, reject) => {
      this.server.once("error", reject);
      this.server.listen(0, "127.0.0.1", () => {
        resolve(this.server.address().port);
      });
    });
  }

  close() {
    for (const socket of this.sockets) socket.destroy();
    return new Promise((resolve) => this.server.close(resolve));
  }

  onConnection(socket) {
    debug("server: connection accepted");
    this.sockets.add(socket);
    socket.setNoDelay(true);
    let handshake = Buffer.alloc(0);
    let established = false;
    let responding = false;
    let parser = null;
    let peer = null;

    const cleanup = () => {
      this.sockets.delete(socket);
    };
    socket.on("error", () => cleanup());
    socket.on("close", () => cleanup());

    socket.on("data", (chunk) => {
      if (!established && !responding) {
        handshake = Buffer.concat([handshake, chunk]);
        const end = handshake.indexOf("\r\n\r\n");
        if (end === -1) {
          if (handshake.length > 16 * 1024) socket.destroy();
          return;
        }
        responding = true;
        const request = handshake.subarray(0, end).toString("latin1");
        const key = /^sec-websocket-key:\s*(.+)\r?$/im.exec(request);
        if (!/^get \/harness\s+http\/1\.[01]\r?$/i.test(request.split("\r\n")[0]) || key === null) {
          socket.destroy();
          return;
        }
        const accept = crypto
          .createHash("sha1")
          .update(key[1].trim() + WS_GUID)
          .digest("base64");
        const finishHandshake = () => {
          if (this.sockets.has(socket) === false) return;
          socket.write(
            "HTTP/1.1 101 Switching Protocols\r\n" +
              "Upgrade: websocket\r\n" +
              "Connection: Upgrade\r\n" +
              `Sec-WebSocket-Accept: ${accept}\r\n` +
              "\r\n",
          );
          established = true;
          debug("server: handshake complete; attaching script");
          const rest = handshake.subarray(end + 4);
          parser = new FrameParser(true);
          peer = this.attachPeer(socket);
          if (rest.length > 0) this.dispatch(peer, parser.push(rest));
          if (this.script !== null) this.script(peer);
        };
        if (this.handshakeDelayMs > 0) {
          setTimeout(finishHandshake, this.handshakeDelayMs);
        } else {
          finishHandshake();
        }
        return;
      }
      this.dispatch(peer, parser.push(chunk));
    });
  }

  attachPeer(socket) {
    const peer = {
      sendText: (text) => socket.write(encodeServerFrame(OP_TEXT, Buffer.from(text, "utf8"))),
      sendBinary: (bytes) => socket.write(encodeServerFrame(OP_BINARY, bytes)),
      sendClose: (code, reason) => {
        const payload = Buffer.alloc(2 + Buffer.byteLength(reason, "utf8"));
        payload.writeUInt16BE(code, 0);
        payload.write(reason, 2, "utf8");
        socket.write(encodeServerFrame(OP_CLOSE, payload));
      },
      destroy: () => socket.destroy(),
      onmessage: null,
    };
    return peer;
  }

  dispatch(peer, frames) {
    for (const frame of frames) {
      if (frame.opcode === OP_CLOSE) {
        // Echo the close handshake, then drop the TCP connection.
        peer.sendClose(
          frame.payload.length >= 2 ? frame.payload.readUInt16BE(0) : 1000,
          "",
        );
        peer.destroy();
        return;
      }
      if (frame.opcode === OP_PING) {
        continue; // no pings are used by this harness; ignore
      }
      if (frame.opcode === OP_TEXT || frame.opcode === OP_BINARY) {
        debug(`server: data frame opcode=${frame.opcode} len=${frame.payload.length}`);
        if (peer.onmessage !== null) {
          peer.onmessage({
            isText: frame.opcode === OP_TEXT,
            data: frame.payload,
          });
        }
      }
    }
  }
}

// ── Browser-faithful WebSocket client polyfill ──────────────────────────────

/**
 * The Emscripten WebSocket shim requires a global WebSocket with the
 * browser surface it touches: readyState numeric constants, binaryType =
 * 'arraybuffer', extensions/protocol strings, on{open,error,close,message}
 * handlers, send(string | typed array), and close(code). Text frames are
 * delivered as JavaScript strings and binary frames as ArrayBuffers, which
 * is exactly what browsers do and all the shim distinguishes.
 */
class HarnessWebSocket {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;

  constructor(url) {
    const parsed = /^ws:\/\/([^:/]+):(\d+)(\/.*)$/.exec(url);
    if (parsed === null) {
      throw new Error(`HarnessWebSocket cannot parse ${url}`);
    }
    this.url = url;
    this.readyState = HarnessWebSocket.CONNECTING;
    this.binaryType = "arraybuffer";
    this.extensions = "";
    this.protocol = "";
    this.bufferedAmount = 0;
    this.onopen = null;
    this.onerror = null;
    this.onclose = null;
    this.onmessage = null;

    this.parser = new FrameParser();
    this.acceptKey = crypto.randomBytes(16).toString("base64");
    this.socket = net.connect(Number(parsed[2]), parsed[1], () => {
      this.socket.write(
        `GET ${parsed[3]} HTTP/1.1\r\n` +
          `Host: ${parsed[1]}:${parsed[2]}\r\n` +
          "Upgrade: websocket\r\n" +
          "Connection: Upgrade\r\n" +
          `Sec-WebSocket-Key: ${this.acceptKey}\r\n` +
          "Sec-WebSocket-Version: 13\r\n" +
          "\r\n",
      );
    });

    let handshake = Buffer.alloc(0);
    let established = false;
    this.socket.setNoDelay(true);
    this.socket.on("data", (chunk) => {
      if (!established) {
        handshake = Buffer.concat([handshake, chunk]);
        const end = handshake.indexOf("\r\n\r\n");
        if (end === -1) {
          if (handshake.length > 16 * 1024) this.failConnection(1002, "handshake too large");
          return;
        }
        const head = handshake.subarray(0, end).toString("latin1");
        const status = head.split("\r\n")[0];
        if (!/^http\/1\.1 101/i.test(status)) {
          this.failConnection(1002, "handshake rejected");
          return;
        }
        // RFC 6455 4.1: the client must verify the server's accept key.
        const serverAccept = /^sec-websocket-accept:\s*(.+)\r?$/im.exec(head);
        const expectedAccept = crypto
          .createHash("sha1")
          .update(this.acceptKey + WS_GUID)
          .digest("base64");
        if (serverAccept === null || serverAccept[1].trim() !== expectedAccept) {
          this.failConnection(1002, "handshake accept-key mismatch");
          return;
        }
        established = true;
        this.readyState = HarnessWebSocket.OPEN;
        debug("client: open");
        if (this.onopen !== null) this.onopen({ type: "open" });
        const rest = handshake.subarray(end + 4);
        if (rest.length > 0) this.consume(rest);
        return;
      }
      this.consume(chunk);
    });
    this.socket.on("error", (error) => {
      debug(`client: socket error ${error.code || error.message}`);
      // Abnormal termination: browsers surface an error event, then a
      // synthetic 1006 unclean close.
      this.failConnection(1006, "");
    });
    this.socket.on("close", () => {
      debug(`client: socket close event readyState=${this.readyState}`);
      if (this.readyState !== HarnessWebSocket.CLOSED) {
        this.failConnection(1006, "");
      }
    });
  }

  /** Mark the connection failed: onerror then synthetic unclean close. */
  failConnection(code, reason) {
    debug(`client: failConnection code=${code} readyState=${this.readyState}`);
    if (this.readyState === HarnessWebSocket.CLOSED) return;
    const hadError = this.readyState === HarnessWebSocket.OPEN
      || this.readyState === HarnessWebSocket.CONNECTING;
    this.readyState = HarnessWebSocket.CLOSED;
    if (hadError && this.onerror !== null) this.onerror({ type: "error" });
    if (this.onclose !== null) {
      this.onclose({ wasClean: false, code, reason });
    }
  }

  consume(chunk) {
    for (const frame of this.parser.push(chunk)) {
      if (frame.opcode === OP_CLOSE) {
        debug(`client: close frame received`);
        const code = frame.payload.length >= 2 ? frame.payload.readUInt16BE(0) : 1005;
        const reason = frame.payload.length > 2
          ? frame.payload.subarray(2).toString("utf8")
          : "";
        this.readyState = HarnessWebSocket.CLOSED;
        this.socket.write(encodeClientFrame(OP_CLOSE, frame.payload.subarray(0, 2)));
        this.socket.end();
        if (this.onclose !== null) {
          this.onclose({ wasClean: true, code, reason });
        }
        return;
      }
      if (frame.opcode === OP_TEXT) {
        debug(`client: text frame len=${frame.payload.length}`);
        if (this.onmessage !== null) {
          this.onmessage({ data: frame.payload.toString("utf8") });
        }
      } else if (frame.opcode === OP_BINARY) {
        if (this.onmessage !== null) {
          const copy = new ArrayBuffer(frame.payload.length);
          new Uint8Array(copy).set(frame.payload);
          this.onmessage({ data: copy });
        }
      } else if (frame.opcode === OP_PING) {
        this.socket.write(encodeClientFrame(OP_PONG, frame.payload));
      }
    }
  }

  send(data) {
    // Browser-faithful per the WHATWG spec: send() throws only while
    // CONNECTING; on CLOSING/CLOSED the data is silently discarded (this
    // silent discard is exactly the browser behavior the transport's
    // live-ready-state send consult must compensate for).
    if (this.readyState === HarnessWebSocket.CONNECTING) {
      throw new Error("HarnessWebSocket.send called while connecting");
    }
    if (this.readyState !== HarnessWebSocket.OPEN) {
      return;
    }
    if (typeof data === "string") {
      this.socket.write(encodeClientFrame(OP_TEXT, Buffer.from(data, "utf8")));
      return;
    }
    const bytes = ArrayBuffer.isView(data)
      ? Buffer.from(data.buffer, data.byteOffset, data.byteLength)
      : Buffer.from(data);
    this.socket.write(encodeClientFrame(OP_BINARY, bytes));
  }

  close(code, reason) {
    if (this.readyState !== HarnessWebSocket.OPEN) return;
    this.readyState = HarnessWebSocket.CLOSING;
    const payload = Buffer.alloc(2);
    payload.writeUInt16BE(typeof code === "number" ? code : 1000, 0);
    this.socket.write(encodeClientFrame(OP_CLOSE, payload));
    this.socket.end();
  }
}

// ── Server-side scenario scripts ────────────────────────────────────────────

const FLOOD_FRAME = "a".repeat(50);

const SCRIPTS = {
  0: (peer) => {
    peer.onmessage = ({ isText, data }) => {
      if (isText) {
        peer.sendText(data.toString("utf8"));
      } else {
        peer.sendBinary(data);
      }
    };
  },
  1: (peer) => {
    // Close only after the client announces readiness, so the close event
    // cannot overtake the open event inside one scheduling step; the
    // round-24 regression pins that a send on the already-dead socket then
    // fails terminally with the frame retained.
    peer.onmessage = ({ isText, data }) => {
      if (isText && data.toString("utf8") === "ready") {
        peer.sendClose(4000, "draining");
      }
    };
  },
  2: (peer) => {
    // Flood six 50-byte text frames; the transport bound admits two and
    // fuses on the third, dropping the rest at the callback.
    for (let i = 0; i < 6; ++i) peer.sendText(FLOOD_FRAME);
  },
  3: (peer) => {
    // Destroy the TCP connection without a WebSocket close; the browser
    // (polyfill) surfaces onerror + synthetic 1006 unclean close.
    debug("server: abrupt destroy scheduled in 30ms");
    setTimeout(() => {
      debug("server: destroying socket now");
      peer.destroy();
    }, 30);
  },
};

// ── Harness module driving ──────────────────────────────────────────────────

async function main() {
  const harnessJs = process.argv[2]
    ? path.resolve(process.argv[2])
    : undefined;
  if (!harnessJs) {
    fail("usage: node run-harness.cjs <path-to-harness.js>");
  }
  const server = new LoopbackServer();
  const port = await server.listen();
  const url = `ws://127.0.0.1:${port}${HARNESS_URL_PATH}`;
  process.stdout.write(`loopback server on ${url}\n`);

  // The Emscripten shim resolves `WebSocket` from the global scope.
  globalThis.WebSocket = HarnessWebSocket;

  const deadline = Date.now() + DEADLINE_MS;

  const runScenario = (mode) => new Promise((resolve) => {
    server.script = SCRIPTS[mode];
    // Scenario 0 pins the pre-open Pending retention contract, so its
    // handshake is delayed to guarantee several CONNECTING steps; every
    // other scenario opens immediately.
    server.handshakeDelayMs = mode === 0 ? 10 : 0;
    const begin = globalThis.Module.ccall(
      "sfh_begin",
      "number",
      ["string", "number"],
      [url, mode],
    );
    if (begin !== 0) {
      const reason = globalThis.Module.UTF8ToString(
        globalThis.Module.ccall("sfh_fail_reason", "number"),
      );
      fail(`scenario ${mode}: sfh_begin failed: ${reason}`);
    }
    const pump = () => {
      if (Date.now() > deadline) {
        fail(`scenario ${mode}: exceeded the ${DEADLINE_MS}ms harness deadline`);
      }
      // Pace one scheduling step per ~1ms so server-side timers (the
      // abrupt-destroy scenario) get event-loop time and the cadence
      // resembles a real polling game loop.
      setTimeout(() => {
        const status = globalThis.Module.ccall("sfh_step", "number");
        if (status === 0) {
          pump();
          return;
        }
        if (status === 1) {
          process.stdout.write(`scenario ${mode}: PASS\n`);
          resolve();
          return;
        }
        const reason = globalThis.Module.UTF8ToString(
          globalThis.Module.ccall("sfh_fail_reason", "number"),
        );
        fail(`scenario ${mode}: ${reason}`);
      }, 1);
    };
    pump();
  });

  // Non-modularized emscripten output for ENVIRONMENT=node initializes
  // synchronously (main() is empty) and exports Module; main() is empty, so
  // once the script is loaded the exports are callable.
  const module = require(harnessJs);
  const active = module && typeof module.ccall === "function"
    ? module
    : globalThis.Module;
  if (!active || typeof active.ccall !== "function") {
    fail("emscripten module did not expose Module.ccall");
  }
  globalThis.Module = active;

  try {
    for (let mode = 0; mode < SCENARIO_COUNT; ++mode) {
      await runScenario(mode);
    }
  } finally {
    await server.close();
  }
  process.stdout.write("all scenarios passed\n");
}

main().catch((error) => fail(error && error.stack ? error.stack : String(error)));
