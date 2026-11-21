//! Analysis of a parsed route file.
//!
//! The parser in [`crate::source`] answers "is this syntactically valid TSX?".
//! Everything here answers questions about the *program* that was parsed: which
//! node is the route's component, what shape it has, and which rules it breaks.

pub mod component;
pub mod props;

pub use component::{
    ComponentInfo, ComponentKind, find_components, find_route_component, validate_route_component,
};
pub use props::check_props;
