//! Interned computed-style tables.
//!
//! A computed style is a dense vector of value ids, one per property, in the order the
//! browser enumerated them. Two styles are equal when their vectors are equal, so every
//! comparison the reducer makes is an integer compare.

use std::collections::HashMap;
use std::rc::Rc;

pub type Val = u32;
pub type Style = Rc<[Val]>;

#[derive(Debug, Default)]
pub struct Table {
    pub props: Vec<String>,
    prop_ix: HashMap<String, usize>,
    values: Vec<String>,
    val_ix: HashMap<String, Val>,
}

impl Table {
    pub fn new(props: Vec<String>, values: Vec<String>) -> Self {
        let prop_ix = props.iter().enumerate().map(|(i, p)| (p.clone(), i)).collect();
        let mut val_ix = HashMap::with_capacity(values.len());
        for (i, v) in values.iter().enumerate() {
            val_ix.entry(v.clone()).or_insert(i as Val);
        }
        Table { props, prop_ix, values, val_ix }
    }

    pub fn nprops(&self) -> usize {
        self.props.len()
    }

    pub fn prop(&self, name: &str) -> Option<usize> {
        self.prop_ix.get(name).copied()
    }

    pub fn intern(&mut self, s: &str) -> Val {
        if let Some(&v) = self.val_ix.get(s) {
            return v;
        }
        let v = self.values.len() as Val;
        self.values.push(s.to_string());
        self.val_ix.insert(s.to_string(), v);
        v
    }

    pub fn get(&self, v: Val) -> &str {
        &self.values[v as usize]
    }

    /// Value of property `p` in `st`, or "" when the browser does not have that property.
    pub fn at<'a>(&'a self, st: &[Val], p: Option<usize>) -> &'a str {
        match p {
            Some(p) => self.get(st[p]),
            None => "",
        }
    }

    /// Re-intern a style that came from another page (its own value list) into this table.
    pub fn adopt(&mut self, vals: &[String], ids: &[Val]) -> Style {
        ids.iter().map(|&i| self.intern(&vals[i as usize])).collect()
    }
}

/// Parse a plain pixel length such as `12.5px` (or a bare `0`).
pub fn parse_px(s: &str) -> Option<f64> {
    if s == "0" {
        return Some(0.0);
    }
    let n = s.strip_suffix("px")?;
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit() || b == b'.' || b == b'-' || b == b'e' || b == b'+') {
        return None;
    }
    n.parse().ok()
}

/// Format a pixel length the way CSS serialises it: no trailing zeros.
pub fn fmt_px(v: f64) -> String {
    format!("{}px", fmt_num(v))
}

pub fn fmt_num(v: f64) -> String {
    let v = if v.abs() < 1e-9 { 0.0 } else { v };
    let mut s = format!("{:.5}", v);
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" {
        s = "0".into();
    }
    s
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outer {
    Block,
    Inline,
    Cell,
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Inner {
    Flow,
    FlowRoot,
    Flex,
    Grid,
    Table,
    ListItem,
    Contents,
    Other,
}

pub fn parse_display(s: &str) -> (Outer, Inner) {
    use Inner::*;
    use Outer::*;
    match s {
        "block" => (Block, Flow),
        "inline" => (Inline, Flow),
        "inline-block" => (Inline, FlowRoot),
        "flow-root" => (Block, FlowRoot),
        "flex" => (Block, Flex),
        "inline-flex" => (Inline, Flex),
        "grid" => (Block, Grid),
        "inline-grid" => (Inline, Grid),
        "table" => (Block, Table),
        "inline-table" => (Inline, Table),
        "list-item" => (Block, ListItem),
        "table-cell" => (Cell, FlowRoot),
        "contents" => (Outer::Other, Contents),
        _ => (Outer::Other, Inner::Other),
    }
}

pub fn make_display(o: Outer, i: Inner) -> Option<&'static str> {
    use Inner::*;
    use Outer::*;
    Some(match (o, i) {
        (Block, Flow) => "block",
        (Block, FlowRoot) => "flow-root",
        (Block, Flex) => "flex",
        (Block, Grid) => "grid",
        (Block, Table) => "table",
        (Block, ListItem) => "list-item",
        (Inline, Flow) => "inline",
        (Inline, FlowRoot) => "inline-block",
        (Inline, Flex) => "inline-flex",
        (Inline, Grid) => "inline-grid",
        (Inline, Table) => "inline-table",
        (Cell, Flow) | (Cell, FlowRoot) => "table-cell",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn px_round_trip() {
        assert_eq!(parse_px("12.5px"), Some(12.5));
        assert_eq!(parse_px("0px"), Some(0.0));
        assert_eq!(parse_px("auto"), None);
        assert_eq!(parse_px("50%"), None);
        assert_eq!(parse_px("calc(1px + 2px)"), None);
        assert_eq!(fmt_px(12.5), "12.5px");
        assert_eq!(fmt_px(0.0), "0px");
        assert_eq!(fmt_px(174.65625), "174.65625px");
        assert_eq!(fmt_px(-0.0), "0px");
    }

    #[test]
    fn display_round_trip() {
        for d in ["block", "inline", "inline-block", "flow-root", "flex", "inline-flex", "grid", "inline-grid", "table", "list-item"] {
            let (o, i) = parse_display(d);
            assert_eq!(make_display(o, i), Some(d));
        }
        assert_eq!(make_display(Outer::Inline, Inner::Contents), None);
    }

    #[test]
    fn interning_is_stable() {
        let mut t = Table::new(vec!["color".into()], vec!["red".into(), "blue".into()]);
        assert_eq!(t.intern("blue"), 1);
        let n = t.intern("green");
        assert_eq!(t.get(n), "green");
        assert_eq!(t.intern("green"), n);
    }
}
