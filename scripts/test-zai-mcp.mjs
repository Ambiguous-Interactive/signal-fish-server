import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { credentials, relay } from '../.devcontainer/zai-mcp.mjs';

test('dotenv file wins over stale env, handles quotes, and never executes expressions', () => {
  const dir = mkdtempSync(join(tmpdir(), 'zai-test-'));
  const file = join(dir, '.env.local');
  try {
    assert.equal(credentials(file, { Z_AI_API_KEY: 'fallback' }).Z_AI_API_KEY, 'fallback');
    writeFileSync(file, '\uFEFF# test\r\nexport Z_AI_API_KEY="file-secret" # comment\r\n');
    assert.equal(credentials(file, { Z_AI_API_KEY: 'stale' }).Z_AI_API_KEY, 'file-secret');
    writeFileSync(file, 'Z_AI_API_KEY=$(never-execute)\n');
    assert.equal(credentials(file, {}).Z_AI_API_KEY, '$(never-execute)');
    writeFileSync(file, 'OTHER=value\n');
    assert.equal(credentials(file, { Z_AI_API_KEY: 'fallback' }).Z_AI_API_KEY, 'fallback');
    for (const value of ['', '" "', '"a\\nb"']) {
      writeFileSync(file, `Z_AI_API_KEY=${value}\n`);
      assert.throws(() => credentials(file, { Z_AI_API_KEY: 'stale' }), /nonempty/);
    }
    assert.throws(() => credentials(dir, {}), /Cannot read/);
  } finally { rmSync(dir, { recursive: true }); }
});

test('relay preserves IDs, negotiated version, notifications, tools, and cleanup', async () => {
  const events = [];
  const transport = name => ({
    start: async () => { events.push(`${name}:start`); },
    close: async () => { events.push(`${name}:close`); },
    send: async message => { events.push([name, message]); },
    setProtocolVersion: version => { events.push(['version', version]); },
  });
  const local = transport('local'), remote = transport('remote');
  const close = await relay(local, remote);
  const init = { jsonrpc: '2.0', id: 42, method: 'initialize', params: {} };
  local.onmessage(init);
  await new Promise(resolve => setImmediate(resolve));
  const response = { jsonrpc: '2.0', id: 42, result: { protocolVersion: '2025-03-26' } };
  remote.onmessage(response);
  const notification = { jsonrpc: '2.0', method: 'notifications/initialized' };
  const call = { jsonrpc: '2.0', id: 43, method: 'tools/call', params: { name: 'test' } };
  local.onmessage(notification); local.onmessage(call);
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(events.slice(0, 2), ['remote:start', 'local:start']);
  assert.deepEqual(events.slice(2), [['remote', init], ['version', '2025-03-26'],
    ['local', response], ['remote', notification], ['remote', call]]);
  await close(); await close();
  assert.deepEqual(events.slice(-2), ['local:close', 'remote:close']);
});
