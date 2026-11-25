pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod analyze;
pub mod codegen;
pub mod diagnostic;
// Test-only: this module's only item is its `tests` module. Publishing it as a
// public module advertised an empty slice of the crate surface.
#[cfg(test)]
mod instruction;
pub mod ir;
pub mod skills;
pub mod source;
pub mod strip;

#[cfg(test)]
mod tests {
    #[test]
    fn exposes_version() {
        assert_eq!(super::VERSION, "0.1.0");
    }
}
