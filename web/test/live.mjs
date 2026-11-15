// Manual check on real pages: node web/test/live.mjs <url> [selector] ...
// Imports the in-page build from a local server (with CORS) into each page, runs it, and
// compares the result with the original by screenshot, the original isolated as the CLI does.
import { decodePng, diffPixels, launch, serve } from './cdp.mjs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '../..');
const srv = await serve(root, { headers: { 'access-control-allow-origin': '*', 'cross-origin-resource-policy': 'cross-origin' } });
const b = await launch();
const args = process.argv.slice(2).map((a) => a.replace('{local}', srv.url));
for (let i = 0; i < args.length; i++) {
  const url = args[i];
  const sel = args[i + 1] && !/^(https?|file|data):/.test(args[i + 1]) ? args[++i] : null;
  const page = await b.newPage();
  await page.serveAs('/__canon/', join(root, 'web/dist'));
  if (process.env.BYPASS_CSP) await page.call('Page.setBypassCSP', { enabled: true });
  try {
    await page.navigate(url, { timeout: 45000 });
    const res = await page.eval(`(async () => {
      const m = await import(new URL('/__canon/canon.js', location.href).href);
      const target = ${JSON.stringify(sel)} ? document.querySelector(${JSON.stringify(sel)}) : null;
      const t0 = performance.now();
      const r = await m.canonicalize(target, { timeLimit: ${process.env.LIMIT || 3000} });
      const el = target || (document.body.children.length === 1 ? document.body.firstElementChild : document.body);
      const q = el.getBoundingClientRect();
      return { elements: r.elements, sourceElements: r.sourceElements, verified: r.verified, warnings: r.warnings, ms: performance.now() - t0, rect: [q.left, q.top, q.width, q.height], document: r.document, trials: r.stats.trials };
    })()`);
    const [x, y, w, h] = res.rect;
    const clip = { x: Math.max(0, Math.floor(x)), y: Math.max(0, Math.floor(y)), width: Math.min(Math.ceil(w), 4000), height: Math.min(Math.ceil(h), 8000) };
    const bg = (res.document.match(/<body style="margin:0;background:([^"]+)"/) || [])[1] || '#fff';
    await page.eval(`(() => {
      const root = ${JSON.stringify(sel)} ? document.querySelector(${JSON.stringify(sel)}) : (document.body.children.length === 1 ? document.body.firstElementChild : document.body);
      root.setAttribute('data-iso-root', '');
      for (let a = root.parentElement; a; a = a.parentElement) a.setAttribute('data-iso-anc', '');
      const s = new CSSStyleSheet();
      s.replaceSync(':not([data-iso-root], [data-iso-root] *) { visibility: hidden !important; } [data-iso-root] { visibility: visible !important; } [data-iso-anc] { background: none !important; } :root { background: ${bg} !important; }');
      document.adoptedStyleSheets = [...document.adoptedStyleSheets, s];
      return new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    })()`);
    const A = decodePng(await page.screenshot(clip));
    const copy = await b.newPage();
    await copy.navigate('data:text/html;charset=utf-8,' + encodeURIComponent(res.document));
    await copy.eval(`(async () => { document.body.style.padding = '${y}px 0 0 ${x}px'; await document.fonts.ready; await Promise.all([...document.images].map((i) => i.decode().catch(() => {}))); await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))); })()`);
    const B = decodePng(await copy.screenshot(clip));
    await copy.close();
    console.log(`${url}${sel ? ' ' + sel : ''}: ${res.sourceElements} -> ${res.elements} elements, ${res.trials} trials, ${Math.round(res.ms)} ms, verified=${res.verified}, ${diffPixels(A, B)} of ${A.width * A.height} px differ`);
    for (const w of res.warnings) console.log('   note: ' + w);
  } catch (e) {
    console.log(`${url}: FAILED ${e.message.split('\n')[0]}`);
    for (const l of page.logs.slice(-3)) console.log('   log: ' + l.slice(0, 200));
  }
  await page.close();
}
b.close();
await srv.close();
