// The in-page build against hostile or unusual hosts: what it leaves behind, what it refuses,
// and what it must not claim. Each case is a small page served with its own headers.
//
//   node --test web/test/        (after web/build.sh)

import assert from 'node:assert/strict';
import { after, before, test } from 'node:test';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { existsSync, readFileSync } from 'node:fs';
import { findChrome, launch, serve } from './cdp.mjs';

// Any font file on this machine will do for the font tests; it is served, never copied.
const FONT = ['/System/Library/Fonts/Supplemental/Courier New.ttf', '/System/Library/Fonts/Monaco.ttf', '/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf', 'C:\\Windows\\Fonts\\cour.ttf'].find((f) => existsSync(f));

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '../..');
const chrome = findChrome();
let browser, server;

const page = (body, head = '') => `<!doctype html><html><head><meta charset="utf-8">${head}</head><body style="margin:20px">${body}</body></html>`;
const html = (body, headers = {}) => ({ headers: { 'content-type': 'text/html; charset=utf-8', ...headers }, body });
const STRICT_STYLE = { 'content-security-policy': "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'" };

const ROUTES = {
  // An ancestor transform set from script, on a host that refuses style attributes.
  '/rotated.html': html(page(`<div id="wrap"><div id="t"><div><p>Rotated</p></div></div></div><script src="/rotate.js"></script>`), STRICT_STYLE),
  '/rotate.js': { headers: { 'content-type': 'text/javascript' }, body: "document.getElementById('wrap').style.transform = 'rotate(10deg)';" },
  // Page CSS that would shrink any iframe.
  '/narrow.html': html(page('<div id="t"><div><b>iframes clamped</b></div></div>', '<style>iframe{max-width:600px!important;display:none}</style>')),
  // Trusted Types enforced; only the policy name the module uses is allowed (twice in a row).
  '/tt-named.html': html(page('<div id="t"><div><i>named policy</i></div></div>'), {
    'content-security-policy': "script-src 'self' 'wasm-unsafe-eval'; require-trusted-types-for 'script'; trusted-types canon-frame",
  }),
  // Trusted Types enforced and no policy at all; blob: frames allowed.
  '/tt-none.html': html(page('<div id="t"><div><i>no policy</i></div></div>'), {
    'content-security-policy': "script-src 'self' 'wasm-unsafe-eval'; frame-src 'self' blob:; require-trusted-types-for 'script'; trusted-types 'none'",
  }),
  // Lazy images and a CSS animation the page controls with animation-play-state.
  '/lazy.html': html(page(`<div id="t"><img id="img" loading="lazy" width="20" height="20" src="data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg'/%3E"><span id="spin">spin</span></div>`,
    '<style>@keyframes s{to{opacity:.5}}#spin{animation:s 3s infinite}.paused #spin{animation-play-state:paused}</style>')),
  // A textarea whose value starts with a blank line.
  '/textarea.html': html(page('<div id="t"><textarea rows="4" cols="20">\n\nthird line</textarea></div>')),
  // A display:contents element between <tbody> and <tr>, built with DOM calls (markup could not).
  '/table.html': html(page('<div id="t"></div><script src="/table.js"></script>')),
  '/table.js': { headers: { 'content-type': 'text/javascript' }, body: `
    const t = document.createElement('table'), b = document.createElement('tbody'), w = document.createElement('x-row');
    w.style.display = 'contents'; w.style.color = 'blue';
    const tr = document.createElement('tr'), td = document.createElement('td'); td.textContent = 'cell';
    tr.append(td); w.append(tr); b.append(w); t.append(b); document.getElementById('t').append(t);` },
  // A web font added from script: no @font-face rule anywhere.
  '/fontface.html': html(page('<div id="t" style="font-family:Probe,serif"><div>Script font</div></div><script src="/fontface.js"></script>')),
  '/fontface.js': { headers: { 'content-type': 'text/javascript' }, body: "document.fonts.add(new FontFace('Probe', 'url(/probe-font)'));" },
  // XHTML 1.0 Transitional: limited-quirks mode.
  '/limited.html': html('<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.0 Transitional//EN" "http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd"><html><body style="margin:20px"><div id="t"><table><tr><td><img width="20" height="20" src="data:image/svg+xml,%3Csvg xmlns=%27http://www.w3.org/2000/svg%27/%3E"></td></tr></table></div></body></html>'),
  // CSS zoom on the root.
  '/zoom.html': html(page('<div id="t" style="border-bottom:1px solid #ccc"><div><span>zoomed</span></div></div>', '<style>html{zoom:1.25}</style>')),
  '/probe-font': (req, res) => {
    if (!FONT) { res.writeHead(404); return res.end(); }
    res.writeHead(200, { 'content-type': 'font/ttf' });
    res.end(readFileSync(FONT));
  },
  // A page whose wasm path serves an error page.
  '/bad-wasm.html': html(page('<div id="t">x</div>')),
};

before(async () => {
  if (!chrome) return;
  server = await serve(ROOT, { routes: ROUTES });
  browser = await launch({ chrome });
});

after(async () => {
  if (browser) browser.close();
  if (server) await server.close();
});

async function open(t, path) {
  if (!chrome) { t.skip('no Chrome on this machine'); return null; }
  const p = await browser.newPage();
  await p.navigate(server.url + path);
  return p;
}

const run = (selector = '#t', options = {}) => `(async () => {
  const m = await import('/web/dist/canon.js');
  try {
    const r = await m.canonicalize(document.querySelector(${JSON.stringify(selector)}), ${JSON.stringify(options)});
    return { ok: true, verified: r.verified, warnings: r.warnings, html: r.html, document: r.document, css: r.css, elements: r.elements };
  } catch (e) { return { ok: false, error: e.message }; }
})()`;

const leftovers = `document.querySelectorAll('iframe').length`;

test('ancestor transforms come back under a CSP that refuses style attributes', async (t) => {
  const p = await open(t, '/rotated.html');
  if (!p) return;
  const before = await p.eval(`getComputedStyle(document.getElementById('wrap')).transform`);
  assert.notEqual(before, 'none');
  const r = await p.eval(run());
  assert.ok(r.ok, r.error);
  assert.equal(await p.eval(`getComputedStyle(document.getElementById('wrap')).transform`), before);
  assert.equal(await p.eval(`document.getElementById('wrap').style.cssText`), 'transform: rotate(10deg);');
  assert.equal(await p.eval(leftovers), 0);
  await p.close();
});

test('page rules cannot shrink or hide the rendering frame', async (t) => {
  const p = await open(t, '/narrow.html');
  if (!p) return;
  const r = await p.eval(run());
  assert.ok(r.ok, r.error);
  assert.equal(r.verified, true, r.warnings.join('; '));
  assert.equal(await p.eval(leftovers), 0);
  await p.close();
});

test('Trusted Types with only our policy name allowed: works, and works again', async (t) => {
  const p = await open(t, '/tt-named.html');
  if (!p) return;
  for (let i = 0; i < 2; i++) {
    const r = await p.eval(run());
    assert.ok(r.ok, 'call ' + (i + 1) + ': ' + r.error);
    assert.equal(r.verified, true, r.warnings.join('; '));
  }
  assert.equal(await p.eval(leftovers), 0);
  await p.close();
});

test('Trusted Types with no policy allowed: falls back to a blob: frame', async (t) => {
  const p = await open(t, '/tt-none.html');
  if (!p) return;
  const r = await p.eval(run());
  assert.ok(r.ok, r.error);
  assert.equal(r.verified, true, r.warnings.join('; '));
  assert.equal(await p.eval(leftovers), 0);
  await p.close();
});

test('lazy-loading attributes and the page\'s control of its animations survive', async (t) => {
  const p = await open(t, '/lazy.html');
  if (!p) return;
  const r = await p.eval(run());
  assert.ok(r.ok, r.error);
  assert.equal(await p.eval(`document.getElementById('img').getAttribute('loading')`), 'lazy');
  // The page can still pause its own CSS animation.
  const state = await p.eval(`(() => { document.body.classList.add('paused'); return document.getAnimations()[0].playState; })()`);
  assert.equal(state, 'paused');
  const again = await p.eval(`(() => { document.body.classList.remove('paused'); return document.getAnimations()[0].playState; })()`);
  assert.equal(again, 'running');
  assert.equal(await p.eval(`document.adoptedStyleSheets.length`), 0);
  await p.close();
});

test('a textarea value starting with a newline keeps it in the markup', async (t) => {
  const p = await open(t, '/textarea.html');
  if (!p) return;
  const r = await p.eval(run());
  assert.ok(r.ok, r.error);
  const textareaValue = await p.eval(`(() => { const d = new DOMParser().parseFromString(${JSON.stringify(r.document)}, 'text/html'); return d.querySelector('textarea').value; })()`);
  assert.equal(textareaValue, '\nthird line');
  await p.close();
});

test('markup the parser would rebuild is not reported as verified', async (t) => {
  const p = await open(t, '/table.html');
  if (!p) return;
  const r = await p.eval(run('#t', { reduce: false }));
  assert.ok(r.ok, r.error);
  assert.equal(r.verified, false);
  assert.ok(r.warnings.some((w) => /parser|parses/.test(w)), r.warnings.join('; '));
  await p.close();
});

test('a web font with no @font-face rule is reported, not silently dropped', async (t) => {
  const p = await open(t, '/fontface.html');
  if (!p) return;
  await p.eval('document.fonts.ready');
  const loaded = await p.eval(`[...document.fonts].filter((f) => f.status === 'loaded').length`);
  if (!loaded) return t.skip('the probe font did not load');
  const r = await p.eval(run());
  assert.ok(r.ok, r.error);
  assert.equal(r.verified, false);
  assert.ok(r.warnings.some((w) => /Probe/.test(w)), r.warnings.join('; '));
  await p.close();
});

test('a limited-quirks page keeps its doctype', async (t) => {
  const p = await open(t, '/limited.html');
  if (!p) return;
  const r = await p.eval(run());
  assert.ok(r.ok, r.error);
  assert.match(r.document, /^<!DOCTYPE html PUBLIC "-\/\/W3C\/\/DTD XHTML 1\.0 Transitional\/\/EN"/);
  assert.equal(r.verified, true, r.warnings.join('; '));
  await p.close();
});

test('CSS zoom on the host: verified, and no junk declarations', async (t) => {
  const p = await open(t, '/zoom.html');
  if (!p) return;
  const r = await p.eval(run());
  assert.ok(r.ok, r.error);
  assert.equal(r.verified, true, r.warnings.join('; '));
  assert.doesNotMatch(r.html + r.css, /rule-width/);
  await p.close();
});

test('a wrong wasm path is not blamed on the page\'s security policy', async (t) => {
  const p = await open(t, '/bad-wasm.html');
  if (!p) return;
  const r = await p.eval(`(async () => {
    const m = await import('/web/dist/canon.js?fresh');
    try { await m.canonicalize(document.querySelector('#t'), { wasm: '/bad-wasm.html' }); return 'no error'; } catch (e) { return e.message; }
  })()`);
  assert.doesNotMatch(r, /Content-Security-Policy/);
  assert.match(r, /not the canon WebAssembly module|could not load/);
  await p.close();
});
