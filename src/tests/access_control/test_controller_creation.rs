#[cfg(test)]
mod tests {
    use crate::access_control::manifest::{CreateAccessControllerOptions, ManifestParams};

    #[test]
    fn test_infer_controller_type_from_params() {
        // Testa inferência com tipo explícito nos parâmetros
        let mut params = CreateAccessControllerOptions::default();
        params.set_type("guardian".to_string());

        let inferred = crate::access_control::infer_controller_type("some/path", &params);
        assert_eq!(inferred, "guardian");
    }

    #[test]
    fn test_infer_controller_type_from_address_guardian() {
        let params = CreateAccessControllerOptions::default();

        // Testa endereço com "/guardian/"
        let inferred = crate::access_control::infer_controller_type("/guardian/db", &params);
        assert_eq!(inferred, "guardian");

        // Testa endereço com "guardian_"
        let inferred = crate::access_control::infer_controller_type("guardian_test", &params);
        assert_eq!(inferred, "guardian");
    }

    #[test]
    fn test_infer_controller_type_from_address_iroh() {
        let params = CreateAccessControllerOptions::default();

        // Testa endereço com "/iroh/"
        let inferred = crate::access_control::infer_controller_type("/iroh/db", &params);
        assert_eq!(inferred, "iroh");

        // Testa endereço com "iroh_"
        let inferred = crate::access_control::infer_controller_type("iroh_test", &params);
        assert_eq!(inferred, "iroh");
    }

    #[test]
    fn test_infer_controller_type_default_simple() {
        let params = CreateAccessControllerOptions::default();

        // Testa endereço genérico que deve retornar "simple"
        let inferred = crate::access_control::infer_controller_type("mydb/test", &params);
        assert_eq!(inferred, "simple");

        let inferred = crate::access_control::infer_controller_type("", &params);
        assert_eq!(inferred, "simple");
    }

    #[test]
    fn test_create_controller_options_default() {
        let options = CreateAccessControllerOptions::default();
        assert_eq!(options.get_type(), "");
        assert!(options.get_all_access().is_empty());
    }

    #[test]
    fn test_create_controller_options_with_type() {
        let mut options = CreateAccessControllerOptions::default();
        options.set_type("iroh".to_string());
        assert_eq!(options.get_type(), "iroh");
    }

    #[test]
    fn test_create_controller_options_with_access() {
        let mut options = CreateAccessControllerOptions::default();

        options.set_access(
            "write".to_string(),
            vec!["user1".to_string(), "user2".to_string()],
        );
        options.set_access("read".to_string(), vec!["*".to_string()]);

        let all_access = options.get_all_access();
        assert_eq!(all_access.len(), 2);
        assert_eq!(all_access.get("write").unwrap().len(), 2);
        assert_eq!(all_access.get("read").unwrap()[0], "*");
    }

    #[test]
    fn test_create_controller_options_get_specific_access() {
        let mut options = CreateAccessControllerOptions::default();
        options.set_access("write".to_string(), vec!["user1".to_string()]);

        let write_access = options.get_access("write");
        assert!(write_access.is_some());
        assert_eq!(write_access.unwrap()[0], "user1");

        let read_access = options.get_access("read");
        assert!(read_access.is_none());
    }
}
