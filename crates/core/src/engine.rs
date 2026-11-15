//! Engine state: the snapshot, the browser's measured defaults, and the working tree.

use crate::geom;
use crate::model::*;
use crate::props::{self, P};
use crate::reduce::Op;
use crate::style::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

#[derive(Clone, Debug)]
pub struct Options {
    /// Drop declarations that cannot change the picture (a border colour with no border...).
    pub simplify: bool,
    /// Remove and merge elements.
    pub structural: bool,
    /// Geometry tolerance in layout units (1/64 px).
    pub tol: i32,
    pub max_trials: usize,
    /// Leave the root's width to its container instead of pinning the captured width.
    pub fluid: bool,
    /// Ask the renderer for its second opinion (pixels) on every accepted step.
    pub strict: bool,
    /// Place inherited declarations where they cost the fewest (see `hoist`).
    pub hoist: bool,
    /// Hand trial documents to the renderer as trees rather than markup (`Doc::tree`).
    pub tree: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { simplify: true, structural: true, tol: 2, max_trials: 20_000, fluid: false, strict: false, hoist: true, tree: false }
    }
}

/// A kind of element whose default computed style the browser is asked for.
#[derive(Serialize, Debug, Clone)]
pub struct ProbeKey {
    /// 1 when the element sits inside SVG content.
    pub c: u8,
    pub n: u8,
    pub g: String,
    pub a: Vec<(String, String)>,
    /// Ancestors to create it under.
    pub w: Vec<&'static str>,
}

impl ProbeKey {
    fn new(c: u8, n: u8, g: &str, a: Vec<(String, String)>) -> ProbeKey {
        let w = if n == 0 && c == 0 { props::probe_wrappers(g).to_vec() } else { Vec::new() };
        ProbeKey { c, n, g: g.to_string(), a, w }
    }
}

fn key_string(k: &ProbeKey) -> String {
    let mut s = format!("{}|{}|{}", k.c, k.n, k.g);
    for (name, v) in &k.a {
        s.push('\u{1}');
        s.push_str(name);
        s.push('=');
        s.push_str(v);
    }
    s
}

#[derive(Default, Debug)]
pub struct Baseline {
    /// Computed style of a bare element, by probe key.
    pub bare: HashMap<Rc<str>, Style>,
    pub div: Style,
    pub span: Style,
    pub inherited: Vec<bool>,
    /// Properties whose default follows `color`.
    pub cc: Vec<usize>,
    /// Properties that may be `currentcolor` without defaulting to it.
    pub paint: Vec<usize>,
    /// Properties on which a bare `<div>` and a bare `<span>` differ.
    pub divspan: Vec<usize>,
    /// Per kind of element, per property: 1 when a bare element takes its parent's value.
    /// Kind 0 is a plain div/span, where this is ordinary inheritance.
    pub follow: Vec<Rc<[u8]>>,
    /// The same per probe key, with properties an attribute sets marked as the element's own.
    pub follow_key: HashMap<Rc<str>, Rc<[u8]>>,
}

#[derive(Default, Debug, Clone, Serialize)]
pub struct Stats {
    pub trials: usize,
    pub unwrapped: usize,
    pub deleted: usize,
    pub merged: usize,
    pub hollowed: usize,
    pub joined: usize,
    /// Subtrees removed because a screenshot showed nothing of them was visible.
    pub unseen: usize,
    /// Text seams closed again (see `reduce::Edit`).
    pub fused: usize,
    /// Nodes the faithful (unreduced) replica could not place where the original had them.
    pub baseline_mismatches: usize,
    pub height_pinned: bool,
}

/// Value ids the planner needs without being able to intern.
#[derive(Default, Clone, Copy)]
pub(crate) struct Consts {
    pub block: Val,
    pub flex: Val,
    pub grid: Val,
    pub table: Val,
}

pub(crate) struct PseudoRaw {
    pub elem_resolved: usize,
    pub items: [Option<(String, usize)>; 2],
}

pub struct Engine {
    pub t: Table,
    pub p: P,
    pub flags: Vec<u8>,
    pub info: Info,
    pub opts: Options,
    pub base: Baseline,
    pub work: Work,
    /// `@font-face` rules the replica needs; prepended to every stylesheet.
    pub font_css: String,
    pub stats: Stats,
    pub warnings: Vec<String>,
    /// Rendered elements in the source subtree.
    pub source_elems: usize,
    pub(crate) tried: HashSet<(usize, Op)>,
    /// Accepted edits, in the order they were applied.
    pub(crate) history: Vec<crate::reduce::Edit>,
    pub(crate) pin_h: bool,
    /// Top-left of the source root, in layout units.
    pub(crate) origin: (i32, i32),
    bounds: [f64; 4],
    pub(crate) v_current: Val,
    pub(crate) c: Consts,
    /// Per property: where an inherited value can show.
    pub(crate) pclass: Vec<crate::hoist::Class>,
    /// The region of the original that is compared pixel by pixel, in layout units.
    pub(crate) view: [i32; 4],
    raw_styles: Vec<Style>,
    raw_pseudo: HashMap<usize, PseudoRaw>,
    /// Node -> (resolved ::placeholder style, resolved style of the element).
    raw_placeholder: HashMap<usize, (usize, usize)>,
    /// Default resolved ::placeholder style, by probe key.
    bare_placeholder: HashMap<Rc<str>, Style>,
    keys: Vec<ProbeKey>,
    /// Kinds of element (tag in context, no attributes); each is also `keys[class_key[i]]`.
    classes: Vec<ProbeKey>,
    class_key: Vec<usize>,
    /// Class of each key.
    key_class: Vec<usize>,
    test_props: Vec<usize>,
    tests: Vec<(String, Vec<String>)>,
    synthetic: Option<usize>,
    pseudo_declared: HashMap<(usize, usize), HashSet<String>>,
    /// Edits accepted, for `--verbose`.
    pub trace: Vec<String>,
}

#[derive(Deserialize)]
struct ProbeOut {
    vals: Vec<String>,
    styles: Vec<Vec<u32>>,
    /// Text colour the probe elements inherited, and the document's real default.
    #[serde(rename = "probeColor")]
    probe_color: String,
    color: String,
    /// Default `::placeholder` style of the keys that can show one.
    #[serde(default)]
    ph: Vec<Option<Vec<u32>>>,
}

#[derive(Deserialize)]
struct InheritOut {
    inh: Vec<u8>,
}

pub(crate) struct Ctx {
    pub parent_inner: Inner,
    pub auto_tag: bool,
    /// A sibling shows a list marker, so this element's place in the count matters.
    pub sibling_markers: bool,
    /// A pseudo-element: blockified `display` is implied, not stated.
    pub pseudo: bool,
}

impl Engine {
    pub fn load(json: &str, opts: Options) -> Result<Engine, String> {
        let raw: RawSnapshot = serde_json::from_str(json).map_err(|e| format!("unreadable snapshot: {e}"))?;
        if let Some(e) = raw.error {
            return Err(e);
        }
        if raw.nodes.is_empty() {
            return Err("the snapshot is empty".into());
        }
        let n = raw.props.len();
        if raw.styles.iter().any(|s| s.len() != n) {
            return Err("snapshot style table is inconsistent".into());
        }
        let t = Table::new(raw.props, raw.values);
        let p = P::new(&t);
        let flags = props::flags(&t);
        let raw_styles: Vec<Style> = raw.styles.into_iter().map(Rc::from).collect();

        let mut nodes: Vec<WNode> = Vec::with_capacity(raw.nodes.len() + 1);
        let mut root = None;
        let mut extra_roots = Vec::new();
        let mut raw_placeholder: HashMap<usize, (usize, usize)> = HashMap::new();
        let mut raw_pseudo = HashMap::new();
        let mut pseudo_declared: HashMap<(usize, usize), HashSet<String>> = HashMap::new();
        let mut source_elems = 0;
        for (i, rn) in raw.nodes.into_iter().enumerate() {
            let parent = match rn.p {
                -1 => {
                    root = Some(i);
                    None
                }
                -2 => {
                    extra_roots.push(i);
                    None
                }
                x if x >= 0 && (x as usize) < i => Some(x as usize),
                _ => return Err("snapshot tree is malformed".into()),
            };
            let kind = if let Some(tag) = rn.g {
                let sid = rn.s.ok_or("element without a style")? as usize;
                let style = raw_styles.get(sid).ok_or("style index out of range")?.clone();
                let parent_svg = match parent.map(|pid| &nodes[pid].kind) {
                    Some(Kind::Elem(pe)) => pe.ns == 1 && pe.orig_tag != "foreignObject",
                    _ => rn.p == -2,
                };
                let in_svg = parent_svg && rn.n == 1;
                let keep = rn.n != 0 || props::is_keep(&tag);
                if rn.p != -2 {
                    source_elems += 1;
                }
                if let (Some(h), Some(e)) = (rn.h, rn.e) {
                    raw_placeholder.insert(i, (h as usize, e as usize));
                }
                if let (Some(q), Some(e)) = (rn.q, rn.e) {
                    let mut items: [Option<(String, usize)>; 2] = [None, None];
                    for (k, it) in q.into_iter().take(2).enumerate() {
                        if let Some(it) = it {
                            if let Some(d) = it.d {
                                pseudo_declared.insert((i, k), d.into_iter().collect::<HashSet<String>>());
                            }
                            items[k] = Some((it.c, it.s as usize));
                        }
                    }
                    raw_pseudo.insert(i, PseudoRaw { elem_resolved: e as usize, items });
                }
                Kind::Elem(Rc::new(Elem {
                    tag: if keep { Tag::Keep(tag.clone()) } else { Tag::Auto },
                    ns: rn.n,
                    orig_tag: tag.clone(),
                    attrs: if keep { rn.a } else { Vec::new() },
                    key: None,
                    cls: 0,
                    style,
                    rects: geom::from_flat(&rn.r),
                    bbox: rn.b,
                    pseudo: [None, None],
                    extra: Vec::new(),
                    inner: rn.i,
                    opaque: rn.n == 0 && props::is_opaque(&tag),
                    rigid: keep,
                    in_svg,
                    unchecked: rn.p == -2,
                }))
            } else {
                Kind::Text(Rc::new(Text { data: rn.x.unwrap_or_default(), rects: geom::from_flat(&rn.r), sep: false }))
            };
            nodes.push(WNode { parent, kids: Vec::new(), alive: true, kind });
            if let Some(pid) = parent {
                nodes[pid].kids.push(i);
            }
        }
        let root = root.ok_or("snapshot has no root")?;
        let mut work = Work { nodes, root };

        // A table part outside its table is dropped by the HTML parser, so it cannot keep its tag.
        for i in 0..work.nodes.len() {
            let demote = match work.elem(i) {
                Some(e) if e.ns == 0 => match &e.tag {
                    Tag::Keep(name) => match props::required_parent(name) {
                        Some(req) => !matches!(
                            work.nodes[i].parent.and_then(|pid| work.elem(pid)),
                            Some(pe) if pe.ns == 0 && matches!(&pe.tag, Tag::Keep(pn) if req.contains(&pn.as_str()))
                        ),
                        None => false,
                    },
                    Tag::Auto => false,
                },
                _ => false,
            };
            if demote {
                let e = work.elem_mut(i).unwrap();
                e.tag = Tag::Auto;
                e.attrs.clear();
                e.rigid = false;
                e.opaque = false;
            }
        }

        // Out-of-tree <use> targets ride along in a zero-size <svg> at the end of the root.
        let mut synthetic = None;
        if !extra_roots.is_empty() {
            let id = work.nodes.len();
            let style = work.elem(root).unwrap().style.clone();
            work.nodes.push(WNode {
                parent: Some(root),
                kids: extra_roots.clone(),
                alive: true,
                kind: Kind::Elem(Rc::new(Elem {
                    tag: Tag::Keep("svg".into()),
                    ns: 1,
                    orig_tag: "svg".into(),
                    attrs: vec![("width".into(), "0".into()), ("height".into(), "0".into())],
                    key: None,
                    cls: 0,
                    style,
                    rects: Vec::new(),
                    bbox: None,
                    pseudo: [None, None],
                    extra: Vec::new(),
                    inner: None,
                    opaque: false,
                    rigid: true,
                    in_svg: false,
                    unchecked: true,
                })),
            });
            for r in extra_roots {
                work.nodes[r].parent = Some(id);
            }
            work.nodes[root].kids.push(id);
            synthetic = Some(id);
        }

        // Probe keys: <div>, <span>, then every distinct kept tag + attribute combination.
        // A "class" is a kept tag in its context without attributes; class 0 is div/span.
        let mut keys = vec![ProbeKey::new(0, 0, "div", vec![]), ProbeKey::new(0, 0, "span", vec![])];
        let mut seen: HashMap<String, (Rc<str>, usize)> = HashMap::new();
        for (i, k) in keys.iter().enumerate() {
            let s = key_string(k);
            seen.insert(s.clone(), (Rc::from(s.as_str()), i));
        }
        let mut classes = vec![keys[0].clone()];
        let mut class_key = vec![0usize];
        let mut key_class = vec![0usize, 0usize];
        let mut class_ix: HashMap<String, usize> = HashMap::new();
        let mut intern_key = |k: ProbeKey, keys: &mut Vec<ProbeKey>, key_class: &mut Vec<usize>, cls: usize| -> (Rc<str>, usize) {
            let s = key_string(&k);
            if let Some(hit) = seen.get(&s) {
                return hit.clone();
            }
            let rc: Rc<str> = Rc::from(s.as_str());
            keys.push(k);
            key_class.push(cls);
            seen.insert(s, (rc.clone(), keys.len() - 1));
            (rc, keys.len() - 1)
        };
        for i in 0..work.nodes.len() {
            let key = match work.elem(i) {
                Some(e) => match &e.tag {
                    Tag::Keep(name) => {
                        let mut a: Vec<(String, String)> =
                            e.attrs.iter().filter(|(k, _)| !props::CONTENT_ATTRS.contains(&k.as_str())).cloned().collect();
                        a.sort();
                        Some(ProbeKey::new(e.in_svg as u8, e.ns, name, a))
                    }
                    Tag::Auto => None,
                },
                None => None,
            };
            if let Some(k) = key {
                let bare_k = ProbeKey::new(k.c, k.n, &k.g, vec![]);
                let cs = key_string(&bare_k);
                let cls = match class_ix.get(&cs) {
                    Some(&c) => c,
                    None => {
                        let c = classes.len();
                        class_ix.insert(cs, c);
                        classes.push(bare_k.clone());
                        class_key.push(0);
                        let (_, ki) = intern_key(bare_k, &mut keys, &mut key_class, c);
                        class_key[c] = ki;
                        c
                    }
                };
                let (rc, _) = intern_key(k, &mut keys, &mut key_class, cls);
                let em = work.elem_mut(i).unwrap();
                em.key = Some(rc);
                em.cls = cls;
            }
        }

        let origin = geom::bounds(&work.elem(root).unwrap().rects).map(|b| (b[0], b[1])).ok_or("the root element has no box")?;
        let mut t = t;
        let v_current = t.intern("currentcolor");
        let c = Consts { block: t.intern("block"), flex: t.intern("flex"), grid: t.intern("grid"), table: t.intern("table") };
        let pclass = t.props.iter().map(|n| crate::hoist::classify(n)).collect();
        let mut eng = Engine {
            t,
            p,
            flags,
            info: raw.info,
            opts,
            base: Baseline::default(),
            work,
            font_css: String::new(),
            stats: Stats::default(),
            warnings: Vec::new(),
            source_elems,
            tried: HashSet::new(),
            history: Vec::new(),
            pin_h: false,
            origin,
            v_current,
            c,
            pclass,
            view: [0; 4],
            raw_styles,
            raw_pseudo,
            raw_placeholder,
            bare_placeholder: HashMap::new(),
            keys,
            classes,
            class_key,
            key_class,
            test_props: Vec::new(),
            tests: Vec::new(),
            synthetic,
            pseudo_declared,
            trace: Vec::new(),
            bounds: [0.0; 4],
        };
        eng.bounds = eng.compute_bounds();
        let u = |v: f64| (v * geom::UNIT).round() as i32;
        eng.view = [u(eng.bounds[0]), u(eng.bounds[1]), u(eng.bounds[0] + eng.bounds[2]), u(eng.bounds[1] + eng.bounds[3])];
        Ok(eng)
    }

    pub fn probe_keys(&self) -> &[ProbeKey] {
        &self.keys
    }

    pub fn set_probe(&mut self, json: &str) -> Result<(), String> {
        let out: ProbeOut = serde_json::from_str(json).map_err(|e| format!("unreadable probe result: {e}"))?;
        if out.styles.len() != self.keys.len() || out.styles.iter().any(|s| s.len() != self.t.nprops()) {
            return Err("probe result does not match the request".into());
        }
        // A bare element was measured under a marker text colour: wherever a property reads
        // back as that colour it is `currentcolor`, and `color` itself is the real default.
        let marker = self.t.intern(&out.probe_color);
        let real = self.t.intern(&out.color);
        for (k, ids) in self.keys.iter().zip(&out.styles) {
            let mut st = self.t.adopt(&out.vals, ids).to_vec();
            for q in 0..st.len() {
                if st[q] == marker {
                    st[q] = if Some(q) == self.p.color { real } else { self.v_current };
                }
            }
            self.base.bare.insert(Rc::from(key_string(k).as_str()), Rc::from(st));
        }
        for (k, ids) in self.keys.iter().zip(&out.ph) {
            if let Some(ids) = ids {
                let st = self.t.adopt(&out.vals, ids);
                self.bare_placeholder.insert(Rc::from(key_string(k).as_str()), st);
            }
        }
        self.base.div = self.base.bare[key_string(&self.keys[0]).as_str()].clone();
        self.base.span = self.base.bare[key_string(&self.keys[1]).as_str()].clone();
        Ok(())
    }

    /// For each property that takes a non-default value somewhere in the snapshot: values to
    /// try on a parent to see whether a child picks them up.
    pub fn inherit_tests(&mut self) -> Vec<(String, Vec<String>)> {
        let n = self.t.nprops();
        let mut cands: Vec<Vec<Val>> = vec![Vec::new(); n];
        for st in &self.raw_styles {
            for p in 0..n {
                let v = st[p];
                if v != self.base.div[p] && cands[p].len() < 3 && !cands[p].contains(&v) {
                    let s = self.t.get(v);
                    if !s.is_empty() && s.len() < 200 && !s.contains("url(") {
                        cands[p].push(v);
                    }
                }
            }
        }
        // A kept tag's own default counts too: `border-spacing` is 2px on a bare <table> even
        // when every table on the page sets it to 0.
        for bare in self.base.bare.values() {
            for p in 0..n {
                let v = bare[p];
                if v != self.base.div[p] && cands[p].len() < 4 && !cands[p].contains(&v) && v != self.v_current {
                    let s = self.t.get(v);
                    if !s.is_empty() && s.len() < 200 && !s.contains("url(") {
                        cands[p].push(v);
                    }
                }
            }
        }
        let mut out = Vec::new();
        self.test_props.clear();
        for (p, c) in cands.iter().enumerate() {
            if c.is_empty() {
                continue;
            }
            self.test_props.push(p);
            out.push((self.t.props[p].clone(), c.iter().map(|&v| self.t.get(v).to_string()).collect()));
        }
        self.tests = out.clone();
        out
    }

    /// Kinds of kept element, and the values to try on their parents, for the per-tag
    /// inheritance test.
    pub fn follow_request(&self) -> (&[ProbeKey], &[(String, Vec<String>)]) {
        (&self.classes[1..], &self.tests)
    }

    pub fn set_follow(&mut self, json: &str) -> Result<(), String> {
        let out: Vec<Vec<u8>> = serde_json::from_str(json).map_err(|e| format!("unreadable inheritance result: {e}"))?;
        if out.len() != self.classes.len() - 1 || out.iter().any(|v| v.len() != self.test_props.len()) {
            return Err("per-tag inheritance result does not match the request".into());
        }
        let auto: Vec<u8> = self.base.inherited.iter().map(|&b| b as u8).collect();
        self.base.follow = vec![Rc::from(auto.clone())];
        for verdicts in out {
            let mut f = auto.clone();
            for (i, &p) in self.test_props.iter().enumerate() {
                match verdicts[i] {
                    1 => f[p] = 1,
                    0 | 2 => f[p] = 0,
                    _ => {} // untested: assume ordinary inheritance
                }
            }
            self.base.follow.push(Rc::from(f));
        }
        Ok(())
    }

    /// Names of the properties the page's rules set on an element's `::before` (0) or
    /// `::after` (1). Lets `prepare` tell an `auto` size from one that only resolved to pixels.
    pub fn set_pseudo_declared(&mut self, node: usize, which: usize, names: HashSet<String>) {
        self.pseudo_declared.insert((node, which), names);
    }

    pub(crate) fn follow_of(&self, e: &Elem) -> &Rc<[u8]> {
        match &e.key {
            Some(k) => self.base.follow_key.get(k).unwrap_or(&self.base.follow[0]),
            None => &self.base.follow[0],
        }
    }

    pub fn set_inherit(&mut self, json: &str) -> Result<(), String> {
        let out: InheritOut = serde_json::from_str(json).map_err(|e| format!("unreadable inheritance result: {e}"))?;
        if out.inh.len() != self.test_props.len() {
            return Err("inheritance result does not match the request".into());
        }
        let n = self.t.nprops();
        self.base.inherited = vec![false; n];
        for (i, &p) in self.test_props.iter().enumerate() {
            self.base.inherited[p] = out.inh[i] == 1;
        }
        // Properties that default to `currentcolor`: the ones a bare <div> reported as such.
        self.base.cc = (0..n).filter(|&p| self.base.div[p] == self.v_current).collect();
        // SVG paint can be `currentColor` too, though it does not start out that way.
        self.base.paint = ["fill", "stroke"].iter().filter_map(|name| self.t.prop(name)).collect();
        self.base.divspan = (0..n).filter(|&p| self.base.div[p] != self.base.span[p]).collect();
        Ok(())
    }

    /// Spell a colour that equals the element's own text colour `currentcolor`, where that
    /// is what it most plausibly is: always for properties that default to it, and for SVG
    /// paint unless the colour is also that property's plain default.
    pub(crate) fn cc_normalize(&self, st: &mut [Val]) {
        if let Some(c) = self.p.color {
            let col = st[c];
            for &p in &self.base.cc {
                if st[p] == col {
                    st[p] = self.v_current;
                }
            }
            for &p in &self.base.paint {
                if st[p] == col && col != self.base.div[p] {
                    st[p] = self.v_current;
                }
            }
        }
    }

    pub(crate) fn bare_of(&self, e: &Elem) -> &Style {
        match &e.key {
            Some(k) => self.base.bare.get(k).unwrap_or(&self.base.div),
            None => &self.base.div,
        }
    }

    /// Turn raw computed styles into canonical target styles and tidy the tree.
    pub fn prepare(&mut self) -> Result<(), String> {
        if self.base.div.is_empty() || self.base.inherited.is_empty() {
            return Err("prepare() needs the probe and inheritance results first".into());
        }
        if self.base.follow.is_empty() {
            let auto: Vec<u8> = self.base.inherited.iter().map(|&b| b as u8).collect();
            self.base.follow = (0..self.classes.len()).map(|_| Rc::from(auto.clone())).collect();
        }
        // A property an attribute sets (`fill="red"`, `width="24"`) is the element's own.
        for (ki, k) in self.keys.iter().enumerate() {
            let cls = self.key_class[ki];
            let ks: Rc<str> = Rc::from(key_string(k).as_str());
            let class_bare = self.base.bare[key_string(&self.keys[self.class_key[cls]]).as_str()].clone();
            let bare = self.base.bare[&ks].clone();
            let mut f = self.base.follow[cls].to_vec();
            for q in 0..f.len() {
                if bare[q] != class_bare[q] {
                    f[q] = 0;
                }
            }
            self.base.follow_key.insert(ks, Rc::from(f));
        }

        let mut pool: HashMap<Vec<Val>, Style> = HashMap::new();
        let mut intern = |v: Vec<Val>| -> Style {
            if let Some(s) = pool.get(&v) {
                return s.clone();
            }
            let s: Style = Rc::from(v.clone());
            pool.insert(v, s.clone());
            s
        };

        let display_of = |eng: &Engine, st: &[Val]| parse_display(eng.t.at(st, eng.p.display));
        let n_nodes = self.work.nodes.len();
        // Parent display is read from the raw style, before any clean-up.
        let parent_inner: Vec<Inner> = (0..n_nodes)
            .map(|i| match self.work.nodes[i].parent.and_then(|pid| self.work.elem(pid)) {
                Some(pe) => display_of(self, &pe.style).1,
                None => Inner::Other,
            })
            .collect();

        // Does any child of this node's parent show a list marker? Then list items there
        // must stay list items: they are being counted.
        let marker: Vec<bool> = (0..n_nodes).map(|i| self.work.elem(i).is_some_and(|e| self.has_marker(&e.style))).collect();
        let sibling_markers: Vec<bool> = (0..n_nodes)
            .map(|i| match self.work.nodes[i].parent {
                Some(pid) => self.work.nodes[pid].kids.iter().any(|&k| marker[k]),
                None => marker[i],
            })
            .collect();

        for i in 0..n_nodes {
            let Some(e) = self.work.elem(i) else { continue };
            let e = e.clone();
            let bare = self.bare_of(&e).clone();
            let mut st = e.style.to_vec();
            self.cc_normalize(&mut st);
            let ctx = Ctx { parent_inner: parent_inner[i], auto_tag: e.tag == Tag::Auto, sibling_markers: sibling_markers[i], pseudo: false };
            if self.opts.simplify {
                if e.in_svg {
                    self.canonicalize_svg(&mut st, &bare);
                } else {
                    self.canonicalize(&mut st, &bare, &ctx);
                }
            }
            let mut pseudo: [Option<Pseudo>; 2] = [None, None];
            let pr = self.raw_pseudo.get(&i).map(|pr| (pr.elem_resolved, pr.items.clone()));
            if let Some((elem_resolved, items)) = pr {
                let eres = self.raw_styles.get(elem_resolved).cloned();
                let own_inner = display_of(self, &st).1;
                for k in 0..2 {
                    let Some((content, sid)) = &items[k] else { continue };
                    let (Some(pres), Some(eres)) = (self.raw_styles.get(*sid).cloned(), eres.clone()) else { continue };
                    let declared = self.pseudo_declared.get(&(i, k)).cloned();
                    let mut ps = self.pseudo_target(&pres, &eres, &st, declared.as_ref());
                    if self.opts.simplify {
                        let span = self.base.span.clone();
                        self.canonicalize(&mut ps, &span, &Ctx { parent_inner: own_inner, auto_tag: true, sibling_markers: true, pseudo: true });
                    }
                    pseudo[k] = Some(Pseudo { content: content.clone(), style: intern(ps) });
                }
            }
            // ::placeholder: only a few text properties apply to it. Those that inherit are
            // compared with the control itself, the others with a bare control's placeholder.
            let mut extra = Vec::new();
            if let (Some(&(h, er)), Some(key)) = (self.raw_placeholder.get(&i), e.key.as_ref()) {
                if let (Some(ps), Some(es), Some(bare_ph)) = (self.raw_styles.get(h), self.raw_styles.get(er), self.bare_placeholder.get(key)) {
                    const OWN: &[&str] =
                        &["color", "opacity", "line-height", "text-decoration-line", "text-decoration-color", "text-decoration-style", "background-color", "text-overflow"];
                    const INHERITED: &[&str] = &[
                        "font-family", "font-size", "font-style", "font-weight", "font-stretch", "font-variant-caps", "letter-spacing", "word-spacing",
                        "text-transform", "text-shadow", "text-align", "text-indent", "-webkit-text-fill-color",
                    ];
                    // A colour that merely follows the placeholder's own colour is not a declaration.
                    let follows_color = |q: usize| self.p.color.is_some_and(|c| q != c && ps[q] == ps[c]) && (self.base.cc.contains(&q));
                    let mut d: Vec<(usize, Val)> = Vec::new();
                    for name in OWN {
                        if let Some(q) = self.t.prop(name) {
                            if ps[q] != bare_ph[q] && !follows_color(q) {
                                d.push((q, ps[q]));
                            }
                        }
                    }
                    for name in INHERITED {
                        if let Some(q) = self.t.prop(name) {
                            if ps[q] != es[q] && !follows_color(q) {
                                d.push((q, ps[q]));
                            }
                        }
                    }
                    d.sort_unstable_by_key(|&(q, _)| q);
                    if !d.is_empty() {
                        extra.push(("placeholder", d));
                    }
                }
            }
            let em = self.work.elem_mut(i).unwrap();
            em.style = intern(st);
            em.pseudo = pseudo;
            em.extra = extra;
        }

        if let Some(id) = self.synthetic {
            let root_style = self.work.elem(self.work.root).unwrap().style.clone();
            let bare = self.bare_of(self.work.elem(id).unwrap()).clone();
            let mut st = bare.to_vec();
            for p in 0..st.len() {
                if self.base.inherited[p] {
                    st[p] = root_style[p];
                }
            }
            if let Some(pp) = self.p.position {
                st[pp] = self.t.intern("absolute");
            }
            self.work.elem_mut(id).unwrap().style = Rc::from(st);
        }

        // Collapsible white space is stored collapsed.
        for i in 0..n_nodes {
            let Kind::Text(tx) = &self.work.nodes[i].kind else { continue };
            let mode = match self.work.nodes[i].parent.and_then(|pid| self.work.elem(pid)) {
                Some(pe) => self.t.at(&pe.style, self.p.white_space_collapse).to_string(),
                None => String::new(),
            };
            let data = match mode.as_str() {
                "collapse" | "" => collapse_ws(&tx.data, false),
                "preserve-breaks" => collapse_ws(&tx.data, true),
                _ => continue,
            };
            if data != tx.data {
                let rects = tx.rects.clone();
                self.work.nodes[i].kind = Kind::Text(Rc::new(Text { data, rects, sep: false }));
            }
        }

        if self.info.transformed {
            self.warnings.push("an ancestor of the element is transformed; the copy is drawn untransformed".into());
        }
        if self.info.scrolled > 0 {
            self.warnings.push(format!("{} scrolled container(s) inside the element are reset to their start", self.info.scrolled));
        }
        if self.info.canvas_tainted > 0 {
            self.warnings.push(format!("{} cross-origin canvas(es) could not be read and are left empty", self.info.canvas_tainted));
        }
        if self.info.backdrop.complex {
            self.warnings.push("an ancestor paints an image or gradient behind the element; only its flat colour is kept".into());
        }
        Ok(())
    }

    /// A pseudo-element's style is only available resolved (used pixels). Bring it back to
    /// something comparable with computed values.
    fn pseudo_target(&mut self, pres: &[Val], eres: &[Val], ecomp: &[Val], declared: Option<&HashSet<String>>) -> Vec<Val> {
        let n = pres.len();
        let mut out = pres.to_vec();
        self.cc_normalize(&mut out);
        for p in 0..n {
            if self.base.inherited[p] && pres[p] == eres[p] {
                out[p] = ecomp[p];
            }
        }
        let span = self.base.span.clone();
        // Sizes, offsets and margins read back as used pixels even when they are `auto`.
        // Where no rule of the page sets them, they are the default.
        if let Some(d) = declared {
            let any = |names: &[&str]| names.iter().any(|x| d.contains(*x));
            let logical_size = any(&["inline-size", "block-size"]);
            let logical_inset = d.iter().any(|x| x.starts_with("inset-"));
            let logical_margin = d.iter().any(|x| x.starts_with("margin-block") || x.starts_with("margin-inline"));
            let mut reset = |q: Option<usize>, stated: bool| {
                if let (Some(q), false) = (q, stated) {
                    out[q] = span[q];
                }
            };
            reset(self.p.width, any(&["width"]) || logical_size);
            reset(self.p.height, any(&["height"]) || logical_size);
            for (i, side) in props::SIDES.iter().enumerate() {
                reset(self.p.inset[i], d.contains(*side) || d.contains("inset") || logical_inset);
                reset(self.p.margin[i], d.contains(&format!("margin-{side}")) || d.contains("margin") || logical_margin);
            }
        }
        let zero = self.t.intern("0px");
        for p in [self.p.min_width, self.p.min_height].into_iter().flatten() {
            if out[p] == zero {
                out[p] = span[p];
            }
        }
        // Resolved widths read 0px wherever the style is `none`; the computed default is not 0.
        for (sp, wp) in [("outline-style", "outline-width"), ("column-rule-style", "column-rule-width"), ("row-rule-style", "row-rule-width")] {
            if let (Some(sp), Some(wp)) = (self.t.prop(sp), self.t.prop(wp)) {
                if self.t.get(out[sp]) == "none" {
                    out[wp] = span[wp];
                }
            }
        }
        for i in 0..4 {
            if let (Some(sp), Some(wp)) = (self.p.border_style[i], self.p.border_width[i]) {
                if matches!(self.t.get(out[sp]), "none" | "hidden") {
                    out[wp] = span[wp];
                }
            }
        }
        let none = |eng: &Engine, st: &[Val], p: Option<usize>| matches!(eng.t.at(st, p), "none" | "");
        if none(self, &out, self.p.transform) && none(self, &out, self.p.rotate) && none(self, &out, self.p.scale) && none(self, &out, self.p.translate) {
            if let Some(p) = self.p.transform_origin {
                out[p] = span[p];
            }
        }
        if none(self, &out, self.p.perspective) {
            if let Some(p) = self.p.perspective_origin {
                out[p] = span[p];
            }
        }
        out
    }

    /// Reset declarations that cannot affect the picture to the element's default, so they
    /// are neither emitted nor mistaken for something the element paints.
    pub(crate) fn canonicalize(&mut self, st: &mut [Val], bare: &[Val], ctx: &Ctx) {
        let p = self.p.clone();
        let v_none = self.t.intern("none");
        let v_auto = self.t.intern("auto");
        let s = |eng: &Engine, st: &[Val], q: Option<usize>| -> String { eng.t.at(st, q).to_string() };
        let reset = |st: &mut [Val], q: Option<usize>| {
            if let Some(q) = q {
                st[q] = bare[q];
            }
        };
        let is_default = |st: &[Val], q: Option<usize>| q.is_none_or(|q| st[q] == bare[q]);
        let display = s(self, st, p.display);

        if !self.info.bidi {
            reset(st, p.unicode_bidi);
        }
        // `color-scheme` is frozen to the scheme the page was captured in, so the copy does
        // not change with the viewer's settings; the light scheme is the default.
        if let Some(q) = self.t.prop("color-scheme") {
            let v = self.t.get(st[q]).to_string();
            let (light, dark) = (v.split_whitespace().any(|w| w == "light"), v.split_whitespace().any(|w| w == "dark"));
            if light || dark {
                let use_dark = dark && (!light || self.info.dark);
                st[q] = if use_dark { self.t.intern("dark") } else { self.base.div[q] };
            }
        }

        // A list item with no marker, among siblings with none, is a block.
        if display == "list-item" && !ctx.sibling_markers && !self.has_marker(st) {
            if let Some(q) = p.display {
                st[q] = self.c.block;
            }
        }
        let display = s(self, st, p.display);
        let (outer, inner) = parse_display(&display);

        // An element with no box: nothing about its box matters.
        if inner == Inner::Contents {
            for q in 0..st.len() {
                if !self.base.inherited[q] && Some(q) != p.display {
                    st[q] = bare[q];
                }
            }
            return;
        }

        // Borders that draw nothing and take no space.
        if matches!(s(self, st, p.border_image_source).as_str(), "none" | "") {
            for i in 0..4 {
                let style = s(self, st, p.border_style[i]);
                let width = s(self, st, p.border_width[i]);
                if style == "none" || (width == "0px" && style != "hidden") {
                    if let Some(q) = p.border_style[i] {
                        st[q] = v_none;
                    }
                    reset(st, p.border_width[i]);
                    reset(st, p.border_color[i]);
                }
            }
            for &q in &p.border_image {
                st[q] = bare[q];
            }
        }
        // Column and row rules that are not drawn: their width is noise.
        for (sp, wp, cp) in [("column-rule-style", "column-rule-width", "column-rule-color"), ("row-rule-style", "row-rule-width", "row-rule-color")] {
            if let (Some(sq), Some(wq)) = (self.t.prop(sp), self.t.prop(wp)) {
                if matches!(self.t.get(st[sq]), "none" | "hidden") {
                    st[wq] = bare[wq];
                    if let Some(cq) = self.t.prop(cp) {
                        st[cq] = bare[cq];
                    }
                }
            }
        }
        let ostyle = s(self, st, p.outline_style);
        if ostyle == "none" || (ostyle != "auto" && s(self, st, p.outline_width) == "0px") {
            if let Some(q) = p.outline_style {
                st[q] = v_none;
            }
            reset(st, p.outline_width);
            reset(st, p.outline_color);
            reset(st, p.outline_offset);
        }
        if s(self, st, p.td_line) == "none" {
            reset(st, p.td_color);
            reset(st, p.td_style);
            reset(st, p.td_thickness);
        }
        let none = |eng: &Engine, st: &[Val], q: Option<usize>| matches!(eng.t.at(st, q), "none" | "");
        if none(self, st, p.transform) && none(self, st, p.rotate) && none(self, st, p.scale) && none(self, st, p.translate) && none(self, st, p.offset_path) {
            reset(st, p.transform_origin);
        }
        if none(self, st, p.perspective) {
            reset(st, p.perspective_origin);
        }
        let positioned = s(self, st, p.position) != "static";
        if !positioned {
            for q in p.inset {
                reset(st, q);
            }
        }
        if none(self, st, p.bg_image) {
            for &q in &p.bg_layer {
                st[q] = bare[q];
            }
            if s(self, st, p.bg_color) == "rgba(0, 0, 0, 0)" {
                reset(st, p.bg_clip);
            }
        } else {
            // One value per layer, all of them the default: that is just the default.
            for q in p.bg_layer.iter().copied().chain(p.bg_clip) {
                let d = self.t.get(bare[q]).to_string();
                let v = self.t.get(st[q]);
                if v != d && v.split(", ").all(|item| item == d) {
                    st[q] = bare[q];
                }
            }
        }
        if none(self, st, p.mask_image) {
            for &q in &p.mask_layer {
                st[q] = bare[q];
            }
        }

        // Container properties only act on the matching kind of container.
        let multicol = !is_default(st, p.column_count) || !is_default(st, p.column_width);
        if inner != Inner::Flex {
            reset(st, p.flex_direction);
            reset(st, p.flex_wrap);
        }
        if !matches!(inner, Inner::Flex | Inner::Grid) {
            reset(st, p.align_items);
            reset(st, p.justify_content);
            reset(st, p.row_gap);
            if !multicol {
                reset(st, p.column_gap);
            }
        }
        if inner != Inner::Grid {
            reset(st, p.justify_items);
            for &q in &p.grid_container {
                st[q] = bare[q];
            }
        }
        // Item properties only act inside the matching kind of container.
        let in_flex_grid = matches!(ctx.parent_inner, Inner::Flex | Inner::Grid);
        if !in_flex_grid {
            reset(st, p.flex_grow);
            reset(st, p.flex_shrink);
            reset(st, p.flex_basis);
            reset(st, p.order);
            if !positioned {
                reset(st, p.align_self);
                reset(st, p.justify_self);
                reset(st, p.z_index);
            }
            for q in [p.min_width, p.min_height].into_iter().flatten() {
                if self.t.get(st[q]) == "0px" && self.t.get(bare[q]) == "auto" {
                    st[q] = v_auto;
                }
            }
        }
        if ctx.parent_inner != Inner::Grid {
            for &q in &p.grid_item {
                st[q] = bare[q];
            }
        }

        if outer == Outer::Block {
            reset(st, p.vertical_align);
        }
        let ox = s(self, st, p.overflow_x);
        let oy = s(self, st, p.overflow_y);
        if ox == "visible" {
            reset(st, p.text_overflow);
        }
        if ox == "visible" && oy == "visible" {
            reset(st, p.resize);
        }
        if ox != "clip" && oy != "clip" {
            reset(st, p.overflow_clip_margin);
        }
        if ctx.auto_tag {
            reset(st, p.object_fit);
            reset(st, p.object_position);
            // Sizes and vertical margins do not apply to a non-replaced inline box.
            if display == "inline" {
                for q in [p.width, p.height, p.min_width, p.min_height, p.max_width, p.max_height, p.aspect_ratio, p.margin[0], p.margin[2]] {
                    reset(st, q);
                }
            }
        }
        // A pseudo-element that is positioned, floated or a flex/grid item is block-level by
        // rule; `display: block` then says nothing.
        if ctx.pseudo && display == "block" {
            let floated = !matches!(s(self, st, p.float).as_str(), "none" | "");
            let out_of_flow = matches!(s(self, st, p.position).as_str(), "absolute" | "fixed");
            if floated || out_of_flow || matches!(ctx.parent_inner, Inner::Flex | Inner::Grid) {
                reset(st, p.display);
            }
        }
        // With no padding and no border the two box models are the same box.
        if (0..4).all(|i| s(self, st, p.padding[i]) == "0px" && s(self, st, p.border_style[i]) == "none") {
            reset(st, p.box_sizing);
        }
        // `box-sizing` only matters once something sizes the box (a minimum of zero does not).
        let no_size = |eng: &Engine, st: &[Val], q: Option<usize>, zero_ok: bool| is_default(st, q) || (zero_ok && eng.t.at(st, q) == "0px");
        if [p.width, p.height, p.max_width, p.max_height, p.flex_basis, p.aspect_ratio].iter().all(|&q| no_size(self, st, q, false))
            && [p.min_width, p.min_height].iter().all(|&q| no_size(self, st, q, true))
        {
            reset(st, p.box_sizing);
        }
        // A radius with nothing to round.
        if ctx.auto_tag
            && s(self, st, p.bg_color) == "rgba(0, 0, 0, 0)"
            && none(self, st, p.bg_image)
            && (0..4).all(|i| s(self, st, p.border_style[i]) == "none")
            && none(self, st, p.box_shadow)
            && none(self, st, p.outline_style)
            && none(self, st, p.backdrop_filter)
            && ox == "visible"
            && oy == "visible"
            && matches!(self.t.at(st, self.t.prop("contain")), "none" | "")
        {
            for q in p.radius {
                reset(st, q);
            }
        }
    }

    /// Whole-pixel shift from source viewport coordinates to replica coordinates: zero unless
    /// the original (or something it draws) starts above or left of the viewport.
    pub fn shift(&self) -> (f64, f64) {
        let b = self.bounds;
        let k = |lo: f64| if lo.floor() >= 0.0 { 0.0 } else { -lo.floor() };
        (k(b[0].min(self.info.rect[0])), k(b[1].min(self.info.rect[1])))
    }

    /// SVG graphics have no CSS box: what page-wide resets put there is noise.
    pub(crate) fn canonicalize_svg(&mut self, st: &mut [Val], bare: &[Val]) {
        let p = &self.p;
        let mut reset = |q: Option<usize>| {
            if let Some(q) = q {
                st[q] = bare[q];
            }
        };
        for i in 0..4 {
            reset(p.margin[i]);
            reset(p.padding[i]);
            reset(p.border_style[i]);
            reset(p.border_width[i]);
            reset(p.border_color[i]);
            reset(p.radius[i]);
        }
        reset(p.box_sizing);
        if !self.info.bidi {
            reset(p.unicode_bidi);
        }
    }

    /// The doctype a document needs to be in the same mode as the source ("" for quirks).
    pub fn doctype(&self) -> String {
        if self.info.compat == "BackCompat" {
            String::new()
        } else if !self.info.doctype.is_empty() {
            self.info.doctype.clone()
        } else {
            "<!doctype html>".into()
        }
    }

    /// Where the replica's root should land, in px: the original position plus `shift`, so
    /// both are rasterised at the same sub-pixel offset.
    pub fn target_origin(&self) -> (f64, f64) {
        let (kx, ky) = self.shift();
        (self.info.rect[0] + kx, self.info.rect[1] + ky)
    }

    /// Bounding box of everything the original drew, in viewport px.
    pub fn source_bounds(&self) -> [f64; 4] {
        self.bounds
    }

    fn compute_bounds(&self) -> [f64; 4] {
        let mut b = None;
        for n in &self.work.nodes {
            if !n.alive {
                continue;
            }
            let rects = match &n.kind {
                Kind::Elem(e) if !e.unchecked => &e.rects,
                Kind::Text(t) => &t.rects,
                _ => continue,
            };
            b = geom::union(b, geom::bounds(rects));
        }
        let b = b.unwrap_or([0, 0, 0, 0]);
        let u = geom::UNIT;
        let (mut x0, mut y0, mut x1, mut y1) = (b[0] as f64 / u, b[1] as f64 / u, (b[0] + b[2]) as f64 / u, (b[1] + b[3]) as f64 / u);
        // Nothing left of or above the document can be scrolled into view, and nothing is
        // compared beyond the document's own extent.
        let (sx, sy) = self.info.scroll;
        let r = self.info.rect;
        x0 = x0.max(-sx).min(r[0]);
        y0 = y0.max(-sy).min(r[1]);
        if self.info.doc_size.0 > 0.0 {
            x1 = x1.min(self.info.doc_size.0 - sx).max(r[0] + r[2]);
            y1 = y1.min(self.info.doc_size.1 - sy).max(r[1] + r[3]);
        }
        [x0, y0, x1 - x0, y1 - y0]
    }
}

/// Collapse runs of collapsible white space to one space. With `keep_newlines`
/// (`white-space: pre-line`) line feeds survive and swallow the spaces around them.
pub fn collapse_ws(s: &str, keep_newlines: bool) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_space = false;
    for ch in s.chars() {
        match ch {
            '\n' if keep_newlines => {
                pending_space = false;
                while out.ends_with(' ') {
                    out.pop();
                }
                out.push('\n');
            }
            ' ' | '\t' | '\n' | '\r' | '\x0c' => pending_space = true,
            _ => {
                if pending_space && !(keep_newlines && (out.ends_with('\n'))) {
                    out.push(' ');
                }
                pending_space = false;
                out.push(ch);
            }
        }
    }
    if pending_space && !(keep_newlines && out.ends_with('\n')) {
        out.push(' ');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::collapse_ws;

    #[test]
    fn collapses_runs() {
        assert_eq!(collapse_ws("  a \n\t b  ", false), " a b ");
        assert_eq!(collapse_ws("\n", false), " ");
        assert_eq!(collapse_ws("a\u{a0}b", false), "a\u{a0}b");
    }

    #[test]
    fn pre_line_keeps_breaks() {
        assert_eq!(collapse_ws("a  \n  b", true), "a\nb");
        assert_eq!(collapse_ws("a\n\nb  c", true), "a\n\nb c");
    }
}
