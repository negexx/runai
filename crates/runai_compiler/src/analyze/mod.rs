//! Analysis of a parsed route file.
//!
//! The parser in [`crate::source`] answers "is this syntactically valid TSX?".
//! Everything here answers questions about the *program* that was parsed: which
//! node is the route's component, what shape it has, and which rules it breaks.
//!
//! Every rule here runs on the **original** source, before types are stripped,
//! so every span it reports is a span the agent can find in their own file.

pub mod component;
pub mod entry;
pub mod jsx;
pub mod props;
pub mod rules;
pub mod signals;

pub use component::{
    ComponentInfo, ComponentKind, find_components, find_route_component, validate_route_component,
};
pub use entry::analyze;
pub use jsx::check_jsx;
pub use props::check_props;
pub use rules::{check_signal_rules, check_static_mode};
pub use signals::{AccessorCall, SignalBindings, find_signal_bindings};
