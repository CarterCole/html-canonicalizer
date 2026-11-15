//! From the working tree to markup.
//!
//! `plan` walks the tree once and decides, for every element, its tag and the declarations
//! it needs: exactly the properties whose target value differs from what the element would
//! get anyway (the browser default for its tag, or the value inherited from its parent).
//! The same plan is then written out either as a quick trial document (inline styles) or as
//! the final document (shorthands folded, repeated blocks shared as classes).

use crate::css;
use crate::engine::Engine;
use crate::geom::{self, Rects};
use crate::model::*;
use crate::props::{self, F_LAYOUT, F_OUTER};
use crate::style::*;
use std::collections::HashMap;
use std::rc::Rc;

/// A `::before` / `::after`: its `content`, the declarations it needs, its target style.
pub type PseudoPlan = (String, Vec<(usize, Val)>, Style);

pub struct Open {
    pub name: String,
    pub ns: u8,
    pub attrs: Vec<(String, String)>,
    /// The style this element is meant to end up with.
    pub style: Style,
    pub decls: Vec<(usize, Val)>,
    /// `::before`, `::after`.
    pub pseudo: [Option<PseudoPlan>; 2],
    /// Where it must be drawn, relative to the root. `None`: draws nothing, any box will do.
    pub expect: Option<Rects>,
    pub void: bool,
    pub inner: Option<String>,
    pub node: usize,
    /// Default style of this tag, and per property whether it takes its parent's value.
    pub bare: Style,
    pub follow: Rc<[u8]>,
    /// Further pseudo-element rules: (name, declarations).
    pub extra: Vec<(&'static str, Vec<(usize, Val)>)>,
}

pub enum Ev {
    Open(Box<Open>),
    /// `seam`: an empty comment precedes it, keeping it a separate node from the text before.
    Text { data: String, expect: Option<Rects>, nodes: Vec<usize>, seam: bool },
    Close,
}

pub struct Plan {
    pub evs: Vec<Ev>,
    pub n_elems: usize,
    pub n_texts: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StyleMode {
    /// A class where a block repeats, an inline style where it does not.
    Auto,
    Inline,
    Classes,
}

pub struct Final {
    /// Complete standalone document.
    pub document: String,
    /// The root element's markup alone.
    pub html: String,
    /// The root element as a tree (see [`Engine::write_tree`]), JSON.
    pub tree: String,
    /// Rules for `html` (without `@font-face`).
    pub css: String,
    pub elements: usize,
    pub declarations: usize,
}

struct ParentCtx<'a> {
    style: &'a [Val],
    name: &'a str,
    ns: u8,
    inner: Inner,
}

impl Engine {
    fn border_w(&self, st: &[Val], side: usize) -> Option<f64> {
        match self.t.at(st, self.p.border_style[side]) {
            "none" | "hidden" | "" => Some(0.0),
            _ => parse_px(self.t.at(st, self.p.border_width[side])),
        }
    }

    /// The root is taken out of its page: what tied it to its surroundings is dropped and
    /// the size its surroundings gave it is written down.
    pub(crate) fn root_style(&mut self, e: &Elem) -> Style {
        let p = self.p.clone();
        let bare = self.bare_of(e).clone();
        let mut st = e.style.to_vec();
        let display = self.t.at(&st, p.display).to_string();
        let position = self.t.at(&st, p.position).to_string();
        for q in 0..st.len() {
            if self.flags[q] & F_OUTER != 0 && !(Some(q) == p.vertical_align && display == "table-cell") {
                st[q] = bare[q];
            }
        }
        let set = |eng: &mut Engine, st: &mut Vec<Val>, q: Option<usize>, v: &str| {
            if let Some(q) = q {
                st[q] = eng.t.intern(v);
            }
        };
        for q in p.margin {
            set(self, &mut st, q, "0px");
        }
        if matches!(position.as_str(), "relative" | "absolute" | "fixed" | "sticky") {
            set(self, &mut st, p.position, "relative");
        }
        let blockified = match display.as_str() {
            "inline-block" => Some("flow-root"),
            "inline-flex" => Some("flex"),
            "inline-grid" => Some("grid"),
            "inline-table" => Some("table"),
            _ => None,
        };
        if let Some(d) = blockified {
            set(self, &mut st, p.display, d);
        }
        if self.info.overflow_propagated {
            set(self, &mut st, p.overflow_x, "visible");
            set(self, &mut st, p.overflow_y, "visible");
        }
        if self.info.zoom > 0.0 && (self.info.zoom - 1.0).abs() > 1e-6 {
            let own: f64 = self.t.at(&st, p.zoom).parse().unwrap_or(1.0);
            let z = fmt_num(own * self.info.zoom);
            set(self, &mut st, p.zoom, &z);
        }

        if display != "inline" {
            if let Some((bw, bh)) = e.bbox {
                // The rect is exact (whole layout units) whenever nothing transformed it.
                let (bw, bh) = match geom::bounds(&e.rects) {
                    Some(b) => {
                        let (rw, rh) = (b[2] as f64 / geom::UNIT, b[3] as f64 / geom::UNIT);
                        (if (rw - bw).abs() < 0.02 { rw } else { bw }, if (rh - bh).abs() < 0.02 { rh } else { bh })
                    }
                    None => (bw, bh),
                };
                if !self.opts.fluid {
                    // The captured width already satisfies these.
                    for q in [p.min_width, p.max_width].into_iter().flatten() {
                        st[q] = bare[q];
                    }
                }
                let mut border_box = self.t.at(&st, p.box_sizing) == "border-box";
                // With only the width stated, `box-sizing` is a matter of spelling: use the default.
                let height_stated = [p.height, p.min_height, p.max_height, p.aspect_ratio, p.flex_basis].iter().any(|q| q.is_some_and(|q| st[q] != bare[q]));
                let px_known = [1usize, 3].iter().all(|&s| parse_px(self.t.at(&st, p.padding[s])).is_some() && self.border_w(&st, s).is_some());
                if border_box && !self.opts.fluid && !self.pin_h && !height_stated && px_known && self.t.at(&bare, p.box_sizing) == "content-box" {
                    if let Some(bs) = p.box_sizing {
                        st[bs] = bare[bs];
                        border_box = false;
                    }
                }
                let pin = |eng: &mut Engine, st: &mut Vec<Val>, prop: Option<usize>, size: f64, a: usize, b: usize| {
                    let Some(prop) = prop else { return };
                    let content = if border_box {
                        Some(size)
                    } else {
                        let sum = [a, b].iter().try_fold(0.0, |acc, &s| {
                            Some(acc + parse_px(eng.t.at(st, eng.p.padding[s]))? + eng.border_w(st, s)?)
                        });
                        sum.map(|s| (size - s).max(0.0))
                    };
                    match content {
                        Some(v) => {
                            let same = parse_px(eng.t.get(st[prop])).is_some_and(|cur| (cur - v).abs() < 0.01);
                            if !same {
                                st[prop] = eng.t.intern(&fmt_px(v));
                            }
                        }
                        None => {
                            if let Some(bs) = eng.p.box_sizing {
                                st[bs] = eng.t.intern("border-box");
                            }
                            st[prop] = eng.t.intern(&fmt_px(size));
                        }
                    }
                };
                if !self.opts.fluid {
                    pin(self, &mut st, p.width, bw, 1, 3);
                }
                if self.pin_h {
                    pin(self, &mut st, p.height, bh, 0, 2);
                }
            }
        }
        Rc::from(st)
    }

    /// Declarations an element needs given its tag's defaults and its parent's style, and
    /// whether any of them is more than layout.
    /// `follow[q]` is 1 where this kind of element takes its parent's value by default.
    pub(crate) fn decls(&self, st: &[Val], bare: &[Val], follow: &[u8], parent: Option<&[Val]>) -> (Vec<(usize, Val)>, bool) {
        let div = &self.base.div;
        let skip_ub = if self.info.bidi { None } else { self.p.unicode_bidi };
        let mut out = Vec::new();
        let mut paints = false;
        for q in 0..st.len() {
            let v = st[q];
            if follow[q] == 1 {
                if v != parent.map_or(div[q], |ps| ps[q]) {
                    out.push((q, v));
                }
            } else if v != bare[q] {
                if Some(q) == skip_ub {
                    continue;
                }
                // A default of `currentcolor` (an attribute says so) is met by the element's own colour.
                if bare[q] == self.v_current && self.p.color.is_some_and(|c| st[c] == v) {
                    continue;
                }
                out.push((q, v));
                if self.flags[q] & F_LAYOUT == 0 && !self.base.inherited[q] {
                    paints = true;
                }
            }
        }
        (out, paints)
    }

    pub(crate) fn pseudo_decls(&self, ps: &[Val], es: &[Val]) -> Vec<(usize, Val)> {
        use crate::hoist::Class;
        let span = &self.base.span;
        let skip_ub = if self.info.bidi { None } else { self.p.unicode_bidi };
        let display = self.t.at(ps, self.p.display);
        (0..ps.len())
            .filter(|&q| Some(q) != self.p.content && Some(q) != skip_ub)
            .filter_map(|q| {
                if self.base.inherited[q] {
                    // Inherited values that cannot show on this box are left to inheritance.
                    let shows = match self.pclass[q] {
                        Class::List => display == "list-item",
                        Class::Table => display.contains("table"),
                        Class::Svg => false,
                        _ => true,
                    };
                    (shows && ps[q] != es[q]).then_some((q, ps[q]))
                } else {
                    (ps[q] != span[q]).then_some((q, ps[q]))
                }
            })
            .collect()
    }

    /// Does the box draw anything in its own text colour (a border, outline, underline or
    /// column rule left at `currentcolor`)? Then `color` shows on it even without text.
    pub(crate) fn draws_in_current_color(&self, st: &[Val]) -> bool {
        let cur = self.v_current;
        let on = |style: Option<usize>, color: Option<usize>| {
            !matches!(self.t.at(st, style), "none" | "") && color.is_none_or(|c| st[c] == cur)
        };
        (0..4).any(|i| on(self.p.border_style[i], self.p.border_color[i]))
            || on(self.p.outline_style, self.p.outline_color)
            || on(self.p.td_line, self.p.td_color)
            || on(self.t.prop("column-rule-style"), self.t.prop("column-rule-color"))
    }

    pub(crate) fn visible(&self, st: &[Val]) -> bool {
        matches!(self.t.at(st, self.p.visibility), "visible" | "")
    }

    pub(crate) fn has_marker(&self, st: &[Val]) -> bool {
        self.t.at(st, self.p.display) == "list-item"
            && (!matches!(self.t.at(st, self.p.list_style_type), "none" | "") || !matches!(self.t.at(st, self.p.list_style_image), "none" | ""))
    }

    /// The plan for the deliverable: as `plan`, with inherited declarations placed where
    /// they are cheapest.
    pub fn plan_final(&mut self, w: &Work) -> Plan {
        let mut plan = self.plan_raw(w);
        if self.opts.hoist {
            self.hoist_inherited(&mut plan);
        }
        self.lang_attrs(&mut plan);
        plan
    }

    pub fn plan(&mut self, w: &Work) -> Plan {
        let mut plan = self.plan_raw(w);
        self.lang_attrs(&mut plan);
        plan
    }

    /// An element's language is an attribute in HTML, not a style: `-webkit-locale: "en"`
    /// becomes `lang="en"`.
    fn lang_attrs(&self, plan: &mut Plan) {
        let Some(lq) = self.t.prop("-webkit-locale") else { return };
        for ev in &mut plan.evs {
            let Ev::Open(o) = ev else { continue };
            let Some(pos) = o.decls.iter().position(|&(q, _)| q == lq) else { continue };
            let v = self.t.get(o.decls[pos].1);
            if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') && !v[1..v.len() - 1].contains('"') {
                o.attrs.push(("lang".to_string(), v[1..v.len() - 1].to_string()));
                o.decls.remove(pos);
            }
        }
    }

    fn plan_raw(&mut self, w: &Work) -> Plan {
        let root = match &w.nodes[w.root].kind {
            Kind::Elem(e) => e.clone(),
            Kind::Text(_) => unreachable!("the root is an element"),
        };
        let rs = self.root_style(&root);
        let mut evs = Vec::with_capacity(w.nodes.len() * 2);
        self.plan_elem(w, w.root, None, Some(rs), &mut evs);
        let n_elems = evs.iter().filter(|e| matches!(e, Ev::Open(_))).count();
        let n_texts = evs.iter().filter(|e| matches!(e, Ev::Text { .. })).count();
        Plan { evs, n_elems, n_texts }
    }

    fn plan_elem(&self, w: &Work, id: usize, parent: Option<&ParentCtx>, over: Option<Style>, evs: &mut Vec<Ev>) {
        let Some(e) = w.elem(id) else { return };
        let mut style: Style = over.unwrap_or_else(|| e.style.clone());
        // A flex or grid item is block-level whatever it asked for; say so, and a moved
        // inline-block needs no `display` at all.
        if let (Some(pc), Some(dq)) = (parent, self.p.display) {
            if matches!(pc.inner, Inner::Flex | Inner::Grid) {
                let blockified = match self.t.get(style[dq]) {
                    // (an item is its own formatting context anyway, so `flow-root` adds nothing)
                    "inline" | "inline-block" | "flow-root" => Some(self.c.block),
                    "inline-flex" => Some(self.c.flex),
                    "inline-grid" => Some(self.c.grid),
                    "inline-table" => Some(self.c.table),
                    _ => None,
                };
                if let Some(v) = blockified {
                    let mut st = style.to_vec();
                    st[dq] = v;
                    style = Rc::from(st);
                }
            }
        }
        let (ox, oy) = self.origin;
        let tol = self.opts.tol;

        // Tag: a kept tag stays itself when the parser will accept it there.
        let kept = match &e.tag {
            Tag::Keep(n) if e.ns != 0 => Some(n.as_str()),
            Tag::Keep(n) => match props::required_parent(n) {
                Some(req) if !parent.is_some_and(|p| p.ns == 0 && req.contains(&p.name)) => None,
                _ => Some(n.as_str()),
            },
            Tag::Auto => None,
        };
        let display = self.t.at(&style, self.p.display);
        let (name, bare): (&str, &Style) = match kept {
            Some(n) => (n, self.bare_of(e)),
            None => {
                let skip_ub = if self.info.bidi { None } else { self.p.unicode_bidi };
                let cost = |b: &Style| self.base.divspan.iter().filter(|&&q| Some(q) != skip_ub && style[q] != b[q]).count();
                let (cd, cs) = (cost(&self.base.div), cost(&self.base.span));
                let inline = parse_display(display).0 == Outer::Inline;
                if cs < cd || (cs == cd && inline) {
                    ("span", &self.base.span)
                } else {
                    ("div", &self.base.div)
                }
            }
        };
        let follow: &[u8] = if kept.is_some() { self.follow_of(e) } else { &self.base.follow[0] };
        let (mut decls, nonlayout) = self.decls(&style, bare, follow, parent.map(|p| p.style));
        // A kept tag that is block-level by rule (flex/grid item, positioned, floated) need
        // not say so.
        if let (Some(_), Some(dq)) = (kept, self.p.display) {
            let by_rule = parent.is_some_and(|pc| matches!(pc.inner, Inner::Flex | Inner::Grid))
                || matches!(self.t.at(&style, self.p.position), "absolute" | "fixed")
                || !matches!(self.t.at(&style, self.p.float), "none" | "");
            if by_rule {
                let implied = match self.t.get(bare[dq]) {
                    "inline" | "inline-block" => Some("block"),
                    "inline-flex" => Some("flex"),
                    "inline-grid" => Some("grid"),
                    "inline-table" => Some("table"),
                    _ => None,
                };
                if let Some(implied) = implied {
                    decls.retain(|&(q, v)| !(q == dq && self.t.get(v) == implied));
                }
            }
        }
        let attrs = if kept.is_some() { e.attrs.clone() } else { Vec::new() };
        let visible = self.visible(&style);
        let paints = !e.unchecked && visible && (kept.is_some() || nonlayout || self.has_marker(&style) || e.has_pseudo());
        let expect = paints.then(|| geom::merge_lines(&geom::translate(&e.rects, ox, oy), tol));
        let mut pseudo: [Option<PseudoPlan>; 2] = [None, None];
        for k in 0..2 {
            if let Some(ps) = &e.pseudo[k] {
                pseudo[k] = Some((ps.content.clone(), self.pseudo_decls(&ps.style, &style), ps.style.clone()));
            }
        }
        let ns = e.ns;
        evs.push(Ev::Open(Box::new(Open {
            name: name.to_string(),
            ns,
            attrs,
            style: style.clone(),
            decls,
            pseudo,
            expect,
            void: ns == 0 && kept.is_some() && props::is_void(name),
            inner: e.inner.clone(),
            node: id,
            bare: bare.clone(),
            follow: if kept.is_some() { self.follow_of(e).clone() } else { self.base.follow[0].clone() },
            extra: if kept.is_some() { e.extra.clone() } else { Vec::new() },
        })));
        if !e.opaque {
            let ctx = ParentCtx { style: &style, name, ns, inner: parse_display(display).1 };
            let check_text = visible && !e.unchecked;
            let mut run: Option<(String, Rects, Vec<usize>, bool)> = None;
            let flush = |run: &mut Option<(String, Rects, Vec<usize>, bool)>, evs: &mut Vec<Ev>| {
                if let Some((data, rects, nodes, seam)) = run.take() {
                    if !data.is_empty() {
                        let expect = check_text.then(|| geom::merge_lines(&geom::translate(&rects, ox, oy), tol));
                        // A seam only means something after another text node.
                        let seam = seam && matches!(evs.last(), Some(Ev::Text { .. }));
                        evs.push(Ev::Text { data, expect, nodes, seam });
                    }
                }
            };
            for &k in &w.nodes[id].kids {
                match &w.nodes[k].kind {
                    Kind::Text(tx) => {
                        if tx.sep && run.is_some() {
                            flush(&mut run, evs);
                        }
                        let r = run.get_or_insert_with(|| (String::new(), Vec::new(), Vec::new(), tx.sep));
                        r.0.push_str(&tx.data);
                        r.1.extend_from_slice(&tx.rects);
                        r.2.push(k);
                    }
                    Kind::Elem(_) => {
                        flush(&mut run, evs);
                        self.plan_elem(w, k, Some(&ctx), None, evs);
                    }
                }
            }
            flush(&mut run, evs);
        }
        evs.push(Ev::Close);
    }

    /// Fast document for a layout trial: every declaration inline, values as computed.
    /// Returns (markup, css).
    pub fn render_trial(&self, plan: &Plan) -> (String, String) {
        let (attach, css) = self.trial_attach(plan);
        (self.write_html(plan, &attach), css)
    }

    /// The same trial as a tree for DOM construction (see [`Engine::write_tree`]). Returns (tree JSON, css).
    pub fn render_trial_tree(&self, plan: &Plan) -> (String, String) {
        let (attach, css) = self.trial_attach(plan);
        (self.write_tree(plan, &attach).to_string(), css)
    }

    /// Per element (class, inline style) for a trial, and the rules the classes need.
    fn trial_attach(&self, plan: &Plan) -> (Vec<Attach>, String) {
        let mut css = String::new();
        let mut attach = Vec::with_capacity(plan.n_elems);
        let mut pc = 0usize;
        let decl_text = |decls: &[(usize, Val)], out: &mut String| {
            for &(q, v) in decls {
                out.push_str(&self.t.props[q]);
                out.push(':');
                out.push_str(self.t.get(v));
                out.push(';');
            }
        };
        for ev in &plan.evs {
            let Ev::Open(o) = ev else { continue };
            let style = if o.decls.is_empty() {
                None
            } else {
                let mut st = String::new();
                decl_text(&o.decls, &mut st);
                Some(st)
            };
            let mut cls = None;
            if o.pseudo.iter().any(Option::is_some) || !o.extra.is_empty() {
                let c = format!("q{pc}");
                pc += 1;
                for (k, ps) in o.pseudo.iter().enumerate() {
                    let Some((content, decls, _)) = ps else { continue };
                    css.push_str(&format!(".{c}::{}{{content:{content};", if k == 0 { "before" } else { "after" }));
                    decl_text(decls, &mut css);
                    css.push_str("}\n");
                }
                for (name, decls) in &o.extra {
                    css.push_str(&format!(".{c}::{name}{{"));
                    decl_text(decls, &mut css);
                    css.push_str("}\n");
                }
                cls = Some(c);
            }
            attach.push((cls, style));
        }
        (attach, css)
    }

    /// Elements and text the HTML parser would not leave where the plan puts them (an
    /// element other than a row inside <tbody>, text directly in a <tr>...). The tree form
    /// can express these; markup cannot, so a result that has them is not what it claims.
    pub fn markup_problems(&self, plan: &Plan) -> usize {
        let allowed = |parent: &str, child: &str| -> bool {
            match parent {
                "table" => matches!(child, "caption" | "colgroup" | "thead" | "tbody" | "tfoot"),
                "thead" | "tbody" | "tfoot" => child == "tr",
                "tr" => matches!(child, "td" | "th"),
                "colgroup" => child == "col",
                "select" => matches!(child, "option" | "optgroup" | "hr"),
                _ => true,
            }
        };
        let mut stack: Vec<(&str, u8)> = Vec::new();
        let mut bad = 0;
        for ev in &plan.evs {
            match ev {
                Ev::Open(o) => {
                    if let Some(&(parent, ns)) = stack.last() {
                        if ns == 0 && o.ns == 0 && !allowed(parent, &o.name) {
                            bad += 1;
                        }
                    }
                    stack.push((&o.name, o.ns));
                }
                Ev::Text { data, .. } => {
                    if let Some(&(parent, 0)) = stack.last() {
                        if matches!(parent, "table" | "thead" | "tbody" | "tfoot" | "tr" | "colgroup") && data.chars().any(|c| !c.is_ascii_whitespace()) {
                            bad += 1;
                        }
                    }
                }
                Ev::Close => {
                    stack.pop();
                }
            }
        }
        bad
    }

    /// The plan as markup, each element with its (class, inline style).
    fn write_html(&self, plan: &Plan, attach: &[Attach]) -> String {
        let mut html = String::with_capacity(plan.evs.len() * 64);
        let mut stack: Vec<&Open> = Vec::new();
        let mut i = 0;
        for ev in &plan.evs {
            match ev {
                Ev::Open(o) => {
                    html.push('<');
                    html.push_str(&o.name);
                    let (cls, inline) = &attach[i];
                    i += 1;
                    if let Some(c) = cls {
                        push_attr(&mut html, "class", c);
                    }
                    for (k, v) in &o.attrs {
                        push_attr(&mut html, k, v);
                    }
                    if let Some(s) = inline {
                        push_attr(&mut html, "style", s);
                    }
                    html.push('>');
                    if let Some(inner) = &o.inner {
                        // The parser drops one newline right after <textarea>: keep the value's own.
                        if o.ns == 0 && o.name == "textarea" && inner.starts_with('\n') {
                            html.push('\n');
                        }
                        html.push_str(inner);
                    }
                    stack.push(o);
                }
                Ev::Text { data, seam, .. } => {
                    if *seam {
                        html.push_str("<!---->");
                    }
                    push_escaped(&mut html, data, false);
                }
                Ev::Close => {
                    if let Some(o) = stack.pop() {
                        if !o.void {
                            html.push_str("</");
                            html.push_str(&o.name);
                            html.push('>');
                        }
                    }
                }
            }
        }
        html
    }

    /// The plan as a tree, for building with DOM calls where markup cannot be used (a page
    /// whose security policy refuses `innerHTML` or style attributes). An element is
    /// `{"t": tag, "n": namespace (1 SVG, 2 MathML; absent for HTML), "a": [[name, value]],
    /// "s": inline style, "k": [children], "v": textarea value, "o": [[option text, selected]]}`;
    /// a text node is a string; `0` is an empty comment (a text seam).
    pub fn write_tree(&self, plan: &Plan, attach: &[Attach]) -> serde_json::Value {
        use serde_json::{json, Map, Value};
        let mut stack: Vec<Map<String, Value>> = Vec::new();
        let mut kids: Vec<Vec<Value>> = vec![Vec::new()];
        let mut i = 0;
        for ev in &plan.evs {
            match ev {
                Ev::Open(o) => {
                    let (cls, inline) = &attach[i];
                    i += 1;
                    let mut m = Map::new();
                    m.insert("t".into(), json!(o.name));
                    if o.ns != 0 {
                        m.insert("n".into(), json!(o.ns));
                    }
                    let mut attrs: Vec<Value> = Vec::new();
                    if let Some(c) = cls {
                        attrs.push(json!(["class", c]));
                    }
                    for (k, v) in &o.attrs {
                        attrs.push(json!([k, v]));
                    }
                    if !attrs.is_empty() {
                        m.insert("a".into(), Value::Array(attrs));
                    }
                    if let Some(st) = inline {
                        m.insert("s".into(), json!(st));
                    }
                    if let Some(inner) = &o.inner {
                        if o.name == "textarea" {
                            m.insert("v".into(), json!(unescape(inner)));
                        } else if o.name == "select" {
                            let opts: Vec<Value> = parse_options(inner).into_iter().map(|(t, sel)| json!([t, sel])).collect();
                            m.insert("o".into(), Value::Array(opts));
                        }
                    }
                    stack.push(m);
                    kids.push(Vec::new());
                }
                Ev::Text { data, seam, .. } => {
                    let top = kids.last_mut().expect("text inside an element");
                    if *seam {
                        top.push(json!(0));
                    }
                    top.push(json!(data));
                }
                Ev::Close => {
                    let mut m = stack.pop().expect("balanced plan");
                    let k = kids.pop().unwrap_or_default();
                    if !k.is_empty() {
                        m.insert("k".into(), Value::Array(k));
                    }
                    kids.last_mut().expect("balanced plan").push(Value::Object(m));
                }
            }
        }
        kids.pop().and_then(|mut v| v.pop()).unwrap_or(Value::Null)
    }

    fn body(&self, decls: &[(usize, Val)], style: Option<&[Val]>, fold: bool) -> String {
        let mut d: Vec<(String, String)> = decls.iter().map(|&(q, v)| (self.t.props[q].clone(), css::minify_value(self.t.get(v)))).collect();
        if fold {
            if let Some(st) = style {
                let target = |name: &str| self.t.prop(name).map(|q| css::minify_value(self.t.get(st[q])));
                css::fold(&mut d, &target);
            }
        }
        let mut s = String::new();
        for (i, (n, v)) in d.iter().enumerate() {
            if i > 0 {
                s.push(';');
            }
            s.push_str(n);
            s.push(':');
            s.push_str(v);
        }
        s
    }

    /// The deliverable: shorthands folded, values minified, repeated blocks shared as classes.
    pub fn render_final(&self, plan: &Plan, mode: StyleMode, fold: bool, prefix: &str) -> Final {
        struct Item {
            body: String,
            pseudo: [Option<String>; 2],
            /// Complete extra rules, with NUL standing for the class selector.
            extra: String,
        }
        let mut items: Vec<Item> = Vec::with_capacity(plan.n_elems);
        let mut declarations = 0;
        for ev in &plan.evs {
            let Ev::Open(o) = ev else { continue };
            declarations += o.decls.len();
            let mut pseudo: [Option<String>; 2] = [None, None];
            for k in 0..2 {
                if let Some((content, d, st)) = &o.pseudo[k] {
                    declarations += d.len() + 1;
                    let b = self.body(d, Some(st), fold);
                    pseudo[k] = Some(if b.is_empty() { format!("content:{content}") } else { format!("content:{content};{b}") });
                }
            }
            let mut extra = String::new();
            for (name, d) in &o.extra {
                declarations += d.len();
                extra.push_str(&format!("\u{0}::{name}{{{}}}\n", self.body(d, None, false)));
            }
            items.push(Item { body: self.body(&o.decls, Some(&o.style), fold), pseudo, extra });
        }
        let key = |it: &Item| format!("{}\u{1}{}\u{1}{}\u{1}{}", it.body, it.pseudo[0].as_deref().unwrap_or(""), it.pseudo[1].as_deref().unwrap_or(""), it.extra);
        let mut uses: HashMap<String, usize> = HashMap::new();
        for it in &items {
            *uses.entry(key(it)).or_insert(0) += 1;
        }
        let mut class_of: HashMap<String, String> = HashMap::new();
        let mut rules = String::new();
        let mut next = 0usize;
        // (class, inline style) per element
        let mut attach: Vec<Attach> = Vec::with_capacity(items.len());
        for it in &items {
            let has_pseudo = it.pseudo.iter().any(Option::is_some) || !it.extra.is_empty();
            if it.body.is_empty() && !has_pseudo {
                attach.push((None, None));
                continue;
            }
            let k = key(it);
            let n = uses[&k];
            let as_class = has_pseudo
                || match mode {
                    StyleMode::Classes => true,
                    StyleMode::Inline => false,
                    // ` style="…"` on each use against one rule plus ` class="x"` on each use
                    StyleMode::Auto => n >= 2 && n * (9 + it.body.len()) > it.body.len() + 6 + n * 11,
                };
            if !as_class {
                attach.push((None, Some(it.body.clone())));
                continue;
            }
            let own_inline = has_pseudo && mode == StyleMode::Inline && !it.body.is_empty();
            let ck = if own_inline { format!("\u{2}{}\u{1}{}\u{1}{}", it.pseudo[0].as_deref().unwrap_or(""), it.pseudo[1].as_deref().unwrap_or(""), it.extra) } else { k };
            let cls = match class_of.get(&ck) {
                Some(c) => c.clone(),
                None => {
                    let c = format!("{prefix}{}", class_name(next));
                    next += 1;
                    if !own_inline && !it.body.is_empty() {
                        rules.push_str(&format!(".{c}{{{}}}\n", it.body));
                    }
                    for (i, ps) in it.pseudo.iter().enumerate() {
                        if let Some(b) = ps {
                            rules.push_str(&format!(".{c}::{}{{{b}}}\n", if i == 0 { "before" } else { "after" }));
                        }
                    }
                    rules.push_str(&it.extra.replace('\u{0}', &format!(".{c}")));
                    class_of.insert(ck, c.clone());
                    c
                }
            };
            attach.push((Some(cls), own_inline.then(|| it.body.clone())));
        }

        let html = self.write_html(plan, &attach);
        let tree = self.write_tree(plan, &attach).to_string();

        let mut document = String::new();
        let doctype = self.doctype();
        if !doctype.is_empty() {
            document.push_str(&doctype);
            document.push('\n');
        }
        document.push_str("<meta charset=\"utf-8\">\n");
        if !self.font_css.is_empty() || !rules.is_empty() {
            document.push_str("<style>\n");
            document.push_str(&self.font_css);
            document.push_str(&rules);
            document.push_str("</style>\n");
        }
        // The backdrop is always stated, white included: an explicit page background is what
        // the element was photographed over, and antialiased edges blend against it.
        let c = self.info.backdrop.color;
        let body_style = format!("margin:0;background:{}", css::minify_value(&format!("rgb({}, {}, {})", c[0], c[1], c[2])));
        document.push_str(&format!("<body style=\"{body_style}\">"));
        document.push_str(&html);
        document.push_str("</body>\n");
        Final { document, html, tree, css: rules, elements: plan.n_elems, declarations }
    }
}

/// (class, inline style) of one element.
pub type Attach = (Option<String>, Option<String>);

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&amp;", "&")
}

/// The `<option>`s the page agent wrote for a `<select>`: (text, selected).
fn parse_options(inner: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut rest = inner;
    while let Some(start) = rest.find("<option") {
        let after = &rest[start + 7..];
        let Some(gt) = after.find('>') else { break };
        let selected = after[..gt].contains("selected");
        let body = &after[gt + 1..];
        let end = body.find("</option>").unwrap_or(body.len());
        out.push((unescape(&body[..end]), selected));
        rest = &body[(end + 9).min(body.len())..];
    }
    out
}

/// a, b, ... z, aa, ab, ...
fn class_name(mut n: usize) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'a' + (n % 26) as u8);
        n /= 26;
        if n == 0 {
            break;
        }
        n -= 1;
    }
    s.reverse();
    String::from_utf8(s).unwrap()
}

fn push_escaped(out: &mut String, s: &str, attr: bool) {
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' if !attr => out.push_str("&lt;"),
            '"' if attr => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
}

fn push_attr(out: &mut String, name: &str, value: &str) {
    out.push(' ');
    out.push_str(name);
    if value.is_empty() {
        return;
    }
    out.push('=');
    // Prefer the quote the value does not contain, so font names stay readable.
    if value.contains('"') && !value.contains('\'') {
        out.push('\'');
        for ch in value.chars() {
            if ch == '&' {
                out.push_str("&amp;");
            } else {
                out.push(ch);
            }
        }
        out.push('\'');
    } else {
        out.push('"');
        push_escaped(out, value, true);
        out.push('"');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_and_text_round_trip() {
        assert_eq!(parse_options("<option>a &amp; b</option><option selected>c &lt; d</option>"), vec![("a & b".into(), false), ("c < d".into(), true)]);
        assert_eq!(unescape("x &lt;y&gt; &amp;amp;"), "x <y> &amp;");
    }

    #[test]
    fn class_names_are_short_and_unique() {
        assert_eq!(class_name(0), "a");
        assert_eq!(class_name(25), "z");
        assert_eq!(class_name(26), "aa");
        assert_eq!(class_name(27), "ab");
        assert_eq!(class_name(26 + 26 * 26), "aaa");
        let mut seen = std::collections::HashSet::new();
        for i in 0..5000 {
            assert!(seen.insert(class_name(i)));
        }
    }

    #[test]
    fn attributes_pick_a_safe_quote() {
        let mut s = String::new();
        push_attr(&mut s, "style", "font-family:\"Helvetica Neue\"");
        assert_eq!(s, " style='font-family:\"Helvetica Neue\"'");
        let mut s = String::new();
        push_attr(&mut s, "alt", "a \"b\" & 'c'");
        assert_eq!(s, " alt=\"a &quot;b&quot; &amp; 'c'\"");
        let mut s = String::new();
        push_attr(&mut s, "checked", "");
        assert_eq!(s, " checked");
    }
}
