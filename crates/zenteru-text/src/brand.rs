//! Product names. Each script gets its own name.

pub const NAME_EN: &str = "zenteru";
pub const NAME_KO: &str = "젠테루";
pub const NAME_JA: &str = "ゼンテール";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// "zenteru  젠테루  ゼンテール"
pub fn full_name() -> String {
    format!("{NAME_EN}  {NAME_KO}  {NAME_JA}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::str_width;

    #[test]
    fn localized_names_are_full_width() {
        assert_eq!(str_width(NAME_KO), 6);
        assert_eq!(str_width(NAME_JA), 10);
    }
}
