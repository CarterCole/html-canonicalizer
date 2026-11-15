//! Fewest declarations for inherited properties.
//!
//! The straightforward plan gives every element exactly its original computed style: an
//! element declares an inherited property whenever its value differs from its parent's.
//! That is more than the picture needs. `list-style-type` on a box that is not a list item,
//! or `font-style` on an empty bar, changes nothing there - such a box is free to carry
//! whatever value makes its descendants cheapest.
//!
//! For one property this is small parsimony on a tree: some nodes are fixed (the value is
//! visible there), the rest are free, and every parent-child edge whose two ends differ
//! costs one declaration. The dynamic programme below (Sankoff) finds the exact minimum.
//!
//! "Is the value visible here?" is the one judgement call, so it is deliberately
//! conservative (anything unknown counts as visible) and the final document is still
//! checked against the original by geometry, read-back and pixels.

use crate::emit::{Ev, Plan};
use crate::engine::Engine;
use crate::style::*;
use std::collections::HashMap;
use std::rc::Rc;

/// Where an inherited property can make a visible difference.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    /// Anywhere.
    Always,
    /// Only where text is set or lines are laid out.
    Text,
    /// Text, plus anything painted in `currentcolor`.
    Color,
    /// Only on list items.
    List,
    /// Only on table boxes.
    Table,
    /// Only on SVG content.
    Svg,
}

const TEXT: &[&str] = &[
    "font-family",
    "font-feature-settings",
    "font-kerning",
    "font-language-override",
    "font-optical-sizing",
    "font-palette",
    "font-size",
    "font-size-adjust",
    "font-stretch",
    "font-style",
    "font-synthesis-small-caps",
    "font-synthesis-style",
    "font-synthesis-weight",
    "font-variant-alternates",
    "font-variant-caps",
    "font-variant-east-asian",
    "font-variant-emoji",
    "font-variant-ligatures",
    "font-variant-numeric",
    "font-variant-position",
    "font-variation-settings",
    "font-weight",
    "letter-spacing",
    "line-height",
    "text-align",
    "text-align-last",
    "text-indent",
    "text-justify",
    "text-rendering",
    "text-shadow",
    "text-transform",
    "text-underline-offset",
    "text-underline-position",
    "text-wrap-mode",
    "text-wrap-style",
    "white-space-collapse",
    "word-break",
    "word-spacing",
    "overflow-wrap",
    "hyphens",
    "hyphenate-character",
    "hyphenate-limit-chars",
    "line-break",
    "tab-size",
    "text-autospace",
    "text-spacing-trim",
    "text-decoration-skip-ink",
    "text-emphasis-color",
    "text-emphasis-position",
    "text-emphasis-style",
    "-webkit-text-fill-color",
    "-webkit-text-stroke-color",
    "-webkit-text-stroke-width",
    "-webkit-font-smoothing",
    "-webkit-locale",
    "-webkit-text-security",
    "text-size-adjust",
    "quotes",
];

const LIST: &[&str] = &["list-style-image", "list-style-position", "list-style-type"];

const TABLE: &[&str] = &["border-collapse", "-webkit-border-horizontal-spacing", "-webkit-border-vertical-spacing", "caption-side", "empty-cells"];

const SVG: &[&str] = &[
    "fill",
    "fill-opacity",
    "fill-rule",
    "stroke",
    "stroke-dasharray",
    "stroke-dashoffset",
    "stroke-linecap",
    "stroke-linejoin",
    "stroke-miterlimit",
    "stroke-opacity",
    "stroke-width",
    "marker-end",
    "marker-mid",
    "marker-start",
    "paint-order",
    "shape-rendering",
    "text-anchor",
    "dominant-baseline",
    "color-interpolation",
    "color-interpolation-filters",
    "clip-rule",
    "color-rendering",
];

pub fn classify(name: &str) -> Class {
    if name == "color" {
        Class::Color
    } else if TEXT.contains(&name) {
        Class::Text
    } else if LIST.contains(&name) {
        Class::List
    } else if TABLE.contains(&name) {
        Class::Table
    } else if SVG.contains(&name) {
        Class::Svg
    } else {
        Class::Always
    }
}

struct Node {
    ev: usize,
    parent: Option<usize>,
    kids: Vec<usize>,
    text: bool,
    inline_kid: bool,
    /// Which classes of property are visible on this element itself.
    sees: [bool; 6],
}

fn ix(c: Class) -> usize {
    match c {
        Class::Always => 0,
        Class::Text => 1,
        Class::Color => 2,
        Class::List => 3,
        Class::Table => 4,
        Class::Svg => 5,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// Takes its parent's value; the value shows here.
    Fixed,
    /// Takes its parent's value; nothing here depends on it.
    Free,
    /// Has a default of its own (a browser rule or an attribute sets it).
    Own,
}

impl Engine {
    /// Rewrite the plan's declarations of inherited properties to the fewest that give
    /// every element the value wherever that value shows. Updates each element's intended
    /// style accordingly, so read-back verification still applies in full.
    pub fn hoist_inherited(&self, plan: &mut Plan) {
        // --- the element tree of the plan
        let mut nodes: Vec<Node> = Vec::with_capacity(plan.n_elems);
        let mut stack: Vec<usize> = Vec::new();
        for (i, ev) in plan.evs.iter().enumerate() {
            match ev {
                Ev::Open(_) => {
                    let id = nodes.len();
                    let parent = stack.last().copied();
                    if let Some(p) = parent {
                        nodes[p].kids.push(id);
                    }
                    nodes.push(Node { ev: i, parent, kids: Vec::new(), text: false, inline_kid: false, sees: [true; 6] });
                    stack.push(id);
                }
                Ev::Text { .. } => {
                    if let Some(&p) = stack.last() {
                        nodes[p].text = true;
                    }
                }
                Ev::Close => {
                    stack.pop();
                }
            }
        }
        let n = nodes.len();
        if n == 0 {
            return;
        }
        let open = |plan: &Plan, node: &Node| -> usize {
            let _ = plan;
            node.ev
        };
        for id in 0..n {
            let Ev::Open(o) = &plan.evs[open(plan, &nodes[id])] else { continue };
            if parse_display(self.t.at(&o.style, self.p.display)).0 == Outer::Inline {
                if let Some(p) = nodes[id].parent {
                    nodes[p].inline_kid = true;
                }
            }
        }
        // --- what shows where
        for id in 0..n {
            let Ev::Open(o) = &plan.evs[nodes[id].ev] else { continue };
            let st = &o.style;
            let display = self.t.at(st, self.p.display);
            let (outer, inner) = parse_display(display);
            let svg = o.ns == 1;
            let has_pseudo = o.pseudo.iter().any(Option::is_some);
            let block_container = matches!(inner, Inner::Flow | Inner::FlowRoot | Inner::ListItem) || display.starts_with("table-c");
            let lines = block_container && nodes[id].inline_kid;
            let inline_box = !svg && outer == Outer::Inline && inner == Inner::Flow;
            let opaque = o.inner.is_some() || o.void || (o.ns == 0 && crate::props::is_opaque(&o.name));
            let text = nodes[id].text || lines || inline_box || opaque || has_pseudo || self.has_marker(st);
            let paints_current = self.draws_in_current_color(st);
            let mut sees = [true; 6];
            sees[ix(Class::Text)] = text;
            sees[ix(Class::Color)] = text || paints_current || svg;
            sees[ix(Class::List)] = display == "list-item" || has_pseudo;
            sees[ix(Class::Table)] = display.contains("table");
            sees[ix(Class::Svg)] = svg;
            nodes[id].sees = sees;
        }

        // --- which properties are worth the trouble: inherited ones something declares
        let np = self.t.nprops();
        let mut wanted = vec![false; np];
        for ev in &plan.evs {
            if let Ev::Open(o) = ev {
                for &(q, _) in &o.decls {
                    if self.base.inherited[q] {
                        wanted[q] = true;
                    }
                }
            }
        }
        let color_q = self.p.color;
        let text_align = self.t.prop("text-align");
        // (property, effective value per node, declared per node)
        let mut results: Vec<(usize, Vec<Val>, Vec<bool>)> = Vec::new();
        for q in (0..np).filter(|&q| wanted[q]) {
            let class = self.pclass[q];
            let b0 = self.base.div[q];
            let mut dom: Vec<Val> = vec![b0];
            let mut dix: HashMap<Val, usize> = HashMap::from([(b0, 0)]);
            let mut ti = vec![0usize; n];
            let mut kind = vec![Kind::Fixed; n];
            let mut own_decl = vec![false; n];
            for id in 0..n {
                let Ev::Open(o) = &plan.evs[nodes[id].ev] else { continue };
                let t = o.style[q];
                ti[id] = *dix.entry(t).or_insert_with(|| {
                    dom.push(t);
                    dom.len() - 1
                });
                kind[id] = if o.follow[q] != 1 {
                    let bare = o.bare[q];
                    let same = t == bare || (bare == self.v_current && color_q.is_some_and(|c| t == o.style[c]));
                    own_decl[id] = !same;
                    Kind::Own
                } else if nodes[id].sees[ix(class)] || (Some(q) == text_align && self.t.get(t).starts_with("-webkit-")) {
                    // (`text-align: -webkit-center` also centres block children)
                    Kind::Fixed
                } else {
                    Kind::Free
                };
            }
            let d = dom.len();
            if d == 1 {
                continue;
            }
            // inh[id][v]: cheapest subtree of `id` given it inherits v. sum[id][u]: its
            // children's total if `id` ends up with u.
            let mut inh = vec![0u32; n * d];
            let mut sum = vec![0u32; n * d];
            for id in (0..n).rev() {
                for &c in &nodes[id].kids {
                    for u in 0..d {
                        sum[id * d + u] += inh[c * d + u];
                    }
                }
                let t = ti[id];
                match kind[id] {
                    Kind::Own => {
                        let k = own_decl[id] as u32 + sum[id * d + t];
                        inh[id * d..(id + 1) * d].fill(k);
                    }
                    Kind::Fixed => {
                        let base = sum[id * d + t];
                        for v in 0..d {
                            inh[id * d + v] = base + (v != t) as u32;
                        }
                    }
                    Kind::Free => {
                        let m = *sum[id * d..(id + 1) * d].iter().min().unwrap();
                        for v in 0..d {
                            inh[id * d + v] = sum[id * d + v].min(1 + m);
                        }
                    }
                }
            }
            let mut eff = vec![0usize; n];
            let mut decl = vec![false; n];
            for id in 0..n {
                let v = nodes[id].parent.map_or(0, |p| eff[p]);
                let t = ti[id];
                match kind[id] {
                    Kind::Own => {
                        eff[id] = t;
                        decl[id] = own_decl[id];
                    }
                    Kind::Fixed => {
                        eff[id] = t;
                        decl[id] = v != t;
                    }
                    Kind::Free => {
                        let row = &sum[id * d..(id + 1) * d];
                        let m = *row.iter().min().unwrap();
                        // If the inherited value is already the cheapest, keep it. Otherwise a
                        // declaration is due here or below at the same price. Text properties go
                        // on the outermost element they hold for (the font on the root, where
                        // one looks for it); the others as close as possible to where they act
                        // (the marker style on the list, not on the page).
                        let outermost = matches!(class, Class::Text | Class::Color | Class::Always);
                        if row[v] == m || (!outermost && row[v] <= 1 + m) {
                            eff[id] = v;
                        } else {
                            // Any cheapest value will do; keep the element's own when it is one.
                            eff[id] = if row[t] == m { t } else { row.iter().position(|&x| x == m).unwrap() };
                            decl[id] = true;
                        }
                    }
                }
            }
            results.push((q, eff.iter().map(|&i| dom[i]).collect(), decl));
        }
        if results.is_empty() {
            return;
        }

        // --- write the result back
        let handled: Vec<bool> = {
            let mut h = vec![false; np];
            for (q, _, _) in &results {
                h[*q] = true;
            }
            h
        };
        for id in 0..n {
            let ev = nodes[id].ev;
            let Ev::Open(o) = &mut plan.evs[ev] else { continue };
            let mut st = o.style.to_vec();
            let mut decls: Vec<(usize, Val)> = o.decls.iter().copied().filter(|&(q, _)| !handled[q]).collect();
            for (q, eff, decl) in &results {
                st[*q] = eff[id];
                if decl[id] {
                    decls.push((*q, eff[id]));
                }
            }
            decls.sort_unstable_by_key(|&(q, _)| q);
            o.decls = decls;
            o.style = Rc::from(st);
            for k in 0..2 {
                if let Some((_, pd, ps)) = &mut o.pseudo[k] {
                    *pd = self.pseudo_decls(ps, &o.style);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes() {
        assert_eq!(classify("color"), Class::Color);
        assert_eq!(classify("font-size"), Class::Text);
        assert_eq!(classify("list-style-type"), Class::List);
        assert_eq!(classify("border-collapse"), Class::Table);
        assert_eq!(classify("stroke-width"), Class::Svg);
        // unknown or layout-affecting: visible everywhere
        assert_eq!(classify("direction"), Class::Always);
        assert_eq!(classify("visibility"), Class::Always);
        assert_eq!(classify("some-future-property"), Class::Always);
    }
}
