//! canon-core: recreate a captured piece of a page with the fewest elements and the least
//! CSS that still renders the same.
//!
//! The crate is pure: it never talks to a browser. It is handed a snapshot (DOM, computed
//! styles and box geometry, as the page agent reports them) and a [`Renderer`] it can ask
//! "lay this markup out and tell me where everything ended up". Everything it removes, it
//! removes because the renderer could not tell the difference.

// Styles are parallel arrays indexed by property id; indexing several of them in one loop
// is the clearest way to write most of this crate.
#![allow(clippy::needless_range_loop)]

pub mod css;
pub mod emit;
pub mod engine;
pub mod fonts;
pub mod geom;
pub mod hoist;
pub mod model;
pub mod props;
pub mod reduce;
pub mod style;

pub use emit::{Final, Plan, StyleMode};
pub use engine::{Engine, Options, ProbeKey, Stats};
pub use reduce::{CompareError, Doc, Measure, Mismatch, Renderer, StyleMismatch};

/// Configuration handed to the page agent: what not to capture and which tags keep their identity.
pub fn agent_config() -> serde_json::Value {
    let keep: serde_json::Map<String, serde_json::Value> =
        props::KEEP_HTML.iter().map(|(tag, attrs)| (tag.to_string(), serde_json::json!(attrs))).collect();
    serde_json::json!({
        "skip": props::SKIP_EXACT,
        "skipPrefix": props::SKIP_PREFIX,
        "keep": keep,
        "opaque": props::OPAQUE_HTML,
    })
}
