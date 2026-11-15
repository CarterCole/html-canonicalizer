// End to end for the in-page build: run canonicalize() inside real pages, then check the
// result the strict way the in-page code cannot, by screenshot. The result's document is
// loaded in a second tab, its root placed where the original was, and the two pictures of
// that region must match pixel for pixel.
//
//   node --test web/test/        (after web/build.sh)
//
// Without a Chrome on the machine the tests skip.

import assert from 'node:assert/strict';
import { after, before, test } from 'node:test';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { decodePng, diffPixels, findChrome, launch, serve } from './cdp.mjs';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '../..');
const STRICT_CSS = `
body { margin: 30px; font: 15px/1.5 Helvetica, Arial, sans-serif; background: #fafafa; }
.card { width: 320px; background: #fff; border: 1px solid #ddd; border-radius: 10px; }
.card > div > div { padding: 16px; }
.card h2 { margin: 0 0 6px; font-size: 18px; }
.card p { margin: 0; color: #555; }
.card .tag { display: inline-block; margin-top: 10px; padding: 2px 8px; border-radius: 99px; background: #e0f2fe; color: #075985; font-size: 12px; }`;
const STRICT_PAGE = `<!doctype html><html><head><meta charset="utf-8"><link rel="stylesheet" href="/strict/card.css"></head>
<body><div class="card" id="card"><div><div><h2><span>Strict host</span></h2><p><span>No inline styles, no </span><span>innerHTML.</span></p><span class="tag"><span>CSP</span></span></div></div></div></body></html>`;

const chrome = findChrome();
let browser, server;

before(async () => {
  if (!chrome) return;
  server = await serve(ROOT, {
    // A strict policy on the hosts that ask for it: no inline styles, no markup sinks.
    headers: (path) => path.includes('strict') ? {
      'content-security-policy': "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; img-src 'self' data:; require-trusted-types-for 'script'",
    } : {},
    routes: {
      '/strict/card.html': { headers: { 'content-type': 'text/html; charset=utf-8' }, body: STRICT_PAGE },
      '/strict/card.css': { headers: { 'content-type': 'text/css' }, body: STRICT_CSS },
    },
  });
  browser = await launch({ chrome });
});

after(async () => {
  if (browser) browser.close();
  if (server) await server.close();
});

/** Load `url`, run canonicalize there, and compare the result with the original by pixels. */
async function roundTrip(t, url, { selector = null, options = {}, viewport = [1280, 800] } = {}) {
  if (!chrome) return t.skip('no Chrome on this machine');
  const page = await browser.newPage({ width: viewport[0], height: viewport[1] });
  await page.navigate(url);
  const res = await page.eval(`(async () => {
    const m = await import('/web/dist/canon.js');
    const target = ${JSON.stringify(selector)} ? document.querySelector(${JSON.stringify(selector)}) : null;
    const t0 = performance.now();
    const r = await m.canonicalize(target, ${JSON.stringify(options)});
    const root = target || (document.body.children.length === 1 ? document.body.firstElementChild : document.body);
    const b = root.getBoundingClientRect();
    return { ...r, ms: performance.now() - t0, rect: [b.left, b.top, b.width, b.height] };
  })()`);
  assert.ok(res, 'canonicalize returned nothing; page log: ' + page.logs.join('\n'));
  // The host page is left as it was: no frame, no marker attributes.
  assert.equal(await page.eval('document.querySelectorAll("iframe[aria-hidden]").length'), 0);
  assert.equal(await page.eval('document.querySelectorAll("[data-canon-q],[data-canon-root]").length'), 0);

  // Picture of the original, then of the result placed at the same spot.
  const [x, y, w, h] = res.rect;
  const pad = 12;
  const clip = { x: Math.max(0, Math.floor(x) - pad), y: Math.max(0, Math.floor(y) - pad), width: Math.ceil(w) + 2 * pad, height: Math.ceil(h) + 2 * pad };
  // Photograph the original alone, as the CLI does: everything else hidden, the backdrop
  // flat. (Rasterisation of gradients and shadows depends on what else is on the page.)
  const bg = (res.document.match(/<body style="margin:0;background:([^"]+)"/) || [])[1] || '#fff';
  await page.eval(`(() => {
    const root = ${JSON.stringify(selector)} ? document.querySelector(${JSON.stringify(selector)}) : (document.body.children.length === 1 ? document.body.firstElementChild : document.body);
    root.setAttribute('data-iso-root', '');
    for (let a = root.parentElement; a; a = a.parentElement) a.setAttribute('data-iso-anc', '');
    const s = new CSSStyleSheet();
    s.replaceSync(':not([data-iso-root], [data-iso-root] *) { visibility: hidden !important; } [data-iso-root] { visibility: visible !important; } [data-iso-anc] { background: none !important; } :root { background: ${bg} !important; }');
    document.adoptedStyleSheets = [...document.adoptedStyleSheets, s];
    return new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
  })()`);
  const before = decodePng(await page.screenshot(clip));
  const copy = await browser.newPage({ width: viewport[0], height: viewport[1] });
  await copy.navigate('data:text/html;charset=utf-8,' + encodeURIComponent(res.document));
  await copy.eval(`(async () => {
    document.body.style.padding = '${y}px 0 0 ${x}px';
    await document.fonts.ready;
    await Promise.all([...document.images].map((i) => i.decode().catch(() => {})));
    await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
  })()`);
  const after = decodePng(await copy.screenshot(clip));
  const differing = diffPixels(before, after);
  await copy.close();
  await page.close();
  return { ...res, differing };
}

const fixture = (name) => server.url + '/fixtures/' + name;

test('div soup: wrappers go, the card stays, pixel-identical', async (t) => {
  const r = await roundTrip(t, fixture('div-soup.html'));
  if (!r) return;
  assert.equal(r.verified, true, JSON.stringify(r.verification) + ' ' + r.warnings.join('; '));
  assert.equal(r.sourceElements, 29);
  assert.ok(r.elements <= 12, r.elements + ' elements');
  assert.equal(r.differing, 0);
  assert.match(r.html, /Quarterly report/);
});

test('rich text: editor span soup', async (t) => {
  const r = await roundTrip(t, fixture('rich-text.html'));
  if (!r) return;
  assert.equal(r.verified, true, r.warnings.join('; '));
  assert.ok(r.elements <= 40, r.elements + ' elements');
  assert.equal(r.differing, 0);
});

test('dashboard: tables, form controls, SVG, generated content', async (t) => {
  const r = await roundTrip(t, fixture('dashboard.html'));
  if (!r) return;
  assert.equal(r.verified, true, r.warnings.join('; '));
  assert.ok(r.elements <= 75, r.elements + ' elements');
  assert.match(r.css, /::placeholder\{/);
  assert.equal(r.differing, 0);
});

test('an element passed directly', async (t) => {
  const r = await roundTrip(t, fixture('dashboard.html'), { selector: '.stats' });
  if (!r) return;
  assert.equal(r.verified, true, r.warnings.join('; '));
  assert.match(r.html, /1,284,112/);
  assert.doesNotMatch(r.html, /Recent deployments/);
  assert.equal(r.differing, 0);
});

test('a host with a strict CSP and Trusted Types', async (t) => {
  const r = await roundTrip(t, server ? server.url + '/strict/card.html' : '', { selector: '#card' });
  if (!r) return;
  assert.equal(r.verified, true, r.warnings.join('; '));
  assert.ok(r.elements < r.sourceElements);
  assert.equal(r.differing, 0);
});

