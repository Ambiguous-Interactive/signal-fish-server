#!/usr/bin/env node
// One startup path for every frontend. Never write credentials to config or logs.
import { readFileSync } from 'node:fs';
import { parseEnv } from 'node:util';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';
import { resolve } from 'node:path';

export const envFile = fileURLToPath(new URL('../.env.local', import.meta.url));
export const vision = '/home/vscode/.npm-global/bin/zai-mcp-server';
export const endpoints = {
  'web-search': 'https://api.z.ai/api/mcp/web_search_prime/mcp',
  'web-reader': 'https://api.z.ai/api/mcp/web_reader/mcp',
  zread: 'https://api.z.ai/api/mcp/zread/mcp',
};

export function credentials(path = envFile, environment = process.env) {
  let values = {};
  try {
    values = parseEnv(readFileSync(path, 'utf8'));
  } catch (error) {
    if (error.code !== 'ENOENT') throw new Error('Cannot read .env.local');
  }
  // An explicit empty file entry also overrides a stale inherited credential.
  const key = values.Z_AI_API_KEY ?? environment.Z_AI_API_KEY ?? '';
  if (!key || /\s/.test(key)) throw new Error('Set a nonempty Z_AI_API_KEY in .env.local');
  return { ...environment, Z_AI_API_KEY: key, Z_AI_MODE: 'ZAI' };
}

// Relay the wire protocol rather than re-declaring upstream tools/capabilities.
// The SDK owns HTTP sessions, SSE framing and accepted empty notification replies.
export async function relay(local, remote) {
  let initializeId;
  let pending = Promise.resolve();
  let closed = false;
  const close = async () => {
    if (closed) return;
    closed = true;
    await Promise.allSettled([local.close(), remote.close()]);
  };
  local.onclose = close;
  remote.onclose = close;
  const fail = () => {
    console.error('Z.AI MCP transport failed; run python3 scripts/check-zai-mcp.py --live');
    process.exitCode = 1;
    void close();
  };
  local.onerror = fail;
  remote.onerror = fail;
  remote.onmessage = message => {
    if (message.id === initializeId && message.result?.protocolVersion) {
      remote.setProtocolVersion(message.result.protocolVersion);
    }
    void local.send(message).catch(fail);
  };
  local.onmessage = message => {
    if (message.method === 'initialize') initializeId = message.id;
    pending = pending.then(() => remote.send(message)).catch(fail);
  };
  try {
    await remote.start();
    await local.start();
  } catch (error) {
    await close();
    throw error;
  }
  return close;
}

export async function main(name, path = envFile) {
  const environment = credentials(path);
  if (name === 'vision') {
    const child = spawn(vision, [], { env: environment, stdio: ['inherit', 'inherit', 'ignore'] });
    for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => child.kill(signal));
    child.on('error', () => { console.error('Z.AI Vision binary unavailable; rerun agent setup'); process.exitCode = 1; });
    child.on('exit', code => { process.exitCode = code ?? 1; });
    return;
  }
  if (!Object.hasOwn(endpoints, name)) throw new Error('Unknown Z.AI MCP server');
  const require = createRequire('/home/vscode/.npm-global/lib/node_modules/@z_ai/mcp-server/package.json');
  const { StreamableHTTPClientTransport } = require('@modelcontextprotocol/sdk/client/streamableHttp.js');
  const { StdioServerTransport } = require('@modelcontextprotocol/sdk/server/stdio.js');
  const remote = new StreamableHTTPClientTransport(new URL(endpoints[name]), {
    requestInit: { headers: { Authorization: `Bearer ${environment.Z_AI_API_KEY}` } },
  });
  const close = await relay(new StdioServerTransport(), remote);
  for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => void close());
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(process.argv[2], process.argv[3]).catch(() => {
    console.error('Z.AI MCP startup failed; check .env.local and the installed Vision package');
    process.exitCode = 1;
  });
}
