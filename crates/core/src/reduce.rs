//! Element reduction: propose edits, let the browser judge them.
//!
//! Nothing here decides on its own that an element is redundant. The static checks only
//! pick *candidates* (cheaply, and optimistically); a candidate is kept only if a real
//! render of the edited tree puts every painted box and every line of text exactly where
//! the original had it. Batches of candidates are tried together and bisected on failure.

use crate::emit::{Ev, Plan};
use crate::engine::Engine;
use crate::geom::{self, Rects, UNIT};
use crate::model::*;
use crate::props::{F_CONTAINER, F_LAYOUT, F_OUTER};
use crate::style::*;
use serde::Deserialize;
use std::collections::HashSet;
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Op {
    /// Replace an element by its children.
    Unwrap,
    /// Remove a subtree that draws nothing.
    Delete,
    /// A parent absorbs its only child (the child was a pure layout shell).
    MergeDown,
    /// An only child takes its parent's place (the parent was a pure layout shell).
    MergeUp,
    /// Replace a subtree that draws nothing but takes space by one empty box of that size.
    Hollow,
    /// Join an inline element with an identical next sibling.
    Join,
    /// Remove a subtree that draws, but where nothing of it can be seen (clipped away,
    /// parked off-page). Only a screenshot can vouch for this one.
    Unseen,
    /// Let a run of text join the run before it (drop the seam an earlier edit left).
    Fuse,
}

/// Removing an element that stood between two runs of text would make them one run, and
/// one run is shaped as a whole: glyphs after the join can move by a fraction of a layout
/// unit - not enough to move a box, enough to change antialiasing. So structural edits
/// leave a *seam*: the two runs stay separate text nodes (an empty comment between them),
/// laid out exactly as before. `Fuse` then removes each seam the picture does not need.
#[derive(Clone, Copy, Debug)]
pub struct Edit {
    pub op: Op,
    pub node: usize,
}

impl Edit {
    /// Geometry alone cannot justify removing something that paints.
    fn needs_pixels(&self) -> bool {
        self.op == Op::Unseen
    }

    /// Can change the picture without moving a box; judged by pixels when there are any.
    fn wants_pixels(&self) -> bool {
        self.op == Op::Fuse
    }
}

/// One document to render in the replica page.
pub struct Doc<'a> {
    pub html: &'a str,
    pub css: &'a str,
    /// Where the stage puts the root, in px.
    pub x: f64,
    pub y: f64,
    /// Stage width, for roots that flow with their container.
    pub w: Option<f64>,
    /// Flat colour behind the element, as CSS.
    pub bg: &'a str,
    /// The document as a tree (JSON, see `Engine::write_tree`) instead of markup, when the
    /// engine was asked for trees (`Options::tree`); `html` is then empty.
    pub tree: Option<&'a str>,
    /// Where this trial can differ visibly from the last accepted tree: x, y, width, height
    /// in px from the root's top-left. `None`: anywhere.
    pub region: Option<[f64; 4]>,
}

/// The browser, as far as the reducer is concerned.
pub trait Renderer {
    /// Lay the document out and report every node's boxes in document order.
    fn render(&mut self, doc: &Doc) -> Result<Measure, String>;

    /// Second opinion on the document just rendered, asked only once its geometry matched:
    /// does it also *look* the same? `None` when the renderer has no way to tell.
    fn accept(&mut self, _doc: &Doc, _measure: &Measure) -> Result<Option<bool>, String> {
        Ok(None)
    }

    /// After `accept` said no: which of these regions (same coordinates as `Doc::region`)
    /// contain a visible difference?
    fn differs(&mut self, regions: &[[f64; 4]]) -> Vec<bool> {
        vec![false; regions.len()]
    }

    /// After `accept` said no: up to `max` points where the picture differs, in px from the
    /// root's top-left.
    fn diff_points(&mut self, _max: usize) -> Vec<(f64, f64)> {
        Vec::new()
    }

    /// The caller's time budget is spent: stop searching and keep what is verified so far.
    fn out_of_time(&self) -> bool {
        false
    }

    /// Can `accept` compare pictures? Without that, edits only pixels can justify (removing
    /// what is clipped away, joining text runs) are not attempted.
    fn has_pixels(&self) -> bool {
        false
    }
}

enum Outcome {
    Committed,
    /// Something moved.
    Geometry,
    /// It moved or looks different, and these are the edits in whose blocks it shows
    /// (possibly none: then the cause is unknown).
    Blamed(Vec<Edit>),
}

#[derive(Deserialize)]
struct MeasureRaw {
    e: usize,
    t: usize,
    m: Vec<i32>,
}

pub struct Measure {
    pub elems: usize,
    pub texts: usize,
    /// (is text, rects) in document order.
    pub nodes: Vec<(bool, Rects)>,
}

impl Measure {
    pub fn parse(json: &str) -> Result<Measure, String> {
        let raw: MeasureRaw = serde_json::from_str(json).map_err(|e| format!("unreadable measurement: {e}"))?;
        let mut nodes = Vec::with_capacity(raw.e + raw.t);
        let mut i = 0;
        while i + 1 < raw.m.len() {
            let is_text = raw.m[i] == 1;
            let n = raw.m[i + 1] as usize;
            let end = i + 2 + n * 4;
            if end > raw.m.len() {
                return Err("truncated measurement".into());
            }
            nodes.push((is_text, geom::from_flat(&raw.m[i + 2..end])));
            i = end;
        }
        Ok(Measure { elems: raw.e, texts: raw.t, nodes })
    }
}

#[derive(Debug, Clone)]
pub struct Mismatch {
    /// Position in document order.
    pub index: usize,
    pub nodes: Vec<usize>,
    pub text: bool,
    pub expected: Rects,
    pub got: Rects,
}

#[derive(Debug)]
pub enum CompareError {
    /// The parser built a different tree than the one written.
    Structure { want: (usize, usize), got: (usize, usize) },
    Geometry(Vec<Mismatch>),
}

pub(crate) struct Traits {
    /// Every non-inherited property is at its default: the element is only a box in the flow.
    pub neutral: bool,
    /// Only layout properties are set: it positions things but draws nothing.
    pub paintless: bool,
    pub outer: Outer,
    pub inner: Inner,
}

fn collapse(a: f64, b: f64) -> f64 {
    if a >= 0.0 && b >= 0.0 {
        a.max(b)
    } else if a < 0.0 && b < 0.0 {
        a.min(b)
    } else {
        a + b
    }
}

impl Engine {
    fn px(&self, st: &[Val], q: Option<usize>) -> Option<f64> {
        parse_px(self.t.at(st, q))
    }

    fn bw(&self, st: &[Val], side: usize) -> Option<f64> {
        match self.t.at(st, self.p.border_style[side]) {
            "none" | "hidden" | "" => Some(0.0),
            _ => self.px(st, self.p.border_width[side]),
        }
    }

    fn set_px(&mut self, st: &mut [Val], q: Option<usize>, v: f64) {
        if let Some(q) = q {
            st[q] = self.t.intern(&fmt_px(v));
        }
    }

    pub(crate) fn traits(&self, e: &Elem, st: &[Val]) -> Traits {
        let (div, span) = (&self.base.div, &self.base.span);
        let display = self.t.at(st, self.p.display);
        let (outer, inner) = parse_display(display);
        let mut neutral = matches!(display, "block" | "inline" | "contents");
        let mut paintless = true;
        if inner != Inner::Contents {
            for q in 0..st.len() {
                if self.base.inherited[q] || Some(q) == self.p.display {
                    continue;
                }
                let v = st[q];
                if v == div[q] || v == span[q] {
                    continue;
                }
                neutral = false;
                if self.flags[q] & F_LAYOUT == 0 {
                    paintless = false;
                    break;
                }
            }
        }
        if e.has_pseudo() || e.tag != Tag::Auto || self.has_marker(st) {
            neutral = false;
            paintless = false;
        }
        Traits { neutral, paintless, outer, inner }
    }

    /// Compare a render against what the plan expects, relative to each root's top-left.
    pub fn compare(&self, plan: &Plan, m: &Measure, limit: usize) -> Result<(), CompareError> {
        if m.elems != plan.n_elems || m.texts != plan.n_texts || m.nodes.len() != plan.n_elems + plan.n_texts {
            return Err(CompareError::Structure { want: (plan.n_elems, plan.n_texts), got: (m.elems, m.texts) });
        }
        let (mx, my) = match m.nodes.first().and_then(|(_, r)| geom::bounds(r)) {
            Some(b) => (b[0], b[1]),
            None => return Err(CompareError::Structure { want: (plan.n_elems, plan.n_texts), got: (0, 0) }),
        };
        let tol = self.opts.tol;
        let mut bad = Vec::new();
        let mut i = 0;
        for ev in &plan.evs {
            let (expect, text, nodes) = match ev {
                Ev::Open(o) => (&o.expect, false, vec![o.node]),
                Ev::Text { expect, nodes, .. } => (expect, true, nodes.clone()),
                Ev::Close => continue,
            };
            let (is_text, rects) = &m.nodes[i];
            if *is_text != text {
                return Err(CompareError::Structure { want: (plan.n_elems, plan.n_texts), got: (m.elems, m.texts) });
            }
            if let Some(exp) = expect {
                let got = geom::merge_lines(&geom::translate(rects, mx, my), tol);
                // Each run fused into this one may have rounded its width up by one unit.
                let tol = tol + nodes.len().saturating_sub(1) as i32;
                if !geom::eq(exp, &got, tol) {
                    bad.push(Mismatch { index: i, nodes, text, expected: exp.clone(), got });
                    if bad.len() >= limit {
                        break;
                    }
                }
            }
            i += 1;
        }
        if bad.is_empty() {
            Ok(())
        } else {
            Err(CompareError::Geometry(bad))
        }
    }

    /// Render `w` and judge it: (geometry matches, looks the same). `pixels`: 0 geometry
    /// only, 1 also ask for the renderer's second opinion if it has one, 2 require it.
    fn trial(&mut self, w: &Work, r: &mut dyn Renderer, pixels: u8, region: Option<[f64; 4]>) -> Result<(Plan, Measure, bool, bool), String> {
        let (plan, m, bad, looks) = self.trial_detail(w, r, pixels, region, 1)?;
        Ok((plan, m, bad.is_none(), looks))
    }

    /// As `trial`, returning up to `limit` of the boxes that moved.
    fn trial_detail(&mut self, w: &Work, r: &mut dyn Renderer, pixels: u8, region: Option<[f64; 4]>, limit: usize) -> Result<(Plan, Measure, Option<CompareError>, bool), String> {
        self.stats.trials += 1;
        let plan = self.plan(w);
        let (body, css) = if self.opts.tree { self.render_trial_tree(&plan) } else { self.render_trial(&plan) };
        let css = format!("{}{}", self.font_css, css);
        let (x, y) = self.target_origin();
        let bg = self.backdrop_css();
        let (html, tree) = if self.opts.tree { ("", Some(body.as_str())) } else { (body.as_str(), None) };
        let doc = Doc { html, css: &css, x, y, w: self.stage_width(), bg: &bg, region, tree };
        let m = r.render(&doc)?;
        let bad = self.compare(&plan, &m, limit).err();
        let mut looks = true;
        if bad.is_none() && pixels > 0 {
            looks = match r.accept(&doc, &m)? {
                Some(v) => v,
                None => pixels < 2,
            };
        }
        Ok((plan, m, bad, looks))
    }

    /// The nearest ancestor-or-self that is not part of a line.
    fn block_of(&self, w: &Work, mut a: usize) -> usize {
        loop {
            let inline = match w.elem(a) {
                Some(el) => parse_display(self.t.at(&el.style, self.p.display)).0 == Outer::Inline,
                None => true,
            };
            match w.nodes[a].parent {
                Some(p) if inline && a != w.root => a = p,
                _ => return a,
            }
        }
    }

    /// The current tree as a trial document, for the caller's own checks.
    pub fn trial_markup(&mut self) -> (Plan, String, String) {
        let w = std::mem::take(&mut self.work);
        let out = self.markup_of(&w);
        self.work = w;
        out
    }

    /// Any tree as a trial document.
    pub fn markup_of(&mut self, w: &Work) -> (Plan, String, String) {
        let plan = self.plan(w);
        let (html, css) = self.render_trial(&plan);
        let css = format!("{}{}", self.font_css, css);
        (plan, html, css)
    }

    /// Everything a subtree draws, in px from the root's top-left (`None`: it has no boxes).
    fn subtree_region(&self, w: &Work, id: usize) -> Option<[f64; 4]> {
        let mut b = None;
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            let rects = match &w.nodes[n].kind {
                Kind::Elem(e) => &e.rects,
                Kind::Text(t) => &t.rects,
            };
            b = geom::union(b, geom::bounds(rects));
            stack.extend(w.nodes[n].kids.iter().copied());
        }
        let b = b?;
        let (ox, oy) = self.origin;
        let px = |v: i32| v as f64 / UNIT;
        Some([px(b[0] - ox) - 4.0, px(b[1] - oy) - 4.0, px(b[2]) + 8.0, px(b[3]) + 8.0])
    }

    pub fn backdrop_css(&self) -> String {
        let c = self.info.backdrop.color;
        format!("rgb({}, {}, {})", c[0], c[1], c[2])
    }

    /// Stage width: only a root that flows with its container needs one.
    pub fn stage_width(&self) -> Option<f64> {
        let display = self.work.elem(self.work.root).map(|e| self.t.at(&e.style, self.p.display).to_string()).unwrap_or_default();
        (self.opts.fluid || display == "inline").then_some(self.info.parent_width)
    }

    fn check(&mut self, w: &Work, r: &mut dyn Renderer, need_pixels: bool) -> Result<bool, String> {
        let level = if need_pixels { 2 } else if self.opts.strict { 1 } else { 0 };
        let (_, _, geometry, looks) = self.trial(w, r, level, None)?;
        Ok(geometry && looks)
    }

    /// The block a text node's line sits in, in px from the root's top-left: where fusing it
    /// with the text before it could show.
    fn fuse_region(&self, w: &Work, text: usize) -> Option<[f64; 4]> {
        let mut a = w.nodes[text].parent?;
        loop {
            let el = w.elem(a)?;
            let inline = parse_display(self.t.at(&el.style, self.p.display)).0 == Outer::Inline;
            if !inline || a == w.root {
                break;
            }
            a = w.nodes[a].parent?;
        }
        let b = geom::bounds(&w.elem(a)?.rects)?;
        let (ox, oy) = self.origin;
        let px = |v: i32| v as f64 / UNIT;
        Some([px(b[0] - ox) - 2.0, px(b[1] - oy) - 2.0, px(b[2]) + 4.0, px(b[3]) + 4.0])
    }

    /// Render the unreduced tree. Where the faithful copy itself cannot match the original
    /// (a feature this tool does not reproduce), its own geometry becomes the reference, so
    /// the reduction is still exact relative to the copy; the count is reported.
    pub fn calibrate(&mut self, r: &mut dyn Renderer) -> Result<Vec<Mismatch>, String> {
        let w = self.work.clone();
        let structure = |want: (usize, usize), got: (usize, usize)| {
            format!(
                "the HTML parser rebuilt the copy differently ({} elements / {} text nodes written, {} / {} parsed); the source DOM nests elements in a way markup cannot express",
                want.0, want.1, got.0, got.1
            )
        };
        let (plan, m, _, _) = self.trial(&w, r, 0, None)?;
        let mut mis = match self.compare(&plan, &m, usize::MAX) {
            Ok(()) => return Ok(Vec::new()),
            Err(CompareError::Structure { want, got }) => return Err(structure(want, got)),
            Err(CompareError::Geometry(v)) => v,
        };
        // A root whose height came from its container (stretched, percentage) needs it written down.
        let root_h_off = mis.first().is_some_and(|x| x.index == 0 && x.expected.len() == 1 && x.got.len() == 1 && (x.expected[0][3] - x.got[0][3]).abs() > self.opts.tol);
        if root_h_off {
            self.pin_h = true;
            let (plan2, m2, _, _) = self.trial(&w, r, 0, None)?;
            match self.compare(&plan2, &m2, usize::MAX) {
                Ok(()) => {
                    self.stats.height_pinned = true;
                    return Ok(Vec::new());
                }
                Err(CompareError::Geometry(v)) if v.len() < mis.len() => {
                    self.stats.height_pinned = true;
                    mis = v;
                }
                _ => self.pin_h = false,
            }
        }
        self.stats.baseline_mismatches = mis.len();
        let (ox, oy) = self.origin;
        for x in &mis {
            let abs = geom::translate(&x.got, -ox, -oy);
            for (k, &id) in x.nodes.iter().enumerate() {
                let rects = if k == 0 { abs.clone() } else { Vec::new() };
                match &mut self.work.nodes[id].kind {
                    Kind::Elem(e) => Rc::make_mut(e).rects = rects,
                    Kind::Text(t) => Rc::make_mut(t).rects = rects,
                }
            }
        }
        Ok(mis)
    }

    // ---------------------------------------------------------------- candidates

    fn sole_elem_child(w: &Work, id: usize) -> Option<usize> {
        match w.nodes[id].kids.as_slice() {
            [c] if w.is_elem(*c) => Some(*c),
            _ => None,
        }
    }

    fn can_unwrap(&self, w: &Work, id: usize) -> bool {
        if id == w.root {
            return false;
        }
        let Some(e) = w.elem(id) else { return false };
        if e.rigid || e.unchecked || e.has_pseudo() {
            return false;
        }
        if !self.traits(e, &e.style).neutral {
            return false;
        }
        // Numbered list items count among their siblings; their parent is that scope.
        if w.nodes[id].kids.iter().any(|&k| w.elem(k).is_some_and(|c| self.has_marker(&c.style))) {
            return false;
        }
        // Text that moves up must find the same inherited style there.
        let has_text = w.nodes[id].kids.iter().any(|&k| !w.is_elem(k));
        if has_text {
            let Some(pe) = w.nodes[id].parent.and_then(|pid| w.elem(pid)) else { return false };
            if (0..e.style.len()).any(|q| self.base.inherited[q] && e.style[q] != pe.style[q]) {
                return false;
            }
        }
        true
    }

    /// Per node: does it, or anything under it, put pixels on screen?
    fn paint_map(&self, w: &Work) -> Vec<bool> {
        let mut paint = vec![false; w.nodes.len()];
        // Children come after parents in id order only initially; walk explicitly instead.
        fn rec(eng: &Engine, w: &Work, id: usize, vis_parent: bool, paint: &mut Vec<bool>) -> bool {
            let res = match &w.nodes[id].kind {
                Kind::Text(t) => vis_parent && geom::has_area(&t.rects),
                Kind::Elem(e) => {
                    let st = &e.style;
                    let visible = eng.visible(st);
                    let mut any = false;
                    for &k in &w.nodes[id].kids {
                        any |= rec(eng, w, k, visible, paint);
                    }
                    if eng.t.at(st, eng.p.opacity) == "0" {
                        false
                    } else {
                        let own = if e.unchecked {
                            true
                        } else if !visible || !geom::has_area(&e.rects) {
                            // box-shadow and outline draw around an empty box too
                            visible && (!matches!(eng.t.at(st, eng.p.box_shadow), "none" | "") || !matches!(eng.t.at(st, eng.p.outline_style), "none" | ""))
                        } else {
                            e.tag != Tag::Auto || !eng.traits(e, st).paintless
                        };
                        own || any
                    }
                }
            };
            paint[id] = res;
            res
        }
        rec(self, w, w.root, true, &mut paint);
        paint
    }

    /// Per node: can any of it, or of anything under it, be seen? A box is unseen when every
    /// rect lies outside what its clipping ancestors (and the compared region) let through.
    /// Optimistic by design - a screenshot has the last word.
    fn seen_map(&self, w: &Work) -> Vec<bool> {
        fn hits(rects: &[geom::Rect], v: &[i32; 4]) -> bool {
            rects.iter().any(|r| r[2] > 0 && r[3] > 0 && r[0] < v[2] && r[0] + r[2] > v[0] && r[1] < v[3] && r[1] + r[3] > v[1])
        }
        fn rec(eng: &Engine, w: &Work, id: usize, view: [i32; 4], seen: &mut Vec<bool>) -> bool {
            let res = match &w.nodes[id].kind {
                Kind::Text(t) => hits(&t.rects, &view),
                Kind::Elem(e) => {
                    let st = &e.style;
                    let own = hits(&e.rects, &view);
                    // `clip` / `clip-path` are how "visually hidden" helpers are written.
                    let clipped_away = !matches!(eng.t.at(st, eng.t.prop("clip")), "auto" | "") || !matches!(eng.t.at(st, eng.t.prop("clip-path")), "none" | "");
                    let mut inner = view;
                    if e.rects.len() == 1 && !e.in_svg {
                        let r = e.rects[0];
                        let u = |v: Option<f64>| (v.unwrap_or(0.0) * UNIT).round() as i32;
                        if !matches!(eng.t.at(st, eng.p.overflow_x), "visible" | "") {
                            inner[0] = inner[0].max(r[0] + u(eng.bw(st, 3)));
                            inner[2] = inner[2].min(r[0] + r[2] - u(eng.bw(st, 1)));
                        }
                        if !matches!(eng.t.at(st, eng.p.overflow_y), "visible" | "") {
                            inner[1] = inner[1].max(r[1] + u(eng.bw(st, 0)));
                            inner[3] = inner[3].min(r[1] + r[3] - u(eng.bw(st, 2)));
                        }
                    }
                    if clipped_away {
                        inner = [0, 0, 0, 0];
                    }
                    let mut any = false;
                    for &k in &w.nodes[id].kids {
                        any |= rec(eng, w, k, inner, seen);
                    }
                    e.unchecked || (!clipped_away && own) || any
                }
            };
            seen[id] = res;
            res
        }
        let mut seen = vec![false; w.nodes.len()];
        rec(self, w, w.root, self.view, &mut seen);
        seen
    }

    fn generate(&mut self, tier: u8) -> Vec<Edit> {
        let w = std::mem::take(&mut self.work);
        let mut out = Vec::new();
        if tier == 5 {
            let mut stack = vec![w.root];
            while let Some(id) = stack.pop() {
                match &w.nodes[id].kind {
                    Kind::Text(t) => {
                        if t.sep && !self.tried.contains(&(id, Op::Fuse)) {
                            out.push(Edit { op: Op::Fuse, node: id });
                        }
                    }
                    Kind::Elem(_) => stack.extend(w.nodes[id].kids.iter().rev().copied()),
                }
            }
            self.work = w;
            return out;
        }
        let paint = if !matches!(tier, 1 | 2) { self.paint_map(&w) } else { Vec::new() };
        let seen = if tier == 4 { self.seen_map(&w) } else { Vec::new() };
        let mut stack = vec![w.root];
        while let Some(id) = stack.pop() {
            let Some(e) = w.elem(id) else { continue };
            if e.ns == 1 || e.unchecked {
                continue; // SVG content is left as it is
            }
            let mut descend = !e.opaque;
            match tier {
                0 => {
                    if id != w.root && !e.rigid && !paint[id] && !self.tried.contains(&(id, Op::Delete)) {
                        out.push(Edit { op: Op::Delete, node: id });
                        descend = false;
                    }
                }
                2 => {
                    if !self.tried.contains(&(id, Op::Unwrap)) && self.can_unwrap(&w, id) {
                        out.push(Edit { op: Op::Unwrap, node: id });
                    }
                }
                1 => {
                    if let Some(c) = Self::sole_elem_child(&w, id) {
                        if !self.tried.contains(&(id, Op::MergeDown)) && self.compose_down(&w, id, c).is_some() {
                            out.push(Edit { op: Op::MergeDown, node: id });
                        } else if !self.tried.contains(&(id, Op::MergeUp)) && self.compose_up(&w, id, c).is_some() {
                            out.push(Edit { op: Op::MergeUp, node: id });
                        }
                    }
                }
                4 => {
                    // Draws something, but none of it lands where it can be seen.
                    let removable = e.tag == Tag::Auto || e.opaque;
                    if id != w.root && removable && paint[id] && !seen[id] && !self.tried.contains(&(id, Op::Unseen)) {
                        out.push(Edit { op: Op::Unseen, node: id });
                        descend = false;
                    }
                }
                _ => {
                    if id != w.root && !e.rigid && !paint[id] && !w.nodes[id].kids.is_empty() && self.tried.contains(&(id, Op::Delete)) && !self.tried.contains(&(id, Op::Hollow)) {
                        out.push(Edit { op: Op::Hollow, node: id });
                        descend = false;
                    } else if !self.tried.contains(&(id, Op::Join)) && self.can_join(&w, id).is_some() {
                        out.push(Edit { op: Op::Join, node: id });
                    }
                }
            }
            if descend {
                stack.extend(w.nodes[id].kids.iter().rev().copied());
            }
        }
        self.work = w;
        out
    }

    fn can_join(&self, w: &Work, id: usize) -> Option<usize> {
        let parent = w.nodes[id].parent?;
        let kids = &w.nodes[parent].kids;
        let pos = kids.iter().position(|&k| k == id)?;
        let next = *kids.get(pos + 1)?;
        let (a, b) = (w.elem(id)?, w.elem(next)?);
        if a.rigid || b.rigid || a.unchecked || b.unchecked || a.has_pseudo() || b.has_pseudo() || a.tag != Tag::Auto || b.tag != Tag::Auto {
            return None;
        }
        if a.style[..] != b.style[..] || self.t.at(&a.style, self.p.display) != "inline" {
            return None;
        }
        for s in [1, 3] {
            if self.px(&a.style, self.p.margin[s])? != 0.0 || self.px(&a.style, self.p.padding[s])? != 0.0 || self.bw(&a.style, s)? != 0.0 {
                return None;
            }
        }
        Some(next)
    }

    // ---------------------------------------------------------------- merges

    /// Style of a parent that takes over the role of its only child `c`, a layout shell that
    /// filled it: the child's inner layout and spacing move onto the parent.
    fn compose_down(&mut self, w: &Work, pid: usize, cid: usize) -> Option<Style> {
        let (pe, ce) = (w.elem(pid)?, w.elem(cid)?);
        if pe.opaque || pe.has_pseudo() || pe.in_svg || pe.ns != 0 || pe.unchecked {
            return None;
        }
        if let Tag::Keep(n) = &pe.tag {
            if !matches!(n.as_str(), "td" | "th" | "caption") {
                return None;
            }
        }
        if ce.rigid || ce.has_pseudo() || ce.unchecked {
            return None;
        }
        let (ps, cs) = (pe.style.clone(), ce.style.clone());
        let ct = self.traits(ce, &cs);
        if !ct.paintless {
            return None;
        }
        let (po, pi) = parse_display(self.t.at(&ps, self.p.display));
        let (co, ci) = (ct.outer, ct.inner);
        let boxy = |i: Inner| matches!(i, Inner::Flow | Inner::FlowRoot | Inner::Flex | Inner::Grid);
        if !boxy(pi) || !boxy(ci) || self.has_marker(&ps) {
            return None;
        }
        // The child is either a block-level box filling the parent, or a plain inline box
        // (`<div><span>text</span></div>`), whose text then sits directly in the parent.
        let inline_child = co == Outer::Inline && ci == Inner::Flow && matches!(pi, Inner::Flow | Inner::FlowRoot);
        if matches!(pi, Inner::Flow | Inner::FlowRoot) && co != Outer::Block && !inline_child {
            return None;
        }
        if inline_child && self.t.at(&cs, self.p.vertical_align) != "baseline" {
            return None;
        }
        if self.t.at(&cs, self.p.position) != "static" || !matches!(self.t.at(&cs, self.p.float), "none" | "") {
            return None;
        }
        // Inherited values the parent's own box depends on must not change under it.
        for q in [self.p.visibility, self.p.direction, self.p.writing_mode].into_iter().flatten() {
            if ps[q] != cs[q] {
                return None;
            }
        }
        // The parent takes the child's text colour: fine unless it draws something of its own in it.
        if self.draws_in_current_color(&ps) || self.has_marker(&ps) {
            if let Some(q) = self.p.color {
                if ps[q] != cs[q] {
                    return None;
                }
            }
        }
        if !inline_child && (pe.rects.len() != 1 || ce.rects.len() != 1) {
            return None;
        }
        let (pr, cr) = (pe.rects.first().copied().unwrap_or([0; 4]), ce.rects.first().copied().unwrap_or([0; 4]));
        let tol = self.opts.tol + 1;
        let u = |v: f64| (v * UNIT).round() as i32;
        let mut pad = [0.0f64; 4];
        let mut p_pad = [0.0f64; 4];
        let mut through: [Option<f64>; 4] = [None; 4];
        for s in 0..4 {
            let pp = self.px(&ps, self.p.padding[s])?;
            let pb = self.bw(&ps, s)?;
            let cm = self.px(&cs, self.p.margin[s])?;
            let cp = self.px(&cs, self.p.padding[s])?;
            p_pad[s] = pp;
            if inline_child {
                // An inline box's own edges only exist along the line.
                pad[s] = if s == 1 || s == 3 { pp + cm + cp } else { pp };
                continue;
            }
            let inset = u(pb + pp);
            let (p_edge, c_edge, p_border, c_border) = match s {
                0 => (pr[1] + inset, cr[1] - u(cm), pr[1], cr[1]),
                1 => (pr[0] + pr[2] - inset, cr[0] + cr[2] + u(cm), pr[0] + pr[2], cr[0] + cr[2]),
                2 => (pr[1] + pr[3] - inset, cr[1] + cr[3] + u(cm), pr[1] + pr[3], cr[1] + cr[3]),
                _ => (pr[0] + inset, cr[0] - u(cm), pr[0], cr[0]),
            };
            if (p_edge - c_edge).abs() <= tol {
                pad[s] = pp + cm + cp;
            } else if (s == 0 || s == 2) && pi == Inner::Flow && pp == 0.0 && pb == 0.0 && cm != 0.0 && (p_border - c_border).abs() <= tol {
                // The child's margin collapsed through the parent's edge: it is the parent's margin now.
                through[s] = Some(cm);
                pad[s] = cp;
            } else {
                return None;
            }
        }
        let mut m = ps.to_vec();
        if !inline_child {
            let inner = if pi == Inner::FlowRoot && ci == Inner::Flow { Inner::FlowRoot } else { ci };
            let d = make_display(po, inner)?;
            if let Some(q) = self.p.display {
                m[q] = self.t.intern(d);
            }
        }
        for q in 0..m.len() {
            if (self.flags[q] & F_CONTAINER != 0 && !inline_child) || self.base.inherited[q] {
                m[q] = cs[q];
            }
        }
        for s in 0..4 {
            self.set_px(&mut m, self.p.padding[s], pad[s]);
            if let Some(cm) = through[s] {
                let pm = self.px(&ps, self.p.margin[s])?;
                self.set_px(&mut m, self.p.margin[s], collapse(pm, cm));
            }
        }
        // With content-box sizing the extra padding has to come out of the stated size.
        if self.t.at(&ps, self.p.box_sizing) != "border-box" {
            let axes = [
                (pad[1] - p_pad[1] + pad[3] - p_pad[3], [self.p.width, self.p.min_width, self.p.max_width]),
                (pad[0] - p_pad[0] + pad[2] - p_pad[2], [self.p.height, self.p.min_height, self.p.max_height]),
            ];
            for (added, sizes) in axes {
                if added.abs() < 1e-9 {
                    continue;
                }
                for q in sizes.into_iter().flatten() {
                    if ps[q] == self.base.div[q] {
                        continue;
                    }
                    let v = self.px(&ps, Some(q))?;
                    self.set_px(&mut m, Some(q), (v - added).max(0.0));
                }
            }
        }
        Some(Rc::from(m))
    }

    /// Style of an only child that takes the place of its parent `p`, a layout shell: the
    /// parent's position in *its* parent and its spacing move onto the child.
    fn compose_up(&mut self, w: &Work, pid: usize, cid: usize) -> Option<Style> {
        let (pe, ce) = (w.elem(pid)?, w.elem(cid)?);
        if pe.rigid || pe.has_pseudo() || pe.unchecked || pe.in_svg || ce.in_svg || ce.unchecked {
            return None;
        }
        // The root is the frame of the picture: its child may take over only if it is that
        // very frame already.
        if pid == w.root && !(pe.rects.len() == 1 && geom::eq(&pe.rects, &ce.rects, self.opts.tol)) {
            return None;
        }
        let (ps, cs) = (pe.style.clone(), ce.style.clone());
        let pt = self.traits(pe, &ps);
        if !pt.paintless {
            return None;
        }
        let (po, pi) = (pt.outer, pt.inner);
        let (co, ci) = parse_display(self.t.at(&cs, self.p.display));
        if !matches!(pi, Inner::Flow | Inner::FlowRoot | Inner::Flex | Inner::Grid) || !matches!(po, Outer::Block | Outer::Inline) {
            return None;
        }
        let bare_c = self.bare_of(ce).clone();
        let div = self.base.div.clone();
        // How the box is placed (positioning, float, vertical alignment) comes from whichever
        // of the two states it; if both do, they cannot be one box.
        let mut place: Vec<usize> = self.p.inset.iter().flatten().copied().collect();
        place.extend([self.p.position, self.p.float, self.p.clear, self.p.vertical_align].into_iter().flatten());
        let p_places = place.iter().any(|&q| ps[q] != div[q]);
        let c_places = place.iter().any(|&q| cs[q] != bare_c[q]);
        if p_places && c_places {
            return None;
        }
        let sizes = [self.p.width, self.p.min_width, self.p.max_width, self.p.height, self.p.min_height, self.p.max_height];
        let p_sized: Vec<bool> = sizes.iter().map(|q| q.is_some_and(|q| ps[q] != div[q])).collect();
        // ...and a minimum of zero does not size anything.
        let p_forces: Vec<bool> = sizes.iter().enumerate().map(|(i, q)| p_sized[i] && !((i == 1 || i == 4) && q.is_some_and(|q| self.t.get(ps[q]) == "0px"))).collect();
        let c_sized = sizes.iter().any(|q| q.is_some_and(|q| cs[q] != bare_c[q]));

        // Cheap plausibility: where the shell placed or sized its child by its own layout,
        // the child must already fill it.
        if pe.rects.len() == 1 && ce.rects.len() == 1 {
            let (pr, cr) = (pe.rects[0], ce.rects[0]);
            let tol = self.opts.tol + 1;
            let u = |v: f64| (v * UNIT).round() as i32;
            let need_x = matches!(pi, Inner::Flex | Inner::Grid) || p_forces[..3].iter().any(|&b| b);
            let need_y = matches!(pi, Inner::Flex | Inner::Grid) || p_forces[3..].iter().any(|&b| b);
            let padv: Vec<Option<f64>> = (0..4).map(|s| self.px(&ps, self.p.padding[s])).collect();
            if need_x {
                let (l, r) = (padv[3]?, padv[1]?);
                if (pr[0] + u(l) - cr[0]).abs() > tol || (pr[0] + pr[2] - u(r) - (cr[0] + cr[2])).abs() > tol {
                    return None;
                }
            }
            if need_y {
                let (t, b) = (padv[0]?, padv[2]?);
                if (pr[1] + u(t) - cr[1]).abs() > tol || (pr[1] + pr[3] - u(b) - (cr[1] + cr[3])).abs() > tol {
                    return None;
                }
            }
        }

        let mut m = cs.to_vec();
        let display: String = if po == Outer::Inline && pi == Inner::Flow {
            if co != Outer::Inline {
                return None;
            }
            self.t.at(&cs, self.p.display).to_string()
        } else {
            if !matches!(ci, Inner::Flow | Inner::FlowRoot | Inner::Flex | Inner::Grid | Inner::Table | Inner::ListItem) {
                return None;
            }
            let inner = if pi == Inner::FlowRoot && ci == Inner::Flow { Inner::FlowRoot } else { ci };
            make_display(po, inner)?.to_string()
        };
        if let Some(q) = self.p.display {
            m[q] = self.t.intern(&display);
        }
        for q in 0..m.len() {
            if self.flags[q] & F_OUTER != 0 && (p_places || !place.contains(&q)) {
                m[q] = ps[q];
            }
        }
        // The shell was a flex or grid item and its child did not fill it: without the shell
        // the child would stretch, so it is told to stay where it sat.
        if let Some(ge) = w.nodes[pid].parent.and_then(|g| w.elem(g)) {
            let gi = parse_display(self.t.at(&ge.style, self.p.display)).1;
            if matches!(gi, Inner::Flex | Inner::Grid) && pe.rects.len() == 1 && ce.rects.len() == 1 {
                let (pr, cr) = (pe.rects[0], ce.rects[0]);
                let tol = self.opts.tol + 1;
                let u = |v: Option<f64>| v.map(|v| (v * UNIT).round() as i32);
                let row = gi == Inner::Grid || self.t.at(&ge.style, self.p.flex_direction).starts_with("row");
                let (start, end, center) = if gi == Inner::Grid { ("start", "end", "center") } else { ("flex-start", "flex-end", "center") };
                // (near side, far side, property) for each axis that stretches
                let mut axes = vec![if row { (0usize, 2usize, self.p.align_self) } else { (3, 1, self.p.align_self) }];
                if gi == Inner::Grid {
                    axes.push((3, 1, self.p.justify_self));
                }
                for (a, b, prop) in axes {
                    let Some(prop) = prop else { continue };
                    let horizontal = a == 3;
                    let (p0, p1, c0, c1) = if horizontal { (pr[0], pr[0] + pr[2], cr[0], cr[0] + cr[2]) } else { (pr[1], pr[1] + pr[3], cr[1], cr[1] + cr[3]) };
                    let (q0, q1) = (p0 + u(self.px(&ps, self.p.padding[a]))?, p1 - u(self.px(&ps, self.p.padding[b]))?);
                    let (m0, m1) = (c0 - u(self.px(&cs, self.p.margin[a])).unwrap_or(0), c1 + u(self.px(&cs, self.p.margin[b])).unwrap_or(0));
                    if (q1 - q0) - (m1 - m0) <= tol {
                        continue; // fills the shell
                    }
                    let v = if (m0 - q0).abs() <= tol {
                        start
                    } else if (m1 - q1).abs() <= tol {
                        end
                    } else if ((m0 - q0) - (q1 - m1)).abs() <= 2 * tol {
                        center
                    } else {
                        return None;
                    };
                    m[prop] = self.t.intern(v);
                }
            }
        }
        for s in 0..4 {
            let Some(mq) = self.p.margin[s] else { continue };
            let pp = self.px(&ps, self.p.padding[s])?;
            let (pm, cm) = (self.px(&ps, Some(mq)), self.px(&cs, Some(mq)));
            m[mq] = match (pm, cm) {
                (Some(a), Some(b)) => {
                    let adjoining = (s == 0 || s == 2) && pp == 0.0 && po == Outer::Block && pi == Inner::Flow && co == Outer::Block;
                    let v = if adjoining { collapse(a, b) } else { a + pp + b };
                    self.t.intern(&fmt_px(v))
                }
                // `auto` or a percentage on one of them, and nothing else to add to it
                (None, Some(b)) if b == 0.0 && pp == 0.0 => ps[mq],
                (Some(a), None) if a == 0.0 && pp == 0.0 => cs[mq],
                _ => return None,
            };
        }
        // The shell's stated size describes its content box, which is the child's border box.
        let p_border_box = self.t.at(&ps, self.p.box_sizing) == "border-box";
        let c_border_box = self.t.at(&cs, self.p.box_sizing) == "border-box";
        for (i, q) in sizes.iter().enumerate() {
            let Some(q) = *q else { continue };
            if !p_sized[i] {
                continue;
            }
            if cs[q] != bare_c[q] {
                return None;
            }
            let (a, b) = if i < 3 { (3, 1) } else { (0, 2) };
            let p_pad = self.px(&ps, self.p.padding[a])? + self.px(&ps, self.p.padding[b])?;
            let mut val = ps[q];
            if p_border_box && p_pad != 0.0 {
                let x = self.px(&ps, Some(q))?;
                val = self.t.intern(&fmt_px((x - p_pad).max(0.0)));
            }
            let c_extra = (|| Some(self.px(&cs, self.p.padding[a])? + self.px(&cs, self.p.padding[b])? + self.bw(&cs, a)? + self.bw(&cs, b)?))();
            if c_extra == Some(0.0) || c_border_box {
                m[q] = val;
            } else {
                match (parse_px(self.t.get(val)), c_extra) {
                    (Some(x), Some(ex)) => m[q] = self.t.intern(&fmt_px((x - ex).max(0.0))),
                    _ => {
                        if c_sized {
                            return None;
                        }
                        if let Some(bs) = self.p.box_sizing {
                            m[bs] = self.t.intern("border-box");
                        }
                        m[q] = val;
                    }
                }
            }
        }
        Some(Rc::from(m))
    }

    // ---------------------------------------------------------------- applying

    /// Apply one edit to `w`. Returns false when it no longer applies.
    /// Text nodes that would become adjacent to text before them once `e` is applied.
    fn seams_of(w: &Work, e: Edit) -> Vec<usize> {
        let n = e.node;
        let Some(parent) = w.nodes[n].parent else { return Vec::new() };
        let sibs = &w.nodes[parent].kids;
        let Some(pos) = sibs.iter().position(|&k| k == n) else { return Vec::new() };
        let text = |id: Option<&usize>| id.copied().filter(|&id| !w.is_elem(id));
        let left = if pos > 0 { text(sibs.get(pos - 1)) } else { None };
        let right = text(sibs.get(pos + 1));
        let kids = &w.nodes[n].kids;
        let mut out = Vec::new();
        match e.op {
            Op::Unwrap => {
                if kids.is_empty() {
                    if left.is_some() {
                        out.extend(right);
                    }
                } else {
                    if left.is_some() {
                        out.extend(text(kids.first()));
                    }
                    if text(kids.last()).is_some() {
                        out.extend(right);
                    }
                }
            }
            Op::Delete | Op::Unseen => {
                if left.is_some() {
                    out.extend(right);
                }
            }
            Op::Join if text(kids.last()).is_some() => {
                if let Some(&m) = sibs.get(pos + 1) {
                    out.extend(text(w.nodes[m].kids.first()));
                }
            }
            _ => {}
        }
        out
    }

    fn apply(&mut self, w: &mut Work, e: Edit) -> bool {
        let n = e.node;
        if !w.alive(n) {
            return false;
        }
        if e.op == Op::Fuse {
            return match &mut w.nodes[n].kind {
                Kind::Text(t) if t.sep => {
                    Rc::make_mut(t).sep = false;
                    true
                }
                _ => false,
            };
        }
        if !w.is_elem(n) {
            return false;
        }
        let seams = Self::seams_of(w, e);
        let done = self.apply_structure(w, e);
        if done {
            for id in seams {
                if let Kind::Text(t) = &mut w.nodes[id].kind {
                    Rc::make_mut(t).sep = true;
                }
            }
        }
        done
    }

    fn apply_structure(&mut self, w: &mut Work, e: Edit) -> bool {
        let n = e.node;
        match e.op {
            Op::Unwrap => {
                if !self.can_unwrap(w, n) {
                    return false;
                }
                let kids = std::mem::take(&mut w.nodes[n].kids);
                w.splice(n, kids);
                w.kill(n);
                true
            }
            Op::Delete | Op::Unseen => {
                if n == w.root {
                    return false;
                }
                w.splice(n, Vec::new());
                w.kill_subtree(n);
                true
            }
            Op::MergeDown => {
                let Some(c) = Self::sole_elem_child(w, n) else { return false };
                let Some(st) = self.compose_down(w, n, c) else { return false };
                let kids = std::mem::take(&mut w.nodes[c].kids);
                for &k in &kids {
                    w.nodes[k].parent = Some(n);
                }
                w.nodes[n].kids = kids;
                w.kill(c);
                w.elem_mut(n).unwrap().style = st;
                true
            }
            Op::MergeUp => {
                let Some(c) = Self::sole_elem_child(w, n) else { return false };
                let Some(st) = self.compose_up(w, n, c) else { return false };
                w.elem_mut(c).unwrap().style = st;
                w.nodes[n].kids.clear();
                if n == w.root {
                    w.nodes[c].parent = None;
                    w.root = c;
                } else {
                    w.splice(n, vec![c]);
                }
                w.kill(n);
                true
            }
            Op::Hollow => {
                if n == w.root || w.nodes[n].kids.is_empty() {
                    return false;
                }
                let Some(el) = w.elem(n) else { return false };
                if el.rigid || el.unchecked {
                    return false;
                }
                let Some((bw, bh)) = el.bbox else { return false };
                let (o, i) = parse_display(self.t.at(&el.style, self.p.display));
                if matches!(i, Inner::Contents | Inner::Other) || (o == Outer::Inline && i == Inner::Flow) || o == Outer::Other {
                    return false;
                }
                let mut st = el.style.to_vec();
                self.set_px(&mut st, self.p.width, bw);
                self.set_px(&mut st, self.p.height, bh);
                if let Some(q) = self.p.box_sizing {
                    st[q] = self.t.intern("border-box");
                }
                for k in std::mem::take(&mut w.nodes[n].kids) {
                    w.kill_subtree(k);
                }
                let em = w.elem_mut(n).unwrap();
                em.style = Rc::from(st);
                em.pseudo = [None, None];
                true
            }
            Op::Fuse => false,
            Op::Join => {
                let Some(next) = self.can_join(w, n) else { return false };
                let kids = std::mem::take(&mut w.nodes[next].kids);
                for &k in &kids {
                    w.nodes[k].parent = Some(n);
                }
                w.nodes[n].kids.extend(kids);
                let extra = w.elem(next).unwrap().rects.clone();
                w.elem_mut(n).unwrap().rects.extend(extra);
                w.splice(next, Vec::new());
                w.kill(next);
                true
            }
        }
    }

    fn count(&mut self, op: Op) {
        match op {
            Op::Unwrap => self.stats.unwrapped += 1,
            Op::Delete => self.stats.deleted += 1,
            Op::MergeDown | Op::MergeUp => self.stats.merged += 1,
            Op::Hollow => self.stats.hollowed += 1,
            Op::Join => self.stats.joined += 1,
            Op::Unseen => self.stats.unseen += 1,
            Op::Fuse => self.stats.fused += 1,
        }
    }

    /// Try a batch of edits on a copy of the tree; commit it if the browser agrees.
    fn try_set(&mut self, cands: &[Edit], r: &mut dyn Renderer) -> Result<Outcome, String> {
        let mut w = self.work.clone();
        let mut applied = Vec::new();
        let mut skipped = Vec::new();
        // Edits that can change the picture without moving a box, each with the only place
        // where that could show.
        let mut seams: Vec<(Edit, [f64; 4])> = Vec::new();
        let mut local = true;
        for &e in cands {
            let own = if e.needs_pixels() { self.subtree_region(&w, e.node) } else { None };
            if self.apply(&mut w, e) {
                applied.push(e);
                if e.wants_pixels() {
                    match self.fuse_region(&w, e.node) {
                        Some(rg) => seams.push((e, rg)),
                        None => local = false,
                    }
                } else if e.needs_pixels() {
                    // (a subtree without boxes cannot have been seen at all)
                    if let Some(rg) = own {
                        seams.push((e, rg));
                    }
                }
            } else {
                skipped.push(e);
            }
        }
        if applied.is_empty() {
            if cands.len() == 1 {
                self.tried.insert((cands[0].node, cands[0].op));
                return Ok(Outcome::Committed);
            }
            return Ok(Outcome::Geometry);
        }
        let must = seams.iter().any(|(e, _)| e.needs_pixels());
        let wants = applied.iter().any(Edit::wants_pixels);
        let level = if must { 2 } else if self.opts.strict || wants { 1 } else { 0 };
        // Such an edit can only show in its own region; in strict mode the whole picture is judged.
        let region = if self.opts.strict || !local || seams.is_empty() {
            None
        } else {
            let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
            for (_, rg) in &seams {
                x0 = x0.min(rg[0]);
                y0 = y0.min(rg[1]);
                x1 = x1.max(rg[0] + rg[2]);
                y1 = y1.max(rg[1] + rg[3]);
            }
            Some([x0, y0, x1 - x0, y1 - y0])
        };
        let limit = if wants { usize::MAX } else { 1 };
        let (_, _, bad, looks) = self.trial_detail(&w, r, level, region, limit)?;
        let geometry = bad.is_none();
        if geometry && looks {
            self.work = w;
            for e in applied {
                self.count(e.op);
                self.trace.push(format!("{:?} #{}", e.op, e.node));
                self.history.push(e);
            }
            // What did not apply on top of the edits now committed will not apply later either.
            for e in skipped {
                self.tried.insert((e.node, e.op));
            }
            return Ok(Outcome::Committed);
        }
        if let Some(err) = bad {
            // Fused seams that moved something: the boxes that moved say in which blocks.
            if let (true, CompareError::Geometry(mis)) = (wants, &err) {
                let blocks: HashSet<usize> = mis.iter().filter_map(|m| m.nodes.first()).map(|&n| self.block_of(&w, n)).collect();
                let blamed: Vec<Edit> = applied.iter().copied().filter(|e| e.wants_pixels() && blocks.contains(&self.block_of(&w, e.node))).collect();
                if !blamed.is_empty() {
                    return Ok(Outcome::Blamed(blamed));
                }
            }
            return Ok(Outcome::Geometry);
        }
        let flags = r.differs(&seams.iter().map(|(_, rg)| *rg).collect::<Vec<_>>());
        Ok(Outcome::Blamed(seams.iter().zip(flags).filter(|(_, bad)| *bad).map(|((e, _), _)| *e).collect()))
    }

    /// Fuse text seams. Seams in different blocks cannot affect each other and the picture
    /// shows which block changed, so every block is bisected at once: each round is one
    /// screenshot, whatever the number of blocks.
    fn solve_fuse(&mut self, cands: &[Edit], r: &mut dyn Renderer) -> Result<(), String> {
        let mut by_block: Vec<(Option<usize>, Vec<Edit>)> = Vec::new();
        for &e in cands {
            let block = self.work.nodes[e.node].parent;
            match by_block.iter_mut().find(|(b, _)| *b == block) {
                Some((_, v)) => v.push(e),
                None => by_block.push((block, vec![e])),
            }
        }
        let mut chunks: Vec<Vec<Edit>> = by_block.into_iter().map(|(_, v)| v).collect();
        let mut later: Vec<Edit> = Vec::new();
        while !chunks.is_empty() && self.stats.trials < self.opts.max_trials && !r.out_of_time() {
            let batch: Vec<Edit> = chunks.iter().flatten().copied().collect();
            match self.try_set(&batch, r)? {
                Outcome::Committed => break,
                Outcome::Blamed(blamed) if !blamed.is_empty() => {
                    let is_blamed = |e: &Edit| blamed.iter().any(|b| b.node == e.node);
                    let mut next = Vec::new();
                    for ch in chunks {
                        if !ch.iter().any(is_blamed) {
                            next.push(ch); // its block looked right: it goes in with the next batch that passes
                        } else if ch.len() == 1 {
                            self.tried.insert((ch[0].node, ch[0].op));
                            self.trace.push(format!("reject {:?} #{}", ch[0].op, ch[0].node));
                        } else {
                            let mid = ch.len() / 2;
                            later.extend_from_slice(&ch[mid..]);
                            next.push(ch[..mid].to_vec());
                        }
                    }
                    chunks = next;
                }
                // Something moved, or the difference is not at a seam: plain bisection.
                _ => {
                    self.bisect(&batch, r)?;
                    break;
                }
            }
        }
        if later.is_empty() {
            Ok(())
        } else {
            self.solve_fuse(&later, r)
        }
    }

    fn solve(&mut self, cands: &[Edit], r: &mut dyn Renderer) -> Result<(), String> {
        if cands.is_empty() || self.stats.trials >= self.opts.max_trials || r.out_of_time() {
            return Ok(());
        }
        if cands.iter().all(|e| e.op == Op::Fuse) {
            return self.solve_fuse(cands, r);
        }
        match self.try_set(cands, r)? {
            Outcome::Committed => Ok(()),
            // The picture shows which removals were visible after all: those stay.
            Outcome::Blamed(blamed) if !blamed.is_empty() => {
                let is_blamed = |e: &Edit| blamed.iter().any(|b| b.node == e.node && b.op == e.op);
                for e in cands.iter().filter(|e| is_blamed(e)) {
                    self.tried.insert((e.node, e.op));
                    self.trace.push(format!("reject {:?} #{}", e.op, e.node));
                }
                let rest: Vec<Edit> = cands.iter().copied().filter(|e| !is_blamed(e)).collect();
                self.solve(&rest, r)
            }
            _ => self.bisect(cands, r),
        }
    }

    /// `cands` failed together: find the ones that pass.
    fn bisect(&mut self, cands: &[Edit], r: &mut dyn Renderer) -> Result<(), String> {
        if cands.len() == 1 {
            self.tried.insert((cands[0].node, cands[0].op));
            self.trace.push(format!("reject {:?} #{}", cands[0].op, cands[0].node));
            return Ok(());
        }
        let mid = cands.len() / 2;
        for half in [&cands[..mid], &cands[mid..]] {
            if self.stats.trials >= self.opts.max_trials || r.out_of_time() {
                // Out of budget: these are simply not tried (and not marked as rejected).
                break;
            }
            if !matches!(self.try_set(half, r)?, Outcome::Committed) {
                self.bisect(half, r)?;
            }
        }
        Ok(())
    }

    /// Remove every element whose removal the browser cannot tell apart.
    pub fn reduce(&mut self, r: &mut dyn Renderer) -> Result<(), String> {
        if !self.opts.structural {
            return Ok(());
        }
        loop {
            let mut progress = false;
            // 0 delete what draws nothing, 1 merge shells into their only child or parent,
            // 2 unwrap neutral boxes, 3 hollow / join, 4 remove what cannot be seen,
            // 5 fuse the text seams those left wherever the picture allows.
            let tiers: u8 = if r.has_pixels() { 6 } else { 4 };
            for tier in 0..tiers {
                loop {
                    let cands = self.generate(tier);
                    if cands.is_empty() || self.stats.trials >= self.opts.max_trials || r.out_of_time() {
                        break;
                    }
                    let before = self.edits_done();
                    self.solve(&cands, r)?;
                    if self.edits_done() > before {
                        progress = true;
                    }
                }
            }
            if !progress || self.stats.trials >= self.opts.max_trials || r.out_of_time() {
                break;
            }
        }
        if self.stats.trials >= self.opts.max_trials {
            self.warnings.push(format!("stopped after {} layout trials; more reduction may be possible with a higher trial limit", self.stats.trials));
        } else if r.out_of_time() {
            self.warnings.push("stopped at the time limit; more reduction may be possible with more time".into());
        }
        Ok(())
    }

    fn edits_done(&self) -> usize {
        let s = &self.stats;
        s.unwrapped + s.deleted + s.merged + s.hollowed + s.joined + s.unseen + s.fused
    }

    fn replay(&mut self, base: &Work, edits: &[Edit]) -> (Work, Vec<Edit>) {
        let mut w = base.clone();
        let mut applied = Vec::with_capacity(edits.len());
        for &e in edits {
            if self.apply(&mut w, e) {
                applied.push(e);
            }
        }
        (w, applied)
    }

    /// The reduced tree lays out identically but does not look identical. Find the edits
    /// responsible - by replaying prefixes of the accepted edits from `base` and asking the
    /// renderer for pixels - and take them back. Returns how many were withdrawn.
    pub fn repair(&mut self, base: &Work, r: &mut dyn Renderer) -> Result<usize, String> {
        let mut kept = self.history.clone();
        let mut removed = 0;
        // First, the cheap way: the picture shows *where* it differs. The accepted edit on the
        // smallest box around each such spot is the likely cause.
        let (ox, oy) = self.origin;
        for _ in 0..6 {
            let (w, _) = self.replay(base, &kept);
            let (_, _, bad, looks) = self.trial_detail(&w, r, 2, None, 1)?;
            if bad.is_some() || looks {
                break;
            }
            let points = r.diff_points(600);
            let mut blamed: HashSet<usize> = HashSet::new();
            for (x, y) in points {
                let (ux, uy) = ((x * UNIT) as i32 + ox, (y * UNIT) as i32 + oy);
                let pad = (2.0 * UNIT) as i32;
                let mut best: Option<(i64, usize)> = None;
                for (i, e) in kept.iter().enumerate() {
                    let rects = match &base.nodes[e.node].kind {
                        Kind::Elem(el) if !el.rects.is_empty() => &el.rects,
                        Kind::Text(t) => &t.rects,
                        _ => continue,
                    };
                    for rc in rects {
                        let inside = ux >= rc[0] - pad && ux <= rc[0] + rc[2] + pad && uy >= rc[1] - pad && uy <= rc[1] + rc[3] + pad;
                        let area = rc[2] as i64 * rc[3] as i64;
                        if inside && best.is_none_or(|(a, _)| area < a) {
                            best = Some((area, i));
                        }
                    }
                }
                if let Some((_, i)) = best {
                    blamed.insert(i);
                }
            }
            if blamed.is_empty() {
                break;
            }
            let mut idx: Vec<usize> = blamed.into_iter().collect();
            idx.sort_unstable_by(|a, b| b.cmp(a));
            for i in idx {
                let culprit = kept.remove(i);
                self.tried.insert((culprit.node, culprit.op));
                let tag = base.elem(culprit.node).map(|e| e.orig_tag.clone()).unwrap_or_else(|| "text".into());
                self.trace.push(format!("withdrawn {:?} #{} <{}>", culprit.op, culprit.node, tag));
                removed += 1;
            }
        }
        loop {
            let (w, applied) = self.replay(base, &kept);
            if self.check(&w, r, true)? {
                self.work = w;
                self.history = applied;
                let s = &mut self.stats;
                (s.unwrapped, s.deleted, s.merged, s.hollowed, s.joined, s.unseen, s.fused) = (0, 0, 0, 0, 0, 0, 0);
                for e in self.history.clone() {
                    self.count(e.op);
                }
                return Ok(removed);
            }
            if kept.is_empty() || removed >= 200 || r.out_of_time() {
                return Err("could not find a reduction that looks identical in the time allowed".into());
            }
            // The shortest failing prefix ends in an edit that must go.
            let (mut lo, mut hi) = (0usize, kept.len());
            while hi - lo > 1 {
                let mid = (lo + hi) / 2;
                let (w, _) = self.replay(base, &kept[..mid]);
                if self.check(&w, r, true)? {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            let culprit = kept.remove(hi - 1);
            self.tried.insert((culprit.node, culprit.op));
            let tag = base.elem(culprit.node).map(|e| e.orig_tag.clone()).unwrap_or_default();
            self.trace.push(format!("withdrawn {:?} #{} <{}>", culprit.op, culprit.node, tag));
            removed += 1;
        }
    }

    /// Go back to an earlier tree and forget what was tried, e.g. to redo the reduction
    /// under a stricter renderer.
    pub fn restore(&mut self, work: Work) {
        self.work = work;
        self.tried.clear();
        self.history.clear();
        let s = &mut self.stats;
        (s.unwrapped, s.deleted, s.merged, s.hollowed, s.joined, s.unseen, s.fused) = (0, 0, 0, 0, 0, 0, 0);
    }
}

#[derive(Deserialize)]
struct StylesOut {
    vals: Vec<String>,
    styles: Vec<Vec<u32>>,
    order: Vec<usize>,
}

#[derive(Debug, Clone)]
pub struct StyleMismatch {
    pub element: usize,
    pub tag: String,
    pub property: String,
    pub expected: String,
    pub got: String,
}

impl Engine {
    /// Compare the computed styles the browser reports for the emitted document with the
    /// targets, property by property.
    pub fn check_styles(&mut self, plan: &Plan, json: &str) -> Result<Vec<StyleMismatch>, String> {
        let out: StylesOut = serde_json::from_str(json).map_err(|e| format!("unreadable style read-back: {e}"))?;
        if out.order.len() != plan.n_elems {
            return Err(format!("style read-back covers {} elements, expected {}", out.order.len(), plan.n_elems));
        }
        let mut table: Vec<Vec<Val>> = Vec::with_capacity(out.styles.len());
        for ids in &out.styles {
            let mut st = self.t.adopt(&out.vals, ids).to_vec();
            self.cc_normalize(&mut st);
            table.push(st);
        }
        let color = self.p.color;
        let cur = self.v_current;
        let skip_ub = if self.info.bidi { None } else { self.p.unicode_bidi };
        // (style, width) pairs: a width is not compared where its line is not drawn, since
        // `medium` snaps differently at different zoom levels.
        let mut lines: Vec<(usize, usize)> = (0..4).filter_map(|i| Some((self.p.border_style[i]?, self.p.border_width[i]?))).collect();
        for (sp, wp) in [("outline-style", "outline-width"), ("column-rule-style", "column-rule-width"), ("row-rule-style", "row-rule-width")] {
            if let (Some(a), Some(b)) = (self.t.prop(sp), self.t.prop(wp)) {
                lines.push((a, b));
            }
        }
        let mut bad = Vec::new();
        let mut i = 0;
        for ev in &plan.evs {
            let Ev::Open(o) = ev else { continue };
            let got = &table[out.order[i]];
            let undrawn = |q: usize| lines.iter().any(|&(sq, wq)| wq == q && matches!(self.t.get(o.style[sq]), "none" | "hidden") && o.style[sq] == got[sq]);
            for q in 0..o.style.len() {
                // `currentcolor` and the colour it currently stands for are the same thing.
                let alias = |a: Val, b: Val| a == cur && color.is_some_and(|c| got[c] == b);
                // One value for every layer and that value once per layer are the same list.
                let repeated = |one: Val, many: Val| self.t.get(many).split(", ").all(|item| item == self.t.get(one));
                if o.style[q] != got[q] && Some(q) != skip_ub && !alias(o.style[q], got[q]) && !alias(got[q], o.style[q]) && !repeated(o.style[q], got[q]) && !undrawn(q) {
                    bad.push(StyleMismatch {
                        element: i,
                        tag: o.name.clone(),
                        property: self.t.props[q].clone(),
                        expected: self.t.get(o.style[q]).to_string(),
                        got: self.t.get(got[q]).to_string(),
                    });
                }
            }
            i += 1;
        }
        Ok(bad)
    }
}
