pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod analyze;
pub mod diagnostic;
pub mod ir;
pub mod skills;
pub mod source;

#[cfg(test)]
mod tests {
    #[test]
    fn exposes_version() {
        assert_eq!(super::VERSION, "0.1.0");
    }
}
