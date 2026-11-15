// The page agent: reports facts about a DOM; every decision is made on the Rust side.
//
// `createCanonAgent(win)` returns an agent bound to one window. It plays one of two roles:
//   * source  - prepare(), capture(), isolate(), loadedFonts(), fetchB64()
//   * replica - init(), probe(), inherit(), follow(), render()/renderTree(), measure(),
//               offset(), styles(), settle()
// The CLI evaluates this file in each Chrome tab, which installs `window.__canon`. The
// in-page build (web/) creates one agent for the host window and one for a hidden iframe.
//
// Everything goes through `win` and `win.document`, never the globals of whatever realm
// this code was loaded in, and no `instanceof` is used on DOM objects: an object from
// another frame is an instance of *that* frame's constructors.
var createCanonAgent = function createCanonAgent(win) {
  'use strict';

  const HTML = 'http://www.w3.org/1999/xhtml';
  const SVG = 'http://www.w3.org/2000/svg';
  const MATHML = 'http://www.w3.org/1998/Math/MathML';
  const XLINK = 'http://www.w3.org/1999/xlink';
  const NS = [HTML, SVG, MATHML];
  // Logical properties duplicate the physical ones; keep the physical set only.
  const LOGICAL = /(^|-)(block|inline)(-|$)|^(border|corner)-(start|end)-(start|end)-(radius|shape)$/;
  const SVG_SKIP = new Set(['script', 'style', 'title', 'desc', 'metadata']);
  const URL_ATTRS = new Set(['src', 'poster', 'data', 'href', 'xlink:href']);
  const Q = 64; // rect units per CSS px (Blink's LayoutUnit)
  const q = (v) => Math.round(v * Q);

  const D = () => win.document;
  const gcs = (el, pseudo) => win.getComputedStyle(el, pseudo);

  let NAMES = [];
  let OPAQUE = new Set();
  let ROOT = null;

  class Interner {
    constructor() { this.map = new Map(); this.list = []; }
    id(s) {
      let i = this.map.get(s);
      if (i === undefined) { i = this.list.length; this.list.push(s); this.map.set(s, i); }
      return i;
    }
  }

  // Type checks that work across frames.
  const kind = (o) => (o && o.constructor && o.constructor.name) || '';
  const isShadowRoot = (n) => !!n && n.nodeType === 11 && !!n.host;
  const isElement = (n) => !!n && n.nodeType === 1;

  function allNames(cfg) {
    const cs = gcs(D().documentElement), out = [];
    const skip = new Set(cfg.skip);
    for (let i = 0; i < cs.length; i++) {
      const n = cs[i];
      if (n.startsWith('--') || LOGICAL.test(n) || skip.has(n)) continue;
      if (cfg.skipPrefix.some((p) => n.startsWith(p))) continue;
      out.push(n);
    }
    return out;
  }

  // True computed values (keeps `auto`, percentages, `repeat(3, 1fr)`...), unlike
  // getComputedStyle which resolves layout-dependent properties to used pixels.
  function readComputed(el) {
    const m = el.computedStyleMap(), out = new Array(NAMES.length);
    for (let i = 0; i < NAMES.length; i++) {
      let v = '';
      try {
        const a = m.getAll(NAMES[i]);
        v = a.length === 1 ? a[0].toString() : a.join(', ');
      } catch (e) { /* property not exposed through Typed OM */ }
      out[i] = v;
    }
    return out;
  }

  function readResolved(el, pseudo) {
    const cs = gcs(el, pseudo), out = new Array(NAMES.length);
    for (let i = 0; i < NAMES.length; i++) out[i] = cs.getPropertyValue(NAMES[i]);
    return out;
  }

  function rectsOf(list) {
    const out = [];
    for (const r of list) out.push(q(r.left), q(r.top), q(r.width), q(r.height));
    return out;
  }

  const escText = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;');

  function flatParent(el) {
    if (el.assignedSlot) return el.assignedSlot;
    if (el.parentElement) return el.parentElement;
    const r = el.getRootNode();
    return isShadowRoot(r) ? r.host : null;
  }

  function flatChildren(el) {
    if (el.shadowRoot) return el.shadowRoot.childNodes;
    if (el.localName === 'slot' && el.namespaceURI === HTML && isShadowRoot(el.getRootNode())) {
      const a = el.assignedNodes({ flatten: true });
      if (a.length) return a;
    }
    return el.childNodes;
  }

  function hasBox(el) {
    const d = gcs(el).display;
    return d !== 'none' && d !== 'contents' && el.checkVisibility();
  }

  // `target`: an element, a selector, or nothing (the page's single top-level element,
  // else <body>).
  function pickRoot(target) {
    const doc = D();
    if (isElement(target)) return target;
    if (target) return doc.querySelector(target);
    const body = doc.body;
    if (!body) return doc.documentElement;
    const range = doc.createRange();
    let only = null, count = 0;
    for (const n of body.childNodes) {
      if (n.nodeType === 1) {
        if (gcs(n).display === 'contents') return body;
        if (hasBox(n)) { count++; only = n; }
      } else if (n.nodeType === 3) {
        range.selectNodeContents(n);
        if (range.getClientRects().length) return body;
      }
    }
    return count === 1 ? only : body;
  }

  const describe = (target) => (isElement(target) ? 'the given element' : JSON.stringify(target));

  // Colour of whatever is painted behind the root, flattened onto white.
  function backdropOf(root) {
    const chain = [];
    for (let a = flatParent(root); a; a = flatParent(a)) chain.push(a);
    const cv = D().createElement('canvas');
    cv.width = cv.height = 1;
    const ctx = cv.getContext('2d', { willReadFrequently: true });
    ctx.fillStyle = '#fff';
    ctx.fillRect(0, 0, 1, 1);
    let complex = false;
    for (let i = chain.length - 1; i >= 0; i--) {
      const cs = gcs(chain[i]);
      if (cs.backgroundImage !== 'none') complex = true;
      ctx.fillStyle = cs.backgroundColor;
      ctx.fillRect(0, 0, 1, 1);
    }
    const d = ctx.getImageData(0, 0, 1, 1).data;
    return { color: [d[0], d[1], d[2]], complex };
  }

  // Which properties do the page's rules set on ::before / ::after? A pseudo-element's style
  // can only be read resolved (used pixels), so this is what tells `width: auto` from a
  // width that merely came out at some number of pixels.
  // Returns per root node (document or shadow root): { rules: [[selector, which, names]], complete }.
  const pseudoIndex = new Map();
  function pseudoRulesFor(root) {
    if (pseudoIndex.has(root)) return pseudoIndex.get(root);
    const out = { rules: [], complete: true };
    pseudoIndex.set(root, out);
    // The pseudo-element, with whatever follows it (`::before:where()`, `::after:hover`):
    // such a rule is counted whenever its element matches, which errs on the safe side.
    const TAIL = /::?(before|after)(?![\w-])(?!.*::?(before|after)(?![\w-]))/i;
    const split = (text) => {
      const parts = [];
      let depth = 0, start = 0, quote = '';
      for (let i = 0; i < text.length; i++) {
        const c = text[i];
        if (quote) { if (c === quote) quote = ''; continue; }
        if (c === '"' || c === "'") quote = c;
        else if (c === '(' || c === '[') depth++;
        else if (c === ')' || c === ']') depth--;
        else if (c === ',' && depth === 0) { parts.push(text.slice(start, i)); start = i + 1; }
      }
      parts.push(text.slice(start));
      return parts.map((p) => p.trim()).filter(Boolean);
    };
    const walk = (list, parentSel) => {
      for (const rule of list) {
        const k = kind(rule);
        if (k === 'CSSStyleRule') {
          const mentions = /before|after/i.test(rule.selectorText);
          const full = [];
          for (let part of split(rule.selectorText)) {
            if (parentSel) part = part.includes('&') ? part.replaceAll('&', ':is(' + parentSel + ')') : ':is(' + parentSel + ') ' + part;
            full.push(part);
            if (!mentions) continue;
            const m = part.match(TAIL);
            if (!m) continue;
            // Inside a functional selector, or reaching across a shadow boundary: not handled.
            const before = part.slice(0, m.index);
            const open = (before.match(/\(/g) || []).length - (before.match(/\)/g) || []).length;
            if (open !== 0 || /:host|::slotted|::part/i.test(part)) { out.complete = false; continue; }
            const names = [];
            for (let i = 0; i < rule.style.length; i++) names.push(rule.style[i]);
            out.rules.push([part.slice(0, m.index).trim() || '*', m[1].toLowerCase() === 'before' ? 0 : 1, names]);
          }
          if (rule.cssRules && rule.cssRules.length) walk(rule.cssRules, full.join(', '));
        } else if (k === 'CSSImportRule') {
          try { if (rule.styleSheet) walk(rule.styleSheet.cssRules, parentSel); } catch (e) { out.complete = false; }
        } else if (rule.cssRules) {
          // A rule in a media query that does not apply sets nothing; other conditions are
          // assumed to hold (the safe side: the pixel value is then kept).
          if (k === 'CSSMediaRule' && !win.matchMedia(rule.conditionText).matches) continue;
          if (k === 'CSSScopeRule') { if (/before|after/i.test(rule.cssText)) out.complete = false; continue; }
          walk(rule.cssRules, parentSel);
        }
      }
    };
    const sheets = [...(root.styleSheets || []), ...(root.adoptedStyleSheets || [])];
    for (const sh of sheets) {
      try { walk(sh.cssRules, ''); } catch (e) { out.complete = false; } // a cross-origin sheet cannot be read
    }
    return out;
  }

  function pseudoDeclared(el, which) {
    const idx = pseudoRulesFor(el.getRootNode());
    if (!idx.complete) return null;
    const names = new Set();
    for (const [sel, w, list] of idx.rules) {
      if (w !== which) continue;
      let hit = false;
      try { hit = el.matches(sel); } catch (e) { return null; }
      if (hit) for (const n of list) names.add(n);
    }
    return [...names];
  }

  async function prepare(target) {
    const doc = D();
    const root = pickRoot(target);
    if (!root) return { error: 'no element matches ' + describe(target) };
    // Lazy images in the subtree are loaded now; `resume()` puts the attribute back.
    for (const img of root.querySelectorAll('img[loading]')) {
      if (img.loading === 'eager') continue;
      LAZY.push([img, img.getAttribute('loading')]);
      img.loading = 'eager';
    }
    try { await doc.fonts.ready; } catch (e) { /* ignore */ }
    const pend = [];
    for (const i of root.querySelectorAll('img')) {
      if (!i.complete) pend.push(new Promise((r) => {
        i.addEventListener('load', r, { once: true });
        i.addEventListener('error', r, { once: true });
      }));
    }
    if (pend.length) await Promise.race([Promise.all(pend), new Promise((r) => win.setTimeout(r, 8000))]);
    // Pages that fill themselves in after load (lazy rows, skeleton placeholders): wait until
    // the DOM has stopped gaining nodes or text for a moment.
    await new Promise((resolve) => {
      const done = () => { mo.disconnect(); win.clearTimeout(quiet); win.clearTimeout(hard); resolve(); };
      let quiet = win.setTimeout(done, 250);
      const hard = win.setTimeout(done, 5000);
      const mo = new win.MutationObserver(() => { win.clearTimeout(quiet); quiet = win.setTimeout(done, 250); });
      mo.observe(doc.documentElement, { subtree: true, childList: true, characterData: true });
    });
    freeze(root);
    return { ok: true };
  }

  // Freeze animations where they stand until `resume()`. CSS animations are paused by a
  // temporary stylesheet rather than through the Web Animations API: calling pause()/play()
  // on a CSS animation detaches it from `animation-play-state` for good. Script-made
  // animations are paused and played.
  let PAUSED = [], FROZEN = [], LAZY = [];
  function freeze(root) {
    const doc = D();
    try {
      const sheet = new win.CSSStyleSheet();
      sheet.replaceSync('*, *::before, *::after, *::marker { animation-play-state: paused !important; }');
      const scopes = [doc];
      const walk = (n) => { for (const el of n.querySelectorAll('*')) if (el.shadowRoot) { scopes.push(el.shadowRoot); walk(el.shadowRoot); } };
      walk(doc);
      for (const sc of scopes) { sc.adoptedStyleSheets = [...sc.adoptedStyleSheets, sheet]; FROZEN.push([sc, sheet]); }
    } catch (e) { /* no constructable sheets: animations keep running */ }
    try {
      for (const a of doc.getAnimations({ subtree: true })) {
        const k = kind(a);
        if (k === 'CSSAnimation' || k === 'CSSTransition') continue;
        if (a.playState === 'running') { a.pause(); PAUSED.push(a); }
      }
    } catch (e) { /* ignore */ }
    void root;
  }

  function resume() {
    for (const [sc, sheet] of FROZEN) {
      try { sc.adoptedStyleSheets = sc.adoptedStyleSheets.filter((s) => s !== sheet); } catch (e) { /* gone */ }
    }
    for (const a of PAUSED) { try { a.play(); } catch (e) { /* removed meanwhile */ } }
    for (const [img, v] of LAZY) { if (v === null) img.removeAttribute('loading'); else img.setAttribute('loading', v); }
    PAUSED = []; FROZEN = []; LAZY = [];
  }

  function attrsFor(el, tag, ns, cfg) {
    const out = [];
    const base = el.baseURI || D().baseURI;
    const abs = (v) => {
      if (!v || v[0] === '#' || /^(data|blob|javascript):/i.test(v)) return v;
      try { return new URL(v, base).href; } catch (e) { return v; }
    };
    if (ns !== 0) {
      for (const at of el.attributes) {
        const n = at.name;
        if (n === 'class' || n === 'style' || n.startsWith('on') || n.startsWith('xmlns') || n.startsWith('data-') || n.startsWith('aria-')) continue;
        out.push([n, URL_ATTRS.has(n) ? abs(at.value) : at.value]);
      }
      return out;
    }
    const allow = cfg.keep[tag];
    if (!allow) return out;
    const dyn = new Map();
    if (tag === 'img') {
      const src = el.currentSrc || el.src;
      if (src) dyn.set('src', src);
      if (!(el.complete && el.naturalWidth > 0) && el.hasAttribute('alt')) dyn.set('alt', el.getAttribute('alt'));
    } else if (tag === 'input') {
      const t = (el.getAttribute('type') || 'text').toLowerCase();
      if (t === 'checkbox' || t === 'radio') { if (el.checked) dyn.set('checked', ''); }
      else if (t !== 'file' && t !== 'password' && el.value) dyn.set('value', el.value);
    } else if (tag === 'video' || tag === 'audio') {
      if (el.currentSrc) dyn.set('src', el.currentSrc);
    } else if (tag === 'progress' || tag === 'meter') {
      if (el.hasAttribute('value')) dyn.set('value', String(el.value));
    }
    for (const name of allow) {
      if (dyn.has(name)) { out.push([name, dyn.get(name)]); continue; }
      if (name === 'alt' || name === 'checked') continue; // only via dyn
      if (tag === 'input' && name === 'value') continue;
      if (el.hasAttribute(name)) {
        const v = el.getAttribute(name);
        out.push([name, URL_ATTRS.has(name) ? abs(v) : v]);
      }
    }
    return out;
  }

  // Transforms on ancestors are neutralised for the duration of `fn` (synchronously, so
  // nothing is painted in between), then put back exactly. The copy is drawn untransformed,
  // so it is measured untransformed. The transform is replaced by an identity transform,
  // not removed, so containing blocks and stacking contexts stay what they were; transitions
  // are suspended so the change starts none. Everything goes through the CSSOM, which a
  // Content-Security-Policy that refuses style attributes still allows.
  const TRANSFORMS = ['transform', 'rotate', 'scale', 'translate'];
  const NEUTRAL = { transform: 'matrix(1, 0, 0, 1, 0, 0)', rotate: 'none', scale: 'none', translate: 'none', transition: 'none' };
  function untransformed(root, fn) {
    const saved = [];
    for (let a = flatParent(root); a; a = flatParent(a)) {
      const cs = gcs(a);
      if (TRANSFORMS.every((p) => { const v = cs.getPropertyValue(p); return !v || v === 'none'; })) continue;
      const had = a.hasAttribute('style');
      const props = Object.keys(NEUTRAL).map((p) => [p, a.style.getPropertyValue(p), a.style.getPropertyPriority(p)]);
      saved.push([a, had, props]);
      for (const p of Object.keys(NEUTRAL)) a.style.setProperty(p, NEUTRAL[p], 'important');
    }
    try {
      return fn(saved.length > 0);
    } finally {
      for (let i = saved.length - 1; i >= 0; i--) {
        const [a, had, props] = saved[i];
        for (const [p, v, prio] of props) {
          if (v) a.style.setProperty(p, v, prio); else a.style.removeProperty(p);
        }
        if (!had && a.style.length === 0) a.removeAttribute('style');
      }
    }
  }

  // Capture `target` (see pickRoot) for the engine. Returns the snapshot as a JSON string.
  function capture(target, cfg) {
    NAMES = allNames(cfg);
    OPAQUE = new Set(cfg.opaque);
    const root = pickRoot(target);
    if (!root) return JSON.stringify({ error: 'no element matches ' + describe(target) });
    if (!hasBox(root)) return JSON.stringify({ error: 'the selected element is not rendered (no box)' });
    ROOT = root;
    const sx = win.scrollX, sy = win.scrollY;
    try {
      return untransformed(root, (transformed) => captureRoot(root, cfg, transformed));
    } finally {
      // Taking transforms off can move scroll anchoring; put the page back where it was.
      if (win.scrollX !== sx || win.scrollY !== sy) win.scrollTo(sx, sy);
    }
  }

  function captureRoot(root, cfg, transformed) {
    const doc = D();
    const vals = new Interner(), styles = [], styleMap = new Map(), nodes = [];
    const range = doc.createRange();
    const info = { scrolled: 0, bidi: false, shadow: 0, canvasTainted: 0, pseudoUnknown: 0 };
    pseudoIndex.clear();
    const RTL = /[֐-ࣿיִ-﷿ﹰ-﻿]/;

    const styleId = (arr) => {
      const ids = new Array(arr.length);
      for (let i = 0; i < arr.length; i++) ids[i] = vals.id(arr[i]);
      const k = ids.join(',');
      let i = styleMap.get(k);
      if (i === undefined) { i = styles.length; styles.push(ids); styleMap.set(k, i); }
      return i;
    };
    const pf = (s) => parseFloat(s) || 0;

    function visitText(t, parent, inSvg) {
      range.selectNodeContents(t);
      const rs = range.getClientRects();
      if (rs.length === 0 && !(inSvg && /\S/.test(t.data))) return;
      if (!info.bidi && RTL.test(t.data)) info.bidi = true;
      nodes.push({ p: parent, x: t.data, r: rectsOf(rs) });
    }

    function visit(el, parent, inSvg) {
      const nsUri = el.namespaceURI;
      const ns = nsUri === SVG ? 1 : nsUri === MATHML ? 2 : 0;
      let tag = el.localName;
      const cs = gcs(el);
      const display = cs.display;
      if (inSvg && ns === 1) {
        if (SVG_SKIP.has(tag)) return;
      } else {
        if (display === 'none') return;
        if (display !== 'contents' && !el.checkVisibility()) return;
      }
      const idx = nodes.length;
      const rec = { p: parent, g: tag };
      nodes.push(rec);
      if (ns) rec.n = ns;
      if (display === 'contents') rec.f = 1;
      rec.s = styleId(readComputed(el));
      rec.r = rectsOf(el.getClientRects());
      if (el.scrollTop || el.scrollLeft) info.scrolled++;

      const w = parseFloat(cs.width), h = parseFloat(cs.height);
      if (!isNaN(w) && !isNaN(h) && !(inSvg && ns === 1)) {
        let bw = w, bh = h;
        if (cs.boxSizing !== 'border-box') {
          bw += pf(cs.paddingLeft) + pf(cs.paddingRight) + pf(cs.borderLeftWidth) + pf(cs.borderRightWidth);
          bh += pf(cs.paddingTop) + pf(cs.paddingBottom) + pf(cs.borderTopWidth) + pf(cs.borderBottomWidth);
        }
        rec.b = [bw, bh];
      }

      // <canvas> becomes an <img> of its current bitmap.
      if (ns === 0 && tag === 'canvas') {
        try {
          const url = el.toDataURL();
          rec.g = tag = 'img';
          rec.a = [['src', url]];
        } catch (e) { info.canvasTainted++; }
      } else {
        const a = attrsFor(el, tag, ns, cfg);
        if (a.length) rec.a = a;
      }

      const opaque = ns === 0 && OPAQUE.has(tag);
      if (!opaque && !(inSvg && ns === 1)) {
        let q2 = null;
        for (let pi = 0; pi < 2; pi++) {
          const ps = pi ? '::after' : '::before';
          const pcs = gcs(el, ps);
          const content = pcs.content;
          if (content && content !== 'none' && content !== 'normal' && pcs.display !== 'none') {
            q2 = q2 || [null, null];
            q2[pi] = { c: content, s: styleId(readResolved(el, ps)) };
            const d = pseudoDeclared(el, pi);
            if (d) q2[pi].d = d; else info.pseudoUnknown++;
          }
        }
        if (q2) {
          rec.q = q2;
          rec.e = styleId(readResolved(el, null));
          // Rules that could not be read from here: mark it so a driver with DevTools access
          // can ask the browser (`cfg.mark`; the in-page build leaves the host DOM alone).
          if (cfg.mark && q2.some((x) => x && !x.d)) el.setAttribute('data-canon-q', String(idx));
        }
      }

      // A placeholder that is showing has a style of its own.
      if (ns === 0 && (tag === 'input' || tag === 'textarea') && el.placeholder && !el.value) {
        rec.h = styleId(readResolved(el, '::placeholder'));
        if (rec.e === undefined) rec.e = styleId(readResolved(el, null));
      }
      if (opaque) {
        if (tag === 'textarea') rec.i = escText(el.value);
        else if (tag === 'select') {
          let s = '';
          for (const o of el.options) s += '<option' + (o.selected ? ' selected' : '') + '>' + escText(o.text) + '</option>';
          rec.i = s;
        }
        return;
      }
      if (el.shadowRoot) info.shadow++;
      const svgCtx = ns === 1 ? tag !== 'foreignObject' : false;
      for (const c of flatChildren(el)) {
        if (c.nodeType === 1) {
          if (svgCtx && c.namespaceURI !== SVG) continue; // HTML inside SVG is not rendered
          visit(c, idx, svgCtx);
        } else if (c.nodeType === 3) visitText(c, idx, svgCtx);
      }
    }
    visit(root, -1, false);

    // Symbols referenced by <use> from outside the captured subtree.
    const extras = [];
    const seen = new Set();
    for (const u of root.querySelectorAll('use')) {
      const href = u.getAttribute('href') || u.getAttribute('xlink:href');
      if (!href || href[0] !== '#') continue;
      const target = (u.getRootNode().getElementById ? u.getRootNode() : doc).getElementById(href.slice(1));
      if (!target || root.contains(target) || seen.has(target)) continue;
      seen.add(target);
      const at = nodes.length;
      visit(target, -2, true);
      if (nodes.length > at) extras.push(at);
    }

    const rr = root.getBoundingClientRect();
    let zoom = 1;
    for (let a = flatParent(root); a; a = flatParent(a)) zoom *= parseFloat(gcs(a).zoom) || 1;
    const par = flatParent(root);
    let overflowPropagated = false;
    if (root === doc.documentElement) overflowPropagated = true;
    else if (root === doc.body) {
      const hcs = gcs(doc.documentElement);
      overflowPropagated = hcs.overflowX === 'visible' && hcs.overflowY === 'visible';
    }
    const de = doc.documentElement;
    Object.assign(info, {
      url: win.location.href,
      title: doc.title,
      compat: doc.compatMode,
      // Public and system ids tell limited-quirks documents (XHTML 1.0 / HTML 4.01
      // Transitional), which compatMode reports as standards, from full standards mode.
      doctype: doc.doctype && (doc.doctype.publicId || doc.doctype.systemId)
        ? '<!DOCTYPE ' + doc.doctype.name + (doc.doctype.publicId ? ' PUBLIC "' + doc.doctype.publicId + '"' : ' SYSTEM') + (doc.doctype.systemId ? ' "' + doc.doctype.systemId + '"' : '') + '>'
        : '',
      viewport: [win.innerWidth, win.innerHeight],
      dpr: win.devicePixelRatio,
      scroll: [win.scrollX, win.scrollY],
      rect: [rr.left, rr.top, rr.width, rr.height],
      rootTag: root.localName,
      domElements: root.querySelectorAll('*').length + 1,
      sourceBytes: root.outerHTML.length,
      zoom, transformed, overflowPropagated,
      parentWidth: par ? par.clientWidth : win.innerWidth,
      dark: win.matchMedia('(prefers-color-scheme: dark)').matches,
      docSize: [Math.max(de.scrollWidth, win.innerWidth), Math.max(de.scrollHeight, win.innerHeight)],
      backdrop: backdropOf(root),
      extras,
    });
    return JSON.stringify({ props: NAMES, values: vals.list, styles, nodes, info });
  }

  // Hide everything that is not the captured subtree so a screenshot shows the
  // element alone. Visibility does not affect layout, so geometry is unchanged.
  // `backdrop` is the flat colour to show behind the element: set on the document element
  // explicitly, because a page that opts into a dark colour scheme gets a dark canvas that
  // no default-background override reaches.
  function isolate(backdrop) {
    const root = ROOT;
    if (!root) return false;
    const doc = D();
    root.setAttribute('data-canon-root', '');
    const css =
      ':not([data-canon-root], [data-canon-root] *) { visibility: hidden !important; }' +
      '[data-canon-root] { visibility: visible !important; }' +
      '[data-canon-anc] { background: none !important; }' +
      (root === doc.documentElement ? '' : ':root { background: ' + backdrop + ' !important; }');
    const sheet = new win.CSSStyleSheet();
    sheet.replaceSync(css);
    const scopes = new Set([doc]);
    for (let a = flatParent(root); a; a = flatParent(a)) {
      a.setAttribute('data-canon-anc', '');
      scopes.add(a.getRootNode());
    }
    scopes.add(root.getRootNode());
    for (const s of scopes) {
      if (s && 'adoptedStyleSheets' in s) s.adoptedStyleSheets = [...s.adoptedStyleSheets, sheet];
    }
    return true;
  }

  function loadedFonts() {
    const out = [];
    for (const f of D().fonts) {
      if (f.status !== 'loaded') continue;
      out.push({ family: f.family, style: f.style, weight: f.weight, stretch: f.stretch, range: f.unicodeRange });
    }
    return out;
  }

  async function fetchB64(url) {
    const r = await win.fetch(url);
    if (!r.ok) throw new Error('HTTP ' + r.status);
    const b = new Uint8Array(await r.arrayBuffer());
    let s = '';
    for (let i = 0; i < b.length; i += 0x8000) s += String.fromCharCode.apply(null, b.subarray(i, i + 0x8000));
    return { type: r.headers.get('content-type') || '', data: win.btoa(s) };
  }

  // ---------------------------------------------------------------- replica

  function init(cfg) {
    NAMES = cfg.names;
    OPAQUE = new Set(cfg.opaque);
    return true;
  }

  // Where a bare element of this kind is created: inside SVG, or inside the ancestors the
  // parser requires (a <td> in table > tbody > tr), so the browser's defaults are the real ones.
  function probeParent(host, svgHost, k) {
    if (k.c) return svgHost;
    let parent = host;
    for (const tag of k.w || []) {
      const el = D().createElement(tag);
      parent.appendChild(el);
      parent = el;
    }
    return parent;
  }

  // The probe host has an unmistakable text colour: a default that reads back as this
  // colour is `currentcolor`, not a colour of its own.
  const PROBE_COLOR = 'rgb(1, 2, 3)';
  function probeHosts() {
    const doc = D();
    const host = doc.createElement('div');
    host.style.color = PROBE_COLOR;
    doc.body.appendChild(host);
    const svgHost = doc.createElementNS(SVG, 'svg');
    host.appendChild(svgHost);
    void doc.body.offsetHeight; // viewport units read back as 0 before the first layout
    return [host, svgHost];
  }

  // Computed style of a bare element of each kind: the browser's own defaults.
  function probe(keys) {
    const doc = D();
    const [host, svgHost] = probeHosts();
    const vals = new Interner(), out = [], ph = [];
    for (const k of keys) {
      const parent = probeParent(host, svgHost, k);
      const el = doc.createElementNS(NS[k.n], k.g);
      for (const [name, v] of k.a) { try { el.setAttribute(name, v); } catch (e) { /* ignore */ } }
      parent.appendChild(el);
      // Some tags set their own text colour (form controls). Note it, then force the marker
      // colour on the element itself so its `currentcolor` defaults still show as the marker.
      const own = gcs(el).color;
      el.style.setProperty('color', PROBE_COLOR);
      const st = readComputed(el);
      const ci = NAMES.indexOf('color');
      if (ci >= 0) st[ci] = own;
      out.push(st.map((v) => vals.id(v)));
      if (k.n === 0 && (k.g === 'input' || k.g === 'textarea')) {
        el.style.removeProperty('color');
        el.setAttribute('placeholder', 'x');
        ph.push(readResolved(el, '::placeholder').map((v) => vals.id(v)));
      } else ph.push(null);
      while (host.lastChild && host.lastChild !== svgHost) host.lastChild.remove();
      while (svgHost.lastChild) svgHost.lastChild.remove();
    }
    host.remove();
    return JSON.stringify({ vals: vals.list, styles: out, ph, probeColor: PROBE_COLOR, color: gcs(doc.body).color });
  }

  // For each kind of element and each property: does a bare element take its parent's value
  // (1), keep a value of its own (0), or something in between (2)? 3: could not be tested.
  // A browser stylesheet can make any tag break inheritance (`input { color: fieldtext }`)
  // or inherit what normally does not (`td { vertical-align: inherit }`).
  function follow(classes, tests) {
    const doc = D();
    const [host, svgHost] = probeHosts();
    const out = [];
    for (const k of classes) {
      const parent = probeParent(host, svgHost, k);
      const el = doc.createElementNS(NS[k.n], k.g);
      parent.appendChild(el);
      const verdicts = [];
      // The value is set on the direct parent; inside SVG that is a <g>, not the probe host.
      let target = parent;
      if (parent === svgHost) {
        target = doc.createElementNS(SVG, 'g');
        svgHost.appendChild(target);
        target.appendChild(el);
      } else if (parent === host) {
        target = doc.createElement('div');
        host.appendChild(target);
        target.appendChild(el);
      }
      // The maps are live: fetch them once per element.
      const tm = target.computedStyleMap(), em = el.computedStyleMap();
      const get = (m, n) => {
        try { const a = m.getAll(n); return a.length === 1 ? a[0].toString() : a.join(', '); } catch (e) { return ''; }
      };
      for (const [n, cands] of tests) {
        const pb = get(tm, n), bv = get(em, n);
        let verdict = 3;
        for (const w of cands) {
          target.style.setProperty(n, w);
          const pv = get(tm, n), cv = get(em, n);
          target.style.removeProperty(n);
          if (pv === pb) continue; // the value did not take on this parent
          verdict = cv === bv ? 0 : (cv === pv ? 1 : 2);
          break;
        }
        verdicts.push(verdict);
      }
      out.push(verdicts);
      while (host.lastChild && host.lastChild !== svgHost) host.lastChild.remove();
      while (svgHost.lastChild) svgHost.lastChild.remove();
    }
    host.remove();
    return JSON.stringify(out);
  }

  // Which properties inherit: measured, not assumed.
  function inherit(tests) {
    const doc = D();
    const p = doc.createElement('div'), c = doc.createElement('div');
    p.appendChild(c);
    doc.body.appendChild(p);
    void doc.body.offsetHeight;
    const rd = (el, n) => {
      try { const a = el.computedStyleMap().getAll(n); return a.length === 1 ? a[0].toString() : a.join(', '); } catch (e) { return ''; }
    };
    const inh = [];
    for (const [n, cands] of tests) {
      const base = rd(c, n);
      let verdict = 2; // 0 not inherited, 1 inherited, 2 inconclusive
      for (const w of cands) {
        p.style.setProperty(n, w);
        const pv = rd(p, n), cv = rd(c, n);
        p.style.removeProperty(n);
        if (pv === base) continue; // the value did not take on a <div>
        verdict = cv === pv ? 1 : 0;
        break;
      }
      inh.push(verdict);
    }
    p.remove();
    return JSON.stringify({ inh });
  }

  // Images referenced from CSS (backgrounds, masks, markers) load on their own schedule and
  // nothing announces them. Load each one once by hand, so a screenshot taken afterwards
  // does not catch a background that has not arrived yet.
  const cssImages = new Set();
  async function preloadCssImages() {
    const doc = D();
    const fresh = [];
    const re = /url\((["']?)(.*?)\1\)/g;
    const scan = (text) => {
      if (!text || text.indexOf('url(') < 0) return;
      for (const m of text.matchAll(re)) {
        const u = m[2];
        if (u && u[0] !== '#' && !cssImages.has(u)) { cssImages.add(u); fresh.push(u); }
      }
    };
    for (const st of doc.querySelectorAll('style')) scan(st.textContent);
    for (const el of doc.querySelectorAll('[style*="url("]')) scan(el.getAttribute('style'));
    if (!fresh.length) return;
    const loads = fresh.map((u) => new Promise((r) => {
      const i = new win.Image();
      i.onload = i.onerror = r;
      i.src = u;
    }));
    await Promise.race([Promise.all(loads), new Promise((r) => win.setTimeout(r, 8000))]);
    // two frames: one for the style system to pick the images up, one to paint them
    await new Promise((r) => win.requestAnimationFrame(() => win.requestAnimationFrame(r)));
  }

  async function settle() {
    const doc = D();
    void doc.body.offsetHeight; // style + layout, so font and image loads start
    await preloadCssImages();
    const pend = [];
    for (const i of doc.images) {
      if (!i.complete) pend.push(new Promise((r) => {
        i.addEventListener('load', r, { once: true });
        i.addEventListener('error', r, { once: true });
      }));
    }
    if (doc.fonts.status === 'loading') pend.push(doc.fonts.ready);
    if (pend.length) {
      await Promise.race([Promise.all(pend), new Promise((r) => win.setTimeout(r, 8000))]);
      // loaded is not yet decoded: make sure the pixels are there before anything is photographed
      await Promise.race([
        Promise.all([...doc.images].map((i) => (i.decode ? i.decode().catch(() => {}) : null))),
        new Promise((r) => win.setTimeout(r, 3000)),
      ]);
      void doc.body.offsetHeight;
      if (doc.fonts.status === 'loading') await doc.fonts.ready;
      await new Promise((r) => win.requestAnimationFrame(() => win.requestAnimationFrame(r)));
    }
  }

  function measure() {
    const doc = D();
    const out = [], range = doc.createRange();
    let ne = 0, nt = 0;
    const walk = (parent) => {
      for (let n = parent.firstChild; n; n = n.nextSibling) {
        if (n.nodeType === 1) {
          ne++;
          const rs = n.getClientRects();
          out.push(0, rs.length);
          for (const r of rs) out.push(q(r.left), q(r.top), q(r.width), q(r.height));
          if (!(n.namespaceURI === HTML && OPAQUE.has(n.localName))) walk(n);
        } else if (n.nodeType === 3) {
          range.selectNodeContents(n);
          const rs = range.getClientRects();
          // Formatting white space that renders nothing (the newline after </body>) is not content.
          if (rs.length === 0 && !/\S/.test(n.data)) continue;
          nt++;
          out.push(1, rs.length);
          for (const r of rs) out.push(q(r.left), q(r.top), q(r.width), q(r.height));
        }
      }
    };
    walk(doc.body);
    return JSON.stringify({ e: ne, t: nt, m: out });
  }

  // The stage: the body holds the copy, offset so its root lands where the original was.
  function stage(d) {
    const doc = D();
    const b = doc.body;
    b.style.cssText = 'margin:0;padding:' + d.y + 'px 0 0 ' + d.x + 'px' + (d.w ? ';width:' + d.w + 'px' : '');
    // Never let the copy grow a scrollbar: on systems with classic scrollbars it would
    // narrow the viewport and move everything.
    doc.documentElement.style.overflow = 'hidden';
    if (d.bg) doc.documentElement.style.background = d.bg;
  }

  // CLI path: the trial as markup, with a <style> element.
  let lastCss = null;
  async function render(d) {
    const doc = D();
    let st = doc.getElementById('__canon_css');
    if (!st) {
      st = doc.createElement('style');
      st.id = '__canon_css';
      doc.head.appendChild(st);
    }
    if (d.css !== lastCss) { st.textContent = d.css; lastCss = d.css; }
    stage(d);
    doc.body.innerHTML = d.html;
    await settle();
    return measure();
  }

  // In-page path: the trial as a tree (see crates/core emit::tree), built with DOM calls
  // and the CSSOM only, so it works under any Content-Security-Policy and Trusted Types
  // (markup, style attributes and <style> elements may all be refused there). Synchronous:
  // fonts and images must already be loaded into this document.
  let lastSheetCss = null, sheet = null;
  function build(node, parent, doc) {
    if (typeof node === 'string') { parent.appendChild(doc.createTextNode(node)); return; }
    if (node === 0) { parent.appendChild(doc.createComment('')); return; }
    const ns = NS[node.n || 0];
    const el = doc.createElementNS(ns, node.t);
    for (const [k, v] of node.a || []) {
      if (k.startsWith('xlink:')) el.setAttributeNS(XLINK, k, v);
      else el.setAttribute(k, v);
    }
    if (node.s) el.style.cssText = node.s;
    if (node.v !== undefined) el.value = node.v; // <textarea>
    if (node.o) {
      for (const [text, selected] of node.o) {
        const o = doc.createElement('option');
        o.textContent = text;
        if (selected) o.selected = o.defaultSelected = true;
        el.appendChild(o);
      }
    }
    for (const c of node.k || []) build(c, el, doc);
    parent.appendChild(el);
  }
  function renderTree(d) {
    const doc = D();
    if (d.css !== lastSheetCss) {
      if (!sheet) {
        sheet = new win.CSSStyleSheet();
        doc.adoptedStyleSheets = [...doc.adoptedStyleSheets, sheet];
      }
      sheet.replaceSync(d.css || '');
      lastSheetCss = d.css;
    }
    stage(d);
    const frag = doc.createDocumentFragment();
    build(d.tree, frag, doc);
    doc.body.replaceChildren(frag);
    void doc.body.offsetHeight;
    return measure();
  }

  // Shift an already loaded document so its root lands where the original was.
  async function offset(x, y) {
    const b = D().body;
    b.style.paddingLeft = x + 'px';
    b.style.paddingTop = y + 'px';
    await settle();
    return measure();
  }

  // Computed styles of every element in measure order, for read-back verification.
  function styles() {
    const vals = new Interner(), table = [], map = new Map(), order = [];
    const walk = (parent) => {
      for (let n = parent.firstElementChild; n; n = n.nextElementSibling) {
        const ids = readComputed(n).map((v) => vals.id(v));
        const k = ids.join(',');
        let i = map.get(k);
        if (i === undefined) { i = table.length; table.push(ids); map.set(k, i); }
        order.push(i);
        if (!(n.namespaceURI === HTML && OPAQUE.has(n.localName))) walk(n);
      }
    };
    walk(D().body);
    return JSON.stringify({ vals: vals.list, styles: table, order });
  }

  return {
    prepare, resume, capture, isolate, loadedFonts, fetchB64,
    init, probe, inherit, follow, render, renderTree, measure, offset, styles, settle,
    root: () => ROOT,
  };
};

if (typeof window !== 'undefined' && window.document && !window.__canon) window.__canon = createCanonAgent(window);
