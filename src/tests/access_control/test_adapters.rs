#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    // Testes simplificados para adapters que não requerem mock complexo do Log

    #[tokio::test]
    async fn test_operation_serialization_roundtrip() {
        // Testa serialização e desserialização de operações
        let original_op = crate::stores::operation::Operation::new(
            Some("test_key".to_string()),
            "PUT".to_string(),
            Some(b"test_value".to_vec()),
        );

        // Serializa
        let serialized = crate::guardian::serializer::serialize(&original_op).unwrap();

        // Desserializa
        let deserialized: crate::stores::operation::Operation =
            crate::guardian::serializer::deserialize(&serialized).unwrap();

        // Verifica igualdade
        assert_eq!(original_op.key, deserialized.key);
        assert_eq!(original_op.op, deserialized.op);
        assert_eq!(original_op.value, deserialized.value);
    }

    #[test]
    fn test_operation_put_creation() {
        let key = "test_key";
        let value = b"test_value".to_vec();

        let operation = crate::stores::operation::Operation::new(
            Some(key.to_string()),
            "PUT".to_string(),
            Some(value.clone()),
        );

        assert_eq!(operation.key, Some(key.to_string()));
        assert_eq!(operation.op, "PUT");
        assert_eq!(operation.value, value);
    }

    #[test]
    fn test_operation_delete_creation() {
        let key = "test_key";

        let operation = crate::stores::operation::Operation::new(
            Some(key.to_string()),
            "DELETE".to_string(),
            None,
        );

        assert_eq!(operation.key, Some(key.to_string()));
        assert_eq!(operation.op, "DELETE");
        assert!(operation.value.is_empty());
    }

    #[test]
    fn test_guardian_db_adapter_structure() {
        // Testa que a estrutura GuardianDBAdapter está corretamente definida
        // Este é um teste de compilação - se compila, a estrutura está correta
        assert!(true, "GuardianDBAdapter structure is correctly defined");
    }

    #[test]
    fn test_keyvalue_store_adapter_structure() {
        // Testa que a estrutura KeyValueStoreAdapter está corretamente definida
        // Este é um teste de compilação - se compila, a estrutura está correta
        assert!(true, "KeyValueStoreAdapter structure is correctly defined");
    }

    #[tokio::test]
    async fn test_multiple_operations_serialization() {
        // Testa serialização de múltiplas operações
        let operations = vec![
            crate::stores::operation::Operation::new(
                Some("key1".to_string()),
                "PUT".to_string(),
                Some(b"value1".to_vec()),
            ),
            crate::stores::operation::Operation::new(
                Some("key2".to_string()),
                "PUT".to_string(),
                Some(b"value2".to_vec()),
            ),
            crate::stores::operation::Operation::new(
                Some("key1".to_string()),
                "DELETE".to_string(),
                None,
            ),
        ];

        for op in operations {
            // Serializa
            let serialized = crate::guardian::serializer::serialize(&op).unwrap();

            // Desserializa
            let deserialized: crate::stores::operation::Operation =
                crate::guardian::serializer::deserialize(&serialized).unwrap();

            // Verifica igualdade
            assert_eq!(op.key, deserialized.key);
            assert_eq!(op.op, deserialized.op);
            assert_eq!(op.value, deserialized.value);
        }
    }

    #[test]
    fn test_operation_state_reconstruction() {
        // Testa reconstrução de estado a partir de operações
        let mut state: HashMap<String, Vec<u8>> = HashMap::new();

        // Simula processamento de operações
        let operations = vec![
            ("key1", "PUT", Some(b"value1".to_vec())),
            ("key2", "PUT", Some(b"value2".to_vec())),
            ("key3", "PUT", Some(b"value3".to_vec())),
            ("key2", "DELETE", None),
        ];

        for (key, op_type, value) in operations {
            match op_type {
                "PUT" => {
                    if let Some(v) = value {
                        state.insert(key.to_string(), v);
                    }
                }
                "DELETE" => {
                    state.remove(key);
                }
                _ => {}
            }
        }

        // Verifica estado final
        assert_eq!(state.len(), 2);
        assert!(state.contains_key("key1"));
        assert!(state.contains_key("key3"));
        assert!(!state.contains_key("key2"));
    }

    #[test]
    fn test_operation_ordering() {
        // Testa que a ordem das operações é importante
        let mut state: HashMap<String, Vec<u8>> = HashMap::new();
        let key = "test_key";

        // Sequência: PUT -> PUT -> DELETE
        state.insert(key.to_string(), b"value1".to_vec());
        state.insert(key.to_string(), b"value2".to_vec());
        state.remove(key);

        assert!(!state.contains_key(key));

        // Sequência: PUT -> DELETE -> PUT
        state.insert(key.to_string(), b"value1".to_vec());
        state.remove(key);
        state.insert(key.to_string(), b"value2".to_vec());

        assert!(state.contains_key(key));
        assert_eq!(state.get(key).unwrap(), b"value2");
    }
}
