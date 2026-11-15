// Test tooling: drive a headless Chrome over the DevTools protocol from Node (22+, which
// has a global WebSocket), serve files with chosen response headers, and read screenshots.
// No dependencies.

import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, statSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { delimiter, extname, join, resolve, sep } from 'node:path';
import { inflateSync } from 'node:zlib';

export function findChrome() {
  for (const v of ['CANON_CHROME', 'CHROME_PATH', 'CHROME']) {
    if (process.env[v] && existsSync(process.env[v])) return process.env[v];
  }
  const fixed = process.platform === 'darwin'
    ? [
        '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
        '/Applications/Chromium.app/Contents/MacOS/Chromium',
        '/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary',
        '/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge',
      ]
    : process.platform === 'win32'
      ? ['C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe', 'C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe']
      : [];
  for (const p of fixed) if (existsSync(p)) return p;
  for (const dir of (process.env.PATH || '').split(delimiter)) {
    for (const name of ['google-chrome', 'google-chrome-stable', 'chromium', 'chromium-browser', 'chrome']) {
      const p = join(dir, name);
      if (existsSync(p)) return p;
    }
  }
  return null;
}

// launch() -> { newPage(opts), close() }. The same deterministic rendering flags the CLI uses.
export async function launch(opts = {}) {
  const exe = opts.chrome || findChrome();
  if (!exe) throw new Error('no Chrome or Chromium found');
  const profile = mkdtempSync(join(tmpdir(), 'canon-web-'));
  const child = spawn(exe, [
    '--headless=new', '--remote-debugging-port=0', `--user-data-dir=${profile}`,
    '--no-first-run', '--no-default-browser-check', '--disable-extensions',
    '--disable-background-networking', '--disable-component-update', '--disable-sync', '--mute-audio',
    '--hide-scrollbars', '--disable-lcd-text', '--force-color-profile=srgb', '--force-device-scale-factor=1',
    '--disable-features=Translate,OptimizationHints,MediaRouter',
    '--disable-background-timer-throttling', '--disable-renderer-backgrounding', '--disable-backgrounding-occluded-windows',
    ...(opts.args || []), 'about:blank',
  ], { stdio: ['ignore', 'ignore', 'pipe'] });
  const wsUrl = await new Promise((res, rej) => {
    let buf = '';
    const timer = setTimeout(() => rej(new Error('Chrome did not open a DevTools endpoint: ' + buf.slice(0, 400))), 30000);
    child.stderr.on('data', (d) => {
      buf += d;
      const m = buf.match(/DevTools listening on (ws:\/\/\S+)/);
      if (m) { clearTimeout(timer); res(m[1]); }
    });
    child.on('exit', () => { clearTimeout(timer); rej(new Error('Chrome exited: ' + buf.slice(0, 400))); });
  });
  const ws = new WebSocket(wsUrl);
  await new Promise((res, rej) => { ws.addEventListener('open', res); ws.addEventListener('error', rej); });
  let nextId = 0;
  const pending = new Map();
  const sessions = new Map(); // sessionId -> (method, params) => void
  ws.addEventListener('message', (ev) => {
    const m = JSON.parse(ev.data);
    if (m.id !== undefined) {
      const p = pending.get(m.id);
      if (!p) return;
      pending.delete(m.id);
      if (m.error) p.rej(new Error(`${p.method}: ${m.error.message}`));
      else p.res(m.result);
    } else if (m.method && sessions.has(m.sessionId)) {
      sessions.get(m.sessionId)(m.method, m.params);
    }
  });
  const call = (method, params = {}, sessionId) => new Promise((res, rej) => {
    const id = ++nextId;
    pending.set(id, { res, rej, method });
    ws.send(JSON.stringify({ id, method, params, sessionId }));
  });

  async function newPage({ width = 1280, height = 800, dpr = 1 } = {}) {
    const { targetId } = await call('Target.createTarget', { url: 'about:blank' });
    const { sessionId } = await call('Target.attachToTarget', { targetId, flatten: true });
    const waiters = []; // { method, pred, res }
    const listeners = []; // [method, fn]
    const page = {
      /** Call `fn(params)` for every `method` event. */
      on(method, fn) { listeners.push([method, fn]); },
      /**
       * Answer requests whose URL contains `marker` from local files: `marker` + path is read
       * from `dir` + path. Makes a local build look like part of any site (same origin).
       */
      async serveAs(marker, dir) {
        page.on('Fetch.requestPaused', async (p) => {
          const path = decodeURIComponent(new URL(p.request.url).pathname.split(marker)[1] || '');
          const file = resolve(join(dir, path));
          if (!file.startsWith(resolve(dir) + sep) || !existsSync(file)) {
            await page.call('Fetch.fulfillRequest', { requestId: p.requestId, responseCode: 404, body: '' }).catch(() => {});
            return;
          }
          await page.call('Fetch.fulfillRequest', {
            requestId: p.requestId, responseCode: 200,
            responseHeaders: [{ name: 'content-type', value: TYPES[extname(file)] || 'application/octet-stream' }],
            body: readFileSync(file).toString('base64'),
          }).catch(() => {});
        });
        await page.call('Fetch.enable', { patterns: [{ urlPattern: '*' + marker + '*' }] });
      },
      /** console.* output and uncaught exceptions of the page, in order. */
      logs: [],
      call: (method, params) => call(method, params, sessionId),
      /** Resolve with the params of the next `method` event (optionally matching `pred`). */
      waitFor(method, pred = () => true, timeout = 30000) {
        return new Promise((res, rej) => {
          const w = { method, pred, res };
          waiters.push(w);
          setTimeout(() => { const i = waiters.indexOf(w); if (i >= 0) { waiters.splice(i, 1); rej(new Error('timed out waiting for ' + method)); } }, timeout);
        });
      },
      /** Evaluate an expression; promises are awaited; the value comes back as JSON. */
      async eval(expression) {
        const r = await page.call('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
        if (r.exceptionDetails) throw new Error('page script failed: ' + (r.exceptionDetails.exception?.description || r.exceptionDetails.text));
        return r.result.value;
      },
      async navigate(url, { timeout = 30000 } = {}) {
        const loaded = page.waitFor('Page.loadEventFired', () => true, timeout);
        const r = await page.call('Page.navigate', { url });
        if (r.errorText) throw new Error(`could not load ${url}: ${r.errorText}`);
        await loaded;
      },
      async setViewport(w, h, scale = 1) {
        await page.call('Emulation.setDeviceMetricsOverride', { width: w, height: h, deviceScaleFactor: scale, mobile: false });
      },
      /** PNG (Buffer) of a region in document CSS px: { x, y, width, height }. */
      async screenshot(clip) {
        const params = { format: 'png' };
        if (clip) {
          params.clip = { ...clip, scale: 1 };
          params.captureBeyondViewport = clip.x + clip.width > width || clip.y + clip.height > height;
        }
        const r = await page.call('Page.captureScreenshot', params);
        return Buffer.from(r.data, 'base64');
      },
      async close() { await call('Target.closeTarget', { targetId }).catch(() => {}); sessions.delete(sessionId); },
    };
    sessions.set(sessionId, (method, params) => {
      if (method === 'Runtime.consoleAPICalled') page.logs.push(`[${params.type}] ` + params.args.map((a) => a.value ?? a.description ?? '').join(' '));
      if (method === 'Runtime.exceptionThrown') page.logs.push('[exception] ' + (params.exceptionDetails.exception?.description || params.exceptionDetails.text));
      for (const [m, fn] of listeners) if (m === method) fn(params);
      for (let i = waiters.length - 1; i >= 0; i--) {
        if (waiters[i].method === method && waiters[i].pred(params)) waiters.splice(i, 1)[0].res(params);
      }
    });
    await page.call('Page.enable');
    await page.call('Runtime.enable');
    await page.setViewport(width, height, dpr);
    return page;
  }

  return {
    newPage,
    close() {
      try { ws.close(); } catch { /* already closed */ }
      child.kill();
      try { rmSync(profile, { recursive: true, force: true }); } catch { /* Chrome may still hold it */ }
    },
  };
}

const TYPES = {
  '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8', '.json': 'application/json', '.wasm': 'application/wasm', '.svg': 'image/svg+xml',
  '.png': 'image/png', '.jpg': 'image/jpeg', '.gif': 'image/gif', '.woff2': 'font/woff2', '.woff': 'font/woff', '.ttf': 'font/ttf',
  '.otf': 'font/otf', '.txt': 'text/plain; charset=utf-8',
};

// serve(root, { headers, routes }) -> { url, port, close() } on 127.0.0.1.
//   headers: object, or (pathname) => object: extra response headers (CSP, CORS, caching).
//   routes:  { '/path': { status, headers, body } | (req, res) => void }: answered before files.
// Start two servers to get two origins (the port is part of the origin).
export async function serve(root, { headers = {}, routes = {} } = {}) {
  const base = resolve(root);
  const server = createServer((req, res) => {
    const { pathname } = new URL(req.url, 'http://x');
    const extra = typeof headers === 'function' ? headers(pathname) || {} : headers;
    const route = routes[pathname];
    if (typeof route === 'function') return route(req, res);
    if (route) {
      res.writeHead(route.status || 200, { 'content-type': 'text/plain; charset=utf-8', ...extra, ...(route.headers || {}) });
      return res.end(route.body ?? '');
    }
    const file = resolve(join(base, decodeURIComponent(pathname)));
    if (!(file === base || file.startsWith(base + sep)) || !existsSync(file) || !statSync(file).isFile()) {
      res.writeHead(404, extra);
      return res.end('not found');
    }
    res.writeHead(200, { 'content-type': TYPES[extname(file)] || 'application/octet-stream', ...extra });
    res.end(readFileSync(file));
  });
  await new Promise((res) => server.listen(0, '127.0.0.1', res));
  const { port } = server.address();
  return { url: `http://127.0.0.1:${port}`, port, close: () => new Promise((res) => server.close(res)) };
}

// decodePng(buffer) -> { width, height, data } with data as RGBA bytes.
// Handles what Chrome writes: 8-bit RGB or RGBA, not interlaced.
export function decodePng(buf) {
  let pos = 8, width = 0, height = 0, colorType = 6;
  const idat = [];
  while (pos < buf.length) {
    const len = buf.readUInt32BE(pos);
    const type = buf.toString('latin1', pos + 4, pos + 8);
    const body = buf.subarray(pos + 8, pos + 8 + len);
    if (type === 'IHDR') {
      width = body.readUInt32BE(0);
      height = body.readUInt32BE(4);
      colorType = body[9];
      if (body[8] !== 8 || (colorType !== 2 && colorType !== 6) || body[12] !== 0) throw new Error('unsupported PNG variant');
    } else if (type === 'IDAT') idat.push(body);
    pos += 12 + len;
  }
  const bpp = colorType === 6 ? 4 : 3;
  const stride = width * bpp;
  const raw = inflateSync(Buffer.concat(idat));
  const data = new Uint8Array(width * height * 4);
  let prev = new Uint8Array(stride);
  for (let y = 0; y < height; y++) {
    const filter = raw[y * (stride + 1)];
    const line = Uint8Array.from(raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1)));
    for (let x = 0; x < stride; x++) {
      const a = x >= bpp ? line[x - bpp] : 0, b = prev[x], c = x >= bpp ? prev[x - bpp] : 0;
      let add = 0;
      if (filter === 1) add = a;
      else if (filter === 2) add = b;
      else if (filter === 3) add = (a + b) >> 1;
      else if (filter === 4) {
        const p = a + b - c, pa = Math.abs(p - a), pb = Math.abs(p - b), pc = Math.abs(p - c);
        add = pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
      }
      line[x] = (line[x] + add) & 255;
    }
    for (let x = 0; x < width; x++) {
      const o = (y * width + x) * 4;
      data[o] = line[x * bpp];
      data[o + 1] = line[x * bpp + 1];
      data[o + 2] = line[x * bpp + 2];
      data[o + 3] = bpp === 4 ? line[x * bpp + 3] : 255;
    }
    prev = line;
  }
  return { width, height, data };
}

/** Number of pixels where any channel differs between two decoded PNGs (Infinity if sizes differ). */
export function diffPixels(a, b) {
  if (a.width !== b.width || a.height !== b.height) return Infinity;
  let n = 0;
  for (let i = 0; i < a.data.length; i += 4) {
    if (a.data[i] !== b.data[i] || a.data[i + 1] !== b.data[i + 1] || a.data[i + 2] !== b.data[i + 2] || a.data[i + 3] !== b.data[i + 3]) n++;
  }
  return n;
}
