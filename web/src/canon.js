// canon in the page: recreate a live element with the fewest elements and the least CSS.
//
//   import { canonicalize } from './canon.js';
//   const result = await canonicalize(document.querySelector('.card'));
//   result.fragment   // <style>…</style><div …>…</div>
//
// The same Rust engine as the `canon` CLI runs here as WebAssembly. Where the CLI uses a
// second Chrome tab to try layouts, this uses a hidden same-origin iframe, sized like the
// host's viewport. Trials are built with DOM calls and the CSSOM, never markup, so they
// also work on pages whose Content-Security-Policy or Trusted Types refuse `innerHTML`,
// style attributes and <style> elements. What it cannot do in-page is take screenshots:
// the result is checked by layout (every box and line of text where the original had it)
// and by reading every computed style back, not pixel by pixel.

import init, { Canon } from './canon_core.js';

// The page agent (agent/agent.js), inlined by web/bundle.mjs. `window` is shadowed so the
// agent does not install itself on the host page.
const createCanonAgent = (function (window) {
  /*@AGENT@*/
  return createCanonAgent;
})(undefined);

let ready = null;

/** Load the WebAssembly engine once. `wasm` overrides where `canon_core_bg.wasm` comes from. */
export function load(wasm) {
  if (!ready) {
    ready = (wasm === undefined ? init() : init({ module_or_path: wasm })).catch((e) => {
      ready = null;
      const msg = String(e && e.message ? e.message : e);
      if (/Content Security Policy|wasm-unsafe-eval|unsafe-eval/i.test(msg)) {
        throw new Error("canon: this page's Content-Security-Policy does not allow WebAssembly (it needs 'wasm-unsafe-eval' in script-src); run the canon CLI on it instead");
      }
      if (e && e.name === 'CompileError') {
        throw new Error('canon: the file loaded as canon_core_bg.wasm is not the canon WebAssembly module (wrong path, or an error page?): ' + msg);
      }
      throw new Error('canon: could not load the WebAssembly engine: ' + msg);
    });
  }
  return ready;
}

const DEFAULTS = {
  style: 'auto', //       'auto' | 'inline' | 'classes'
  reduce: true, //        remove and merge elements (false: only minimise the CSS)
  simplify: true, //      drop declarations that cannot change the picture
  fluid: false, //        leave the width to the container instead of fixing it
  timeLimit: 3000, //     ms the reduction may take; what is verified by then is kept
  maxTrials: 20000,
  embedFonts: true, //    inline the web fonts the element uses into the result
  classPrefix: '',
  wasm: undefined, //     URL, Response or bytes of canon_core_bg.wasm (default: next to this module)
  settle: true, //        wait for images, fonts and DOM churn before capturing
};

/**
 * Recreate `target` (an element, or a selector) with the fewest elements and the least CSS.
 * Resolves to `{ html, css, fragment, document, tree, elements, sourceElements,
 * declarations, verified, verification, stats, warnings }`.
 */
export async function canonicalize(target, options = {}) {
  const o = { ...DEFAULTS, ...options };
  await load(o.wasm);

  const hostDoc = target && target.ownerDocument ? target.ownerDocument : document;
  const win = hostDoc.defaultView;
  if (!win) throw new Error('canon: the element is not in a displayed document (detached, or from a parsed document)');
  const host = createCanonAgent(win);
  const warnings = [];
  let frame = null;
  try {
    if (o.settle) {
      const p = await host.prepare(target);
      if (p.error) throw new Error('canon: ' + p.error);
    }
    const cfg = JSON.parse(Canon.agentConfig());
    const snapshot = host.capture(target, cfg);
    if (snapshot.startsWith('{"error"')) throw new Error('canon: ' + JSON.parse(snapshot).error);
    host.resume();

    const canon = new Canon(snapshot, JSON.stringify({
      simplify: o.simplify, reduce: o.reduce, fluid: o.fluid, maxTrials: o.maxTrials,
    }));
    const info = JSON.parse(canon.info());

    // ---- the rendering frame
    const stage = JSON.parse(canon.stage());
    frame = await createFrame(win, info, stage.doctype);
    const fwin = frame.contentWindow;
    const replica = createCanonAgent(fwin);
    replica.init({ names: JSON.parse(canon.props()), opaque: cfg.opaque });
    const lost = await shareResources(win, hostDoc, fwin, snapshot);
    if (lost.length) warnings.push('not available to the copy (layout may differ): ' + lost.slice(0, 5).join(', ') + (lost.length > 5 ? ', …' : ''));

    // ---- the browser's own defaults, measured in the frame
    canon.setProbe(replica.probe(JSON.parse(canon.probeKeys())));
    const tests = canon.inheritTests();
    canon.setInherit(replica.inherit(JSON.parse(tests)));
    canon.setFollow(replica.follow(JSON.parse(canon.followClasses()), JSON.parse(tests)));
    canon.prepare();

    // ---- reduce, with the frame as the judge
    const render = (docJson) => replica.renderTree(JSON.parse(docJson));
    const off = canon.calibrate(render);
    if (off) warnings.push(off + ' node(s) of the faithful copy could not be placed exactly where the original had them');
    await tick(win);
    if (o.reduce) canon.reduce(render, Date.now() + o.timeLimit);

    // ---- fonts for the finished document
    let fontRules = '';
    if (o.embedFonts) {
      const fonts = await fontCss(win, hostDoc, host);
      if (fonts.failed) warnings.push(fonts.failed + ' web font file(s) could not be read and stay referenced by URL');
      fontRules = fonts.css;
    } else {
      fontRules = (await fontCss(win, hostDoc, host, false)).css;
    }
    canon.setFontCss(fontRules);
    // Trials were laid out with the page's own font faces. A face the result carries no
    // @font-face rule for (one added from script, or from another origin's stylesheet)
    // will not be there when the result is used elsewhere.
    const missingFonts = usedFamilies(host.loadedFonts(), snapshot).filter((f) => !fontRules.toLowerCase().includes(f.toLowerCase()));
    if (missingFonts.length) warnings.push('web font(s) used without a readable @font-face rule, so the result names them but cannot load them: ' + missingFonts.join(', '));

    // ---- emit, and check the result the way it will be used
    let best = null;
    for (const [hoist, fold] of [[true, true], [true, false], [false, true], [false, false]]) {
      const out = JSON.parse(canon.emit(o.style, hoist, fold, o.classPrefix));
      const measured = replica.renderTree({ tree: out.tree, css: out.css, ...stage });
      const verdict = JSON.parse(canon.check(measured, replica.styles()));
      const ok = verdict.structure && verdict.geometry.length === 0;
      const clean = ok && verdict.styles.length === 0;
      const rank = (ok ? 0 : 1e9) + verdict.styles.length;
      if (!best || rank < best.rank) best = { out, verdict, rank, ok };
      if (clean) break;
    }
    const report = JSON.parse(canon.report());
    const { out, verdict } = best;
    // The tree was checked; the markup is what is delivered. Parse it where the page allows
    // and make sure it builds the same tree.
    const parsed = parsedElements(win, out.document, cfg.opaque);
    if (parsed !== null && parsed !== out.elements) {
      verdict.structure = false;
      warnings.push('the returned markup parses into ' + parsed + ' elements, not ' + out.elements);
    }
    warnings.push(...report.warnings);
    if (verdict.markupProblems) warnings.push(verdict.markupProblems + ' element(s) or text run(s) sit where the HTML parser would move them (inside a table or <select>)');
    else if (!verdict.structure) warnings.push('the browser built the result differently from what was written');
    if (verdict.geometry.length) warnings.push(verdict.geometry.length + ' box(es) of the result are not where the original had them');
    if (verdict.styles.length) warnings.push(verdict.styles.length + ' computed value(s) of the result read back differently from their target');
    const css = out.fontCss + out.css;
    return {
      html: out.html,
      css,
      fragment: (css ? '<style>\n' + css + '</style>\n' : '') + out.html,
      document: out.document,
      tree: out.tree,
      elements: out.elements,
      sourceElements: report.sourceElements,
      declarations: out.declarations,
      verified: best.ok && verdict.structure && verdict.styles.length === 0 && off === 0 && missingFonts.length === 0,
      verification: verdict,
      stats: report.stats,
      warnings,
    };
  } finally {
    host.resume();
    if (frame) frame.remove();
  }
}

// ------------------------------------------------------------------ the frame

const SHELL = '<html><head><meta charset="utf-8"></head><body></body></html>';

// A same-origin iframe the size of the host's viewport, laid out but invisible. Its
// document is in the same mode (standards, limited quirks or quirks) as the page the
// element came from: `doctype` is the one that mode needs ("" for quirks).
async function createFrame(win, info, doctype) {
  const doc = win.document;
  const f = doc.createElement('iframe');
  f.setAttribute('aria-hidden', 'true');
  f.tabIndex = -1;
  // Never display:none or 0x0: an iframe that is not laid out reports no boxes, and one
  // without a size has no viewport. !important, so the page's own iframe rules cannot
  // resize or hide it.
  const w = Math.round(info.viewport[0]), h = Math.round(info.viewport[1]);
  const css = {
    position: 'fixed', left: '-100000px', top: '0px', border: '0', margin: '0', padding: '0', display: 'block',
    visibility: 'hidden', 'pointer-events': 'none', 'box-sizing': 'content-box', transform: 'none',
    width: w + 'px', height: h + 'px', 'min-width': '0', 'min-height': '0', 'max-width': 'none', 'max-height': 'none',
  };
  for (const k in css) f.style.setProperty(k, css[k], 'important');
  let blobUrl = null;
  try {
    const quirks = doctype === '';
    if (!quirks) {
      // An about:blank document is always in quirks mode; srcdoc documents are always in
      // no-quirks mode, whatever their doctype. A limited-quirks doctype therefore needs a
      // document parsed on its own: a blob: URL.
      const loaded = new Promise((r) => f.addEventListener('load', r, { once: true }));
      const limited = doctype !== '<!doctype html>';
      const html = limited ? null : trustedHtml(win, doctype + SHELL);
      if (html !== null) f.srcdoc = html;
      else {
        // Trusted Types without a policy we may create: a blob: document is not a sink.
        blobUrl = win.URL.createObjectURL(new win.Blob([doctype + SHELL], { type: 'text/html' }));
        f.src = blobUrl;
      }
      doc.documentElement.appendChild(f);
      await loaded;
    } else {
      doc.documentElement.appendChild(f);
    }
    // CSS zoom on the host leaks into the frame and changes text layout: cancel it.
    const z = f.currentCSSZoom;
    if (z && Math.abs(z - 1) > 1e-6) f.style.setProperty('zoom', String(1 / z), 'important');
    const fw = f.contentWindow;
    const fd = f.contentDocument;
    if (!fd || !fd.body) throw new Error("canon: the page's security policy does not let it use a same-origin frame" + (blobUrl ? ' (it refuses blob: documents, which ' + (doctype !== '<!doctype html>' ? 'a limited-quirks page needs' : 'Trusted Types without an allowed policy need') + ')' : ''));
    if (quirks !== (fd.compatMode === 'BackCompat')) throw new Error('canon: could not create a rendering frame in ' + (quirks ? 'quirks' : 'standards') + ' mode');
    if (fw.innerWidth !== w || fw.innerHeight !== h) throw new Error('canon: the rendering frame did not get the viewport size (' + fw.innerWidth + 'x' + fw.innerHeight + ' instead of ' + w + 'x' + h + ')');
    // An explicit check that the CSSOM is honoured here (a policy could still refuse it).
    const probe = fd.createElement('div');
    probe.style.cssText = 'width:37px;height:1px';
    fd.body.appendChild(probe);
    const pw = probe.getBoundingClientRect().width;
    probe.remove();
    if (pw !== 37) throw new Error("canon: styles set through the CSSOM are not applied in this page's frames");
    return f;
  } catch (e) {
    f.remove();
    throw e;
  } finally {
    if (blobUrl) win.URL.revokeObjectURL(blobUrl);
  }
}

// The frame's markup as Trusted Types want it. null: Trusted Types are enforced and no
// policy can be created (the caller then avoids the sink).
const policies = new WeakMap();
function trustedHtml(win, s) {
  const tt = win.trustedTypes;
  if (!tt || !tt.createPolicy) return s;
  if (!policies.has(win)) {
    let p = null;
    try {
      p = tt.createPolicy('canon-frame', { createHTML: (x) => (x.endsWith(SHELL) ? x : '') });
    } catch (e) { /* not allowed, or the name is taken */ }
    policies.set(win, p);
  }
  const p = policies.get(win);
  if (p) return p.createHTML(s);
  // No policy: plain strings work unless Trusted Types are actually enforced.
  return enforced(win) ? null : s;
}

function enforced(win) {
  try {
    const t = win.document.createElement('template');
    t.innerHTML = ''; // throws only when Trusted Types are required for HTML sinks
    return false;
  } catch (e) {
    return true;
  }
}

// Number of elements the HTML parser makes of `document` under <body>, or null where the
// page forbids parsing strings (Trusted Types).
// (Counted like the agent's measure(): the inside of opaque elements, such as a <select>'s
// options, is part of that element.)
function parsedElements(win, documentHtml, opaque) {
  if (enforced(win)) return null;
  try {
    const d = new win.DOMParser().parseFromString(documentHtml, 'text/html');
    if (!d.body) return null;
    const skip = new Set(opaque);
    let n = 0;
    const walk = (p) => {
      for (let c = p.firstElementChild; c; c = c.nextElementSibling) {
        n++;
        if (!(c.namespaceURI === 'http://www.w3.org/1999/xhtml' && skip.has(c.localName))) walk(c);
      }
    };
    walk(d.body);
    return n;
  } catch (e) {
    return null;
  }
}

// Families of the loaded web fonts that the captured element actually names.
function usedFamilies(loaded, snapshot) {
  const data = JSON.parse(snapshot);
  const fi = data.props.indexOf('font-family');
  if (fi < 0) return [];
  const named = new Set();
  for (const st of data.styles) named.add(data.values[st[fi]].toLowerCase());
  const out = [];
  for (const f of loaded) {
    const fam = f.family.replace(/^["']|["']$/g, '');
    const needle = fam.toLowerCase();
    if (!out.includes(fam) && [...named].some((v) => v.includes(needle))) out.push(fam);
  }
  return out;
}

// Fonts and images the copy needs, made available to the frame before any trial runs, so
// every trial can be laid out synchronously. Returns what could not be made available.
async function shareResources(win, hostDoc, fwin, snapshot) {
  const fdoc = fwin.document;
  const lost = [];
  for (const face of hostDoc.fonts) {
    if (face.status !== 'loaded') continue;
    try { fdoc.fonts.add(face); } catch (e) { lost.push('font ' + face.family); }
  }
  try { await fdoc.fonts.ready; } catch (e) { /* ignore */ }

  // Every image URL the snapshot mentions: <img src>, SVG <image href>, url() in styles.
  const urls = new Set();
  const data = JSON.parse(snapshot);
  for (const n of data.nodes) {
    for (const [k, v] of n.a || []) {
      if ((k === 'src' || k === 'href' || k === 'xlink:href' || k === 'poster') && v && !v.startsWith('#') && !v.startsWith('javascript:')) urls.add(v);
    }
  }
  for (const v of data.values) {
    if (v.indexOf('url(') < 0) continue;
    for (const m of v.matchAll(/url\((["']?)(.*?)\1\)/g)) if (m[2] && m[2][0] !== '#') urls.add(m[2]);
  }
  const keep = (fwin.__canonImages = []);
  await Promise.all([...urls].map(async (u) => {
    const img = new fwin.Image();
    img.src = u;
    keep.push(img); // held for the frame's lifetime, so trials find it decoded in memory
    try { await Promise.race([img.decode(), new Promise((_, rej) => win.setTimeout(rej, 8000))]); } catch (e) {
      if (!/^data:/.test(u)) lost.push(u.length > 80 ? u.slice(0, 77) + '...' : u);
    }
  }));
  return lost;
}

// @font-face rules for the finished document, with the font data inlined where the files
// can be read.
async function fontCss(win, hostDoc, host, embed = true) {
  const loaded = host.loadedFonts();
  if (!loaded.length) return { css: '', failed: 0 };
  const sheets = [];
  const roots = [hostDoc];
  for (const el of hostDoc.querySelectorAll('*')) if (el.shadowRoot) roots.push(el.shadowRoot);
  for (const root of roots) {
    for (const sh of [...root.styleSheets, ...(root.adoptedStyleSheets || [])]) collectFaces(sh, sh.href || hostDoc.baseURI, sheets);
  }
  if (!sheets.length) return { css: '', failed: 0 };
  const sheetsJson = JSON.stringify(sheets), loadedJson = JSON.stringify(loaded);
  const urls = JSON.parse(Canon.fontUrls(sheetsJson, loadedJson));
  const fetched = {};
  let failed = 0;
  await Promise.all((embed ? urls : []).map(async (u) => {
    try {
      const r = await win.fetch(u, { cache: 'force-cache', credentials: 'same-origin' });
      if (!r.ok) throw new Error(String(r.status));
      fetched[u] = toBase64(new Uint8Array(await r.arrayBuffer()));
    } catch (e) { failed++; }
  }));
  return { css: JSON.parse(Canon.fontCss(sheetsJson, loadedJson, JSON.stringify(fetched))).css, failed };
}

function collectFaces(sheet, base, out) {
  let rules;
  try { rules = sheet.cssRules; } catch (e) { return; } // another origin's sheet: not readable
  for (const r of rules) {
    const k = (r.constructor && r.constructor.name) || '';
    if (k === 'CSSFontFaceRule') out.push([r.cssText, base]);
    else if (k === 'CSSImportRule') { if (r.styleSheet) collectFaces(r.styleSheet, r.styleSheet.href || base, out); }
    else if (r.cssRules) collectFaces({ cssRules: r.cssRules }, base, out);
  }
}

function toBase64(bytes) {
  let s = '';
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
  return btoa(s);
}

// Let the page breathe between long synchronous steps.
function tick(win) {
  return new Promise((r) => {
    const ch = new win.MessageChannel();
    ch.port1.onmessage = () => r();
    ch.port2.postMessage(0);
  });
}
