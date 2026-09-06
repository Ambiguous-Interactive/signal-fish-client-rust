const assert = require('node:assert/strict');
const { test } = require('node:test');
const { credentialEnvironment } = require('../.devcontainer/scripts/mcp-env.cjs');

test('loads quoted dotenv credentials, comments, CRLF and export syntax', () => {
  const env = credentialEnvironment(
    '# local credentials\r\nexport Z_AI_API_KEY="local key # literal"\r\nGH_TOKEN=local-token # comment\r\n',
    {},
  );
  assert.equal(env.Z_AI_API_KEY, 'local key # literal');
  assert.equal(env.GH_TOKEN, 'local-token');
});

test('nonempty inherited credentials win; empty remoteEnv values allow fallback', () => {
  const inherited = { Z_AI_API_KEY: 'inherited', GH_TOKEN: '', PATH: '/usr/bin' };
  const env = credentialEnvironment('Z_AI_API_KEY=local\nGH_TOKEN=local-token', inherited);
  assert.equal(env.Z_AI_API_KEY, 'inherited');
  assert.equal(env.GH_TOKEN, 'local-token');
  assert.equal(env.PATH, '/usr/bin');
  assert.equal(inherited.GH_TOKEN, '');
});

test('does not load unrelated configuration or execute/interpolate shell syntax', () => {
  const env = credentialEnvironment(
    'NODE_OPTIONS=--require=untrusted.js\nPATH=/untrusted\nZ_AI_API_KEY=\'$(touch should-not-exist) ${SECRET}\'\n',
    {},
  );
  assert.deepEqual(env, { Z_AI_API_KEY: '$(touch should-not-exist) ${SECRET}' });
});
