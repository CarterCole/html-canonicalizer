//! The snapshot as the page agent reports it, and the working tree the reducer edits.

use crate::geom::Rects;
use crate::style::Style;
use serde::{Deserialize, Serialize};
use std::rc::Rc;

#[derive(Deserialize, Debug, Default)]
#[serde(default)]
pub struct RawSnapshot {
    pub error: Option<String>,
    pub props: Vec<String>,
    pub values: Vec<String>,
    pub styles: Vec<Vec<u32>>,
    pub nodes: Vec<RawNode>,
    pub info: Info,
}

/// One node, in document order. Elements have `g` (tag); text nodes have `x` (data).
#[derive(Deserialize, Debug, Default)]
#[serde(default)]
pub struct RawNode {
    /// Parent index, -1 for the root, -2 for an out-of-tree `<use>` target.
    pub p: i64,
    pub g: Option<String>,
    /// Namespace: 0 HTML, 1 SVG, 2 MathML.
    pub n: u8,
    /// Bit 0: `display: contents`.
    pub f: u32,
    pub s: Option<u32>,
    /// Client rects, four ints each, in 1/64 px.
    pub r: Vec<i32>,
    /// Untransformed border-box size in px.
    pub b: Option<(f64, f64)>,
    pub a: Vec<(String, String)>,
    /// `::before` / `::after`.
    pub q: Option<Vec<Option<RawPseudo>>>,
    /// Resolved style of the element itself, present when it has a pseudo-element.
    pub e: Option<u32>,
    /// Raw inner HTML of an opaque element (`<select>` options, `<textarea>` value).
    pub i: Option<String>,
    /// Resolved style of the `::placeholder`, when one is showing.
    pub h: Option<u32>,
    pub x: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct RawPseudo {
    pub c: String,
    pub s: u32,
    /// Property names the page's rules declare on it, when they could be read.
    #[serde(default)]
    pub d: Option<Vec<String>>,
}

#[derive(Deserialize, Serialize, Debug, Default, Clone)]
#[serde(default, rename_all = "camelCase")]
pub struct Backdrop {
    pub color: [u8; 3],
    /// An ancestor paints an image or gradient behind the element; only its colour is kept.
    pub complex: bool,
}

#[derive(Deserialize, Serialize, Debug, Default, Clone)]
#[serde(default, rename_all = "camelCase")]
pub struct Info {
    pub url: String,
    pub title: String,
    pub compat: String,
    /// The document's doctype when it carries public or system ids (limited-quirks
    /// documents), else empty.
    pub doctype: String,
    pub viewport: (f64, f64),
    pub dpr: f64,
    pub scroll: (f64, f64),
    /// Bounding rect of the root in viewport px.
    pub rect: [f64; 4],
    pub root_tag: String,
    /// Elements under the root in the live DOM, rendered or not.
    pub dom_elements: usize,
    /// Length of the root's markup in the live DOM.
    pub source_bytes: usize,
    pub zoom: f64,
    pub transformed: bool,
    pub overflow_propagated: bool,
    pub parent_width: f64,
    /// The page was rendered with `prefers-color-scheme: dark`.
    pub dark: bool,
    /// Scrollable extent of the document, in px.
    pub doc_size: (f64, f64),
    pub backdrop: Backdrop,
    pub extras: Vec<usize>,
    pub scrolled: usize,
    pub bidi: bool,
    pub shadow: usize,
    pub canvas_tainted: usize,
    /// Pseudo-elements whose rules the page agent could not read.
    pub pseudo_unknown: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tag {
    /// No behaviour of its own: emitted as `<div>` or `<span>`, whichever needs less CSS.
    Auto,
    /// Emitted as itself.
    Keep(String),
}

#[derive(Clone, Debug)]
pub struct Pseudo {
    pub content: String,
    pub style: Style,
}

#[derive(Clone, Debug)]
pub struct Elem {
    pub tag: Tag,
    pub ns: u8,
    pub orig_tag: String,
    pub attrs: Vec<(String, String)>,
    /// Probe key of a kept tag (its default style depends on tag and attributes).
    pub key: Option<Rc<str>>,
    /// Which kind of element this is for inheritance purposes (0: a plain div/span).
    pub cls: usize,
    /// Target computed style.
    pub style: Style,
    /// Where the original was drawn, in viewport layout units.
    pub rects: Rects,
    pub bbox: Option<(f64, f64)>,
    pub pseudo: [Option<Pseudo>; 2],
    /// Further pseudo-element rules, ready made: (name, declarations). `::placeholder`.
    pub extra: Vec<(&'static str, Vec<(usize, crate::style::Val)>)>,
    pub inner: Option<String>,
    pub opaque: bool,
    /// Structural edits leave it alone.
    pub rigid: bool,
    /// Lives inside SVG content.
    pub in_svg: bool,
    /// Geometry is not checked (invisible helper content).
    pub unchecked: bool,
}

impl Elem {
    pub fn has_pseudo(&self) -> bool {
        self.pseudo[0].is_some() || self.pseudo[1].is_some()
    }
}

#[derive(Clone, Debug)]
pub struct Text {
    pub data: String,
    pub rects: Rects,
    /// Keep this text a node of its own, apart from text before it (see `Edit::seam`).
    pub sep: bool,
}

#[derive(Clone, Debug)]
pub enum Kind {
    Elem(Rc<Elem>),
    Text(Rc<Text>),
}

#[derive(Clone, Debug)]
pub struct WNode {
    pub parent: Option<usize>,
    pub kids: Vec<usize>,
    pub alive: bool,
    pub kind: Kind,
}

/// The tree being reduced. Node ids are stable; removed nodes are tombstoned.
#[derive(Clone, Debug, Default)]
pub struct Work {
    pub nodes: Vec<WNode>,
    pub root: usize,
}

impl Work {
    pub fn elem(&self, id: usize) -> Option<&Elem> {
        match &self.nodes[id].kind {
            Kind::Elem(e) => Some(e),
            Kind::Text(_) => None,
        }
    }

    pub fn elem_mut(&mut self, id: usize) -> Option<&mut Elem> {
        match &mut self.nodes[id].kind {
            Kind::Elem(e) => Some(Rc::make_mut(e)),
            Kind::Text(_) => None,
        }
    }

    pub fn is_elem(&self, id: usize) -> bool {
        matches!(self.nodes[id].kind, Kind::Elem(_))
    }

    pub fn alive(&self, id: usize) -> bool {
        self.nodes[id].alive
    }

    /// Replace `id` in its parent's child list with `with` (which may be empty).
    pub fn splice(&mut self, id: usize, with: Vec<usize>) {
        let Some(parent) = self.nodes[id].parent else { return };
        for &k in &with {
            self.nodes[k].parent = Some(parent);
        }
        let kids = &mut self.nodes[parent].kids;
        if let Some(pos) = kids.iter().position(|&k| k == id) {
            kids.splice(pos..=pos, with);
        }
    }

    pub fn kill(&mut self, id: usize) {
        self.nodes[id].alive = false;
        self.nodes[id].kids.clear();
    }

    pub fn kill_subtree(&mut self, id: usize) {
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            stack.extend(self.nodes[n].kids.iter().copied());
            self.kill(n);
        }
    }

    /// Element ids in document order.
    pub fn elems_preorder(&self) -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack = vec![self.root];
        while let Some(n) = stack.pop() {
            if self.is_elem(n) {
                out.push(n);
            }
            stack.extend(self.nodes[n].kids.iter().rev().copied());
        }
        out
    }

    pub fn count_elems(&self) -> usize {
        self.elems_preorder().len()
    }
}
