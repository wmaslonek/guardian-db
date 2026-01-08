#[cfg(test)]
mod tests {
    use crate::access_control::manifest::{CreateAccessControllerOptions, ManifestParams};
    use crate::access_control::{ensure_address, validate_address};

    #[tokio::test]
    async fn test_create_access_controller_options_builder_pattern() {
        let mut options = CreateAccessControllerOptions::default();
        options.set_type("simple".to_string());

        options.set_access("write".to_string(), vec!["*".to_string()]);

        assert_eq!(options.get_type(), "simple");
        assert!(options.get_access("write").is_some());
    }

    #[test]
    fn test_ensure_and_validate_address_integration() {
        // Testa integração entre ensure_address e validate_address
        let address = "my/database";
        let ensured = ensure_address(address);
        assert_eq!(ensured, "my/database/_access");
        assert!(validate_address(&ensured).is_ok());
    }

    #[test]
    fn test_ensure_address_idempotent() {
        // ensure_address deve ser idempotente para endereços já corretos
        let address = "mydb/_access";
        let ensured_once = ensure_address(address);
        let ensured_twice = ensure_address(&ensured_once);
        assert_eq!(ensured_once, ensured_twice);
    }

    #[test]
    fn test_validate_address_with_special_characters() {
        // Testa caracteres especiais válidos
        assert!(validate_address("my-database").is_ok());
        assert!(validate_address("my_database").is_ok());
        assert!(validate_address("my.database").is_ok());
        assert!(validate_address("database123").is_ok());
    }

    #[test]
    fn test_controller_type_normalization() {
        // Testa que tipos são normalizados para lowercase
        let types = vec!["Simple", "GUARDIAN", "IrOh"];
        for t in types {
            assert!(crate::access_control::is_supported_type(t));
        }
    }

    #[tokio::test]
    async fn test_multiple_access_levels() {
        // Testa configuração com múltiplos níveis de acesso
        let mut options = CreateAccessControllerOptions::default();

        options.set_access("read".to_string(), vec!["*".to_string()]);
        options.set_access(
            "write".to_string(),
            vec!["admin".to_string(), "editor".to_string()],
        );
        options.set_access("delete".to_string(), vec!["admin".to_string()]);

        assert_eq!(options.get_access("read").unwrap().len(), 1);
        assert_eq!(options.get_access("write").unwrap().len(), 2);
        assert_eq!(options.get_access("delete").unwrap().len(), 1);
        assert!(options.get_access("unknown").is_none());
    }

    #[test]
    fn test_address_edge_cases() {
        // Testa casos extremos de endereços

        // Apenas o sufixo
        assert_eq!(ensure_address("_access"), "_access");

        // Múltiplos níveis
        assert_eq!(ensure_address("a/b/c/d/e/f"), "a/b/c/d/e/f/_access");

        // Com caracteres especiais
        assert_eq!(ensure_address("my-db_test.v1"), "my-db_test.v1/_access");
    }

    #[test]
    fn test_validate_address_boundary_cases() {
        // Testa exatamente no limite de 1000 caracteres
        let boundary_address = "a".repeat(1000);
        assert!(validate_address(&boundary_address).is_ok());

        // Testa 1 caractere além do limite
        let over_limit = "a".repeat(1001);
        assert!(validate_address(&over_limit).is_err());

        // Testa caminho válido longo mas dentro do limite
        // 249 + 1 + 250 + 1 + 250 + 1 + 248 = 1000
        let long_path = format!(
            "{}/{}/{}/{}",
            "a".repeat(249),
            "b".repeat(250),
            "c".repeat(250),
            "d".repeat(248)
        );
        assert_eq!(long_path.len(), 1000);
        assert!(validate_address(&long_path).is_ok());
    }

    #[test]
    fn test_permissions_empty_vs_wildcard() {
        let options1 = CreateAccessControllerOptions::default();
        assert!(options1.get_all_access().is_empty());

        let mut options2 = CreateAccessControllerOptions::default();
        options2.set_access("write".to_string(), vec!["*".to_string()]);

        assert!(!options2.get_all_access().is_empty());
        assert_eq!(options2.get_access("write").unwrap()[0], "*");
    }
}
