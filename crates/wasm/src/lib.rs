//! canon-core for the browser.
//!
//! The engine is driven step by step from JavaScript (`web/canon.js`): the JS side owns the
//! DOM (capture on the host page, trial renders in a hidden iframe), this side owns every
//! decision. Data crosses as JSON strings, the same shapes the CLI exchanges with Chrome.
//!
//! A JS `render(docJson) -> measureJson` function stands in for the CLI's clean tab. It is
//! called synchronously from inside `calibrate` / `reduce`, so it must lay the document out
//! and measure it without awaiting anything (resources are preloaded beforehand).

use canon_core::{Doc, Engine, Measure, Options, Renderer, StyleMode};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

/// Engine options as JS passes them (all optional).
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Opts {
    simplify: Option<bool>,
    reduce: Option<bool>,
    max_trials: Option<usize>,
    fluid: Option<bool>,
    hoist: Option<bool>,
    /// Geometry tolerance in 1/64 px.
    tolerance: Option<i32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DocOut<'a> {
    /// Tree JSON, embedded as a value.
    tree: &'a serde_json::value::RawValue,
    css: &'a str,
    x: f64,
    y: f64,
    w: Option<f64>,
    bg: &'a str,
}

struct JsRenderer<'a> {
    render: &'a js_sys::Function,
    /// `Date.now()` value after which the search stops (0: no limit).
    deadline: f64,
    error: Option<String>,
}

impl Renderer for JsRenderer<'_> {
    fn render(&mut self, doc: &Doc) -> Result<Measure, String> {
        let tree_json = doc.tree.ok_or("the engine produced markup where a tree was expected")?;
        let tree: &serde_json::value::RawValue = serde_json::from_str(tree_json).map_err(|e| e.to_string())?;
        let arg = serde_json::to_string(&DocOut { tree, css: doc.css, x: doc.x, y: doc.y, w: doc.w, bg: doc.bg }).map_err(|e| e.to_string())?;
        let out = self.render.call1(&JsValue::NULL, &JsValue::from_str(&arg)).map_err(|e| {
            let msg = js_error(&e);
            self.error = Some(msg.clone());
            msg
        })?;
        let s = out.as_string().ok_or("render() must return the measurement as a JSON string")?;
        Measure::parse(&s)
    }

    fn out_of_time(&self) -> bool {
        self.deadline > 0.0 && js_sys::Date::now() >= self.deadline
    }
}

fn js_error(e: &JsValue) -> String {
    if let Some(err) = e.dyn_ref::<js_sys::Error>() {
        return String::from(err.message());
    }
    e.as_string().unwrap_or_else(|| "render() threw".into())
}

fn needed_faces(sheets: &str, loaded: &str) -> Result<Vec<canon_core::fonts::Face>, JsError> {
    let sheets: Vec<(String, String)> = serde_json::from_str(sheets).map_err(err)?;
    let loaded: Vec<canon_core::fonts::Loaded> = serde_json::from_str(loaded).map_err(err)?;
    let mut faces = Vec::new();
    for (text, base) in &sheets {
        faces.extend(canon_core::fonts::extract(text, base));
    }
    Ok(canon_core::fonts::needed(&faces, &loaded))
}

fn err(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

#[wasm_bindgen]
pub struct Canon {
    eng: Engine,
    last_plan: Option<canon_core::Plan>,
}

#[wasm_bindgen]
impl Canon {
    /// Load a snapshot (the page agent's `capture()` result) with options (JSON, may be "{}").
    #[wasm_bindgen(constructor)]
    pub fn new(snapshot: &str, options: &str) -> Result<Canon, JsError> {
        let o: Opts = serde_json::from_str(if options.is_empty() { "{}" } else { options }).map_err(err)?;
        let d = Options::default();
        let opts = Options {
            simplify: o.simplify.unwrap_or(d.simplify),
            structural: o.reduce.unwrap_or(d.structural),
            max_trials: o.max_trials.unwrap_or(d.max_trials),
            fluid: o.fluid.unwrap_or(d.fluid),
            hoist: o.hoist.unwrap_or(d.hoist),
            tol: o.tolerance.unwrap_or(d.tol),
            // The page builds trials with DOM calls (see `Engine::write_tree`).
            tree: true,
            ..d
        };
        Ok(Canon { eng: Engine::load(snapshot, opts).map_err(err)?, last_plan: None })
    }

    /// Configuration for the page agent's `capture()`.
    #[wasm_bindgen(js_name = agentConfig)]
    pub fn agent_config() -> String {
        canon_core::agent_config().to_string()
    }

    /// Captured property names, for the agent's `init()` in the rendering frame.
    pub fn props(&self) -> String {
        serde_json::to_string(&self.eng.t.props).unwrap_or_default()
    }

    /// Snapshot info (viewport, rect, backdrop, flags...) as JSON.
    pub fn info(&self) -> String {
        serde_json::to_string(&self.eng.info).unwrap_or_default()
    }

    #[wasm_bindgen(js_name = probeKeys)]
    pub fn probe_keys(&self) -> String {
        serde_json::to_string(self.eng.probe_keys()).unwrap_or_default()
    }

    #[wasm_bindgen(js_name = setProbe)]
    pub fn set_probe(&mut self, json: &str) -> Result<(), JsError> {
        self.eng.set_probe(json).map_err(err)
    }

    #[wasm_bindgen(js_name = inheritTests)]
    pub fn inherit_tests(&mut self) -> String {
        serde_json::to_string(&self.eng.inherit_tests()).unwrap_or_default()
    }

    #[wasm_bindgen(js_name = setInherit)]
    pub fn set_inherit(&mut self, json: &str) -> Result<(), JsError> {
        self.eng.set_inherit(json).map_err(err)
    }

    /// Kinds of kept element to test per-tag inheritance on.
    #[wasm_bindgen(js_name = followClasses)]
    pub fn follow_classes(&self) -> String {
        serde_json::to_string(self.eng.follow_request().0).unwrap_or_default()
    }

    #[wasm_bindgen(js_name = setFollow)]
    pub fn set_follow(&mut self, json: &str) -> Result<(), JsError> {
        self.eng.set_follow(json).map_err(err)
    }

    /// `@font-face` rules the finished document carries (call before `emit`, not before
    /// `reduce`: trial documents get their fonts from the frame's FontFaceSet).
    #[wasm_bindgen(js_name = setFontCss)]
    pub fn set_font_css(&mut self, css: String) {
        self.eng.font_css = css;
    }

    /// Font files to fetch for the faces the page actually used. `sheets`: JSON
    /// `[[cssText, baseUrl]]` of the page's `@font-face` rules; `loaded`: the agent's
    /// `loadedFonts()`. Returns a JSON array of URLs.
    #[wasm_bindgen(js_name = fontUrls)]
    pub fn font_urls(sheets: &str, loaded: &str) -> Result<String, JsError> {
        let mut urls: Vec<String> = Vec::new();
        for face in needed_faces(sheets, loaded)? {
            canon_core::fonts::rule(&face, true, &mut |u| {
                if !urls.iter().any(|x| x == u) {
                    urls.push(u.to_string());
                }
                None
            });
        }
        serde_json::to_string(&urls).map_err(err)
    }

    /// `@font-face` rules for those faces. `fetched`: JSON `{url: base64}` of the files that
    /// could be read; the others stay referenced by URL. Returns JSON `{css, faces}`.
    #[wasm_bindgen(js_name = fontCss)]
    pub fn font_css(sheets: &str, loaded: &str, fetched: &str) -> Result<String, JsError> {
        let got: std::collections::HashMap<String, String> = serde_json::from_str(if fetched.is_empty() { "{}" } else { fetched }).map_err(err)?;
        let faces = needed_faces(sheets, loaded)?;
        let mut css = String::new();
        for face in &faces {
            if let Some(r) = canon_core::fonts::rule(face, true, &mut |u| got.get(u).cloned()) {
                css.push_str(&r);
            }
        }
        Ok(serde_json::json!({ "css": css, "faces": faces.len() }).to_string())
    }

    pub fn prepare(&mut self) -> Result<(), JsError> {
        self.eng.prepare().map_err(err)
    }

    /// Lay the unreduced copy out. Returns how many nodes it could not place exactly
    /// (those become their own reference).
    pub fn calibrate(&mut self, render: &js_sys::Function) -> Result<usize, JsError> {
        let mut r = JsRenderer { render, deadline: 0.0, error: None };
        let res = self.eng.calibrate(&mut r);
        if let Some(e) = r.error {
            return Err(JsError::new(&e));
        }
        Ok(res.map_err(err)?.len())
    }

    /// Remove every element the renderer cannot tell apart. `deadline`: a `Date.now()` value
    /// to stop at (0 for none); what is verified by then is kept.
    pub fn reduce(&mut self, render: &js_sys::Function, deadline: f64) -> Result<(), JsError> {
        let mut r = JsRenderer { render, deadline, error: None };
        let res = self.eng.reduce(&mut r);
        if let Some(e) = r.error {
            return Err(JsError::new(&e));
        }
        res.map_err(err)
    }

    /// The finished markup. `style`: "auto" | "inline" | "classes". Returns JSON:
    /// `{document, html, css, fontCss, elements, declarations}`. `hoist` / `fold` select the
    /// CSS savings to apply (the caller verifies and falls back).
    pub fn emit(&mut self, style: &str, hoist: bool, fold: bool, class_prefix: &str) -> Result<String, JsError> {
        let mode = match style {
            "" | "auto" => StyleMode::Auto,
            "inline" => StyleMode::Inline,
            "classes" => StyleMode::Classes,
            other => return Err(JsError::new(&format!("unknown style mode {other:?}"))),
        };
        let keep = self.eng.opts.hoist;
        self.eng.opts.hoist = hoist && keep;
        let w = self.eng.work.clone();
        let plan = self.eng.plan_final(&w);
        self.eng.opts.hoist = keep;
        let fin = self.eng.render_final(&plan, mode, fold, class_prefix);
        let tree: serde_json::Value = serde_json::from_str(&fin.tree).map_err(err)?;
        let out = serde_json::json!({
            "document": fin.document,
            "html": fin.html,
            "tree": tree,
            "css": fin.css,
            "fontCss": self.eng.font_css,
            "elements": fin.elements,
            "declarations": fin.declarations,
        });
        self.last_plan = Some(plan);
        Ok(out.to_string())
    }

    /// Check the document last returned by `emit`, as laid out by the browser: `measure` is
    /// the agent's `measure()` of it, `styles` its `styles()`. Returns JSON
    /// `{structure, geometry: [...], styles: [...]}`.
    pub fn check(&mut self, measure: &str, styles: &str) -> Result<String, JsError> {
        let plan = self.last_plan.as_ref().ok_or_else(|| JsError::new("check() needs emit() first"))?;
        let m = Measure::parse(measure).map_err(err)?;
        let (structure, geometry) = match self.eng.compare(plan, &m, 200) {
            Ok(()) => (true, Vec::new()),
            Err(canon_core::CompareError::Structure { .. }) => (false, Vec::new()),
            Err(canon_core::CompareError::Geometry(v)) => (true, v),
        };
        let style_bad = if structure { self.eng.check_styles(plan, styles).map_err(err)? } else { Vec::new() };
        let markup = self.eng.markup_problems(plan);
        let out = serde_json::json!({
            "structure": structure && markup == 0,
            "markupProblems": markup,
            "geometry": geometry.iter().map(|g| serde_json::json!({
                "index": g.index, "text": g.text, "expected": g.expected, "got": g.got,
            })).collect::<Vec<_>>(),
            "styles": style_bad.iter().map(|s| serde_json::json!({
                "element": s.element, "tag": s.tag, "property": s.property, "expected": s.expected, "got": s.got,
            })).collect::<Vec<_>>(),
        });
        Ok(out.to_string())
    }

    /// Where the copy's root should be placed in the rendering frame, `[x, y]` px, and the
    /// stage width (or null).
    pub fn stage(&self) -> String {
        let (x, y) = self.eng.target_origin();
        serde_json::json!({ "x": x, "y": y, "w": self.eng.stage_width(), "bg": self.eng.backdrop_css(), "doctype": self.eng.doctype() }).to_string()
    }

    /// Statistics, warnings and counts, as JSON.
    pub fn report(&self) -> String {
        serde_json::json!({
            "sourceElements": self.eng.source_elems,
            "stats": self.eng.stats,
            "warnings": self.eng.warnings,
        })
        .to_string()
    }
}
