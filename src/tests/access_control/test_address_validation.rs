use crate::access_control::{
    ensure_address, is_supported_type, list_available_types, validate_address,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ensure_address_empty() {
        assert_eq!(ensure_address(""), "_access");
    }

    #[test]
    fn test_ensure_address_whitespace() {
        assert_eq!(ensure_address("   "), "_access");
    }

    #[test]
    fn test_ensure_address_already_has_suffix() {
        assert_eq!(ensure_address("mydb/_access"), "mydb/_access");
        assert_eq!(ensure_address("/path/to/_access"), "/path/to/_access");
    }

    #[test]
    fn test_ensure_address_without_suffix() {
        assert_eq!(ensure_address("mydb"), "mydb/_access");
        assert_eq!(ensure_address("/path/to/db"), "/path/to/db/_access");
    }

    #[test]
    fn test_ensure_address_with_trailing_slash() {
        assert_eq!(ensure_address("mydb/"), "mydb/_access");
        assert_eq!(ensure_address("/path/to/db/"), "/path/to/db/_access");
    }

    #[test]
    fn test_ensure_address_complex_paths() {
        assert_eq!(
            ensure_address("org/project/database"),
            "org/project/database/_access"
        );
        assert_eq!(
            ensure_address("org/project/database/"),
            "org/project/database/_access"
        );
    }

    #[test]
    fn test_ensure_address_already_ends_with_access_slash() {
        // Edge case: "foo/_access/" tem next_back() == Some("")
        // ensure_address deve adicionar "_access" novamente
        assert_eq!(ensure_address("mydb/_access/"), "mydb/_access/_access");
    }

    #[test]
    fn test_validate_address_valid() {
        assert!(validate_address("mydb/_access").is_ok());
        assert!(validate_address("/path/to/db/_access").is_ok());
        assert!(validate_address("simple_address").is_ok());
    }

    #[test]
    fn test_validate_address_empty() {
        assert!(validate_address("").is_err());
        assert!(validate_address("   ").is_err());
    }

    #[test]
    fn test_validate_address_invalid_double_dots() {
        assert!(validate_address("path/../other").is_err());
        assert!(validate_address("..").is_err());
    }

    #[test]
    fn test_validate_address_invalid_double_slashes() {
        assert!(validate_address("path//other").is_err());
        assert!(validate_address("//path").is_err());
    }

    #[test]
    fn test_validate_address_too_long() {
        let long_address = "a".repeat(1001);
        assert!(validate_address(&long_address).is_err());
    }

    #[test]
    fn test_validate_address_max_length() {
        let max_address = "a".repeat(1000);
        assert!(validate_address(&max_address).is_ok());
    }

    #[test]
    fn test_list_available_types() {
        let types = list_available_types();
        assert_eq!(types.len(), 3);
        assert!(types.contains(&"simple".to_string()));
        assert!(types.contains(&"guardian".to_string()));
        assert!(types.contains(&"iroh".to_string()));
    }

    #[test]
    fn test_is_supported_type_valid() {
        assert!(is_supported_type("simple"));
        assert!(is_supported_type("guardian"));
        assert!(is_supported_type("iroh"));
    }

    #[test]
    fn test_is_supported_type_case_insensitive() {
        assert!(is_supported_type("Simple"));
        assert!(is_supported_type("GUARDIAN"));
        assert!(is_supported_type("IrOh"));
    }

    #[test]
    fn test_is_supported_type_invalid() {
        assert!(!is_supported_type("unknown"));
        assert!(!is_supported_type("custom"));
        assert!(!is_supported_type(""));
    }
}
