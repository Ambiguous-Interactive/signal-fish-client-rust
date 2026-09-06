#!/usr/bin/env node
// Load only MCP credentials from dotenv data; never execute workspace shell code.
const fs = require('node:fs');
const { parseEnv } = require('node:util');
const { spawn } = require('node:child_process');

const CREDENTIAL_NAMES = [
  'Z_AI_API_KEY', 'Z_AI_MODE', 'GITHUB_PERSONAL_ACCESS_TOKEN',
  'GITHUB_TOKEN', 'GH_TOKEN', 'GITHUB_PAT',
];

function credentialEnvironment(contents, inherited) {
  const parsed = parseEnv(contents);
  const env = { ...inherited };
  for (const name of CREDENTIAL_NAMES) {
    // remoteEnv may inject an empty value when the host variable is absent.
    if (!env[name] && parsed[name]) env[name] = parsed[name];
  }
  return env;
}

function main() {
  const [file, launcher, ...args] = process.argv.slice(2);
  let env;
  try {
    env = credentialEnvironment(fs.readFileSync(file, 'utf8'), process.env);
  } catch {
    console.error('signal-fish-mcp: cannot read .env.local; check file permissions and dotenv syntax');
    process.exitCode = 1;
    return;
  }
  const child = spawn('bash', [launcher, '--env-loaded', ...args], { env, stdio: 'inherit' });
  const handlers = new Map();
  for (const signal of ['SIGTERM', 'SIGINT', 'SIGHUP']) {
    const handler = () => child.kill(signal);
    handlers.set(signal, handler);
    process.on(signal, handler);
  }
  child.on('error', () => {
    console.error('signal-fish-mcp: could not start bash; run the agent inside the devcontainer');
    process.exitCode = 1;
  });
  child.on('close', (code, signal) => {
    for (const [name, handler] of handlers) process.removeListener(name, handler);
    if (signal) process.kill(process.pid, signal);
    else process.exitCode = code ?? 1;
  });
}

module.exports = { credentialEnvironment };
if (require.main === module) main();
