use crate::error::{GuardianError, Result};
use crate::ipfs_log::entry::Entry;
use serde::{Deserialize, Serialize};
use serde_json;

/// Representa um documento em uma operação de log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OpDoc {
    #[serde(rename = "key")]
    pub key: String,

    #[serde(rename = "value")]
    pub value: Vec<u8>,
}

impl OpDoc {
    /// Cria um novo OpDoc
    pub fn new(key: String, value: Vec<u8>) -> Self {
        Self { key, value }
    }

    /// Getter para key
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Getter para value
    pub fn value(&self) -> &[u8] {
        &self.value
    }
}

/// Representa uma operação a ser adicionada ao log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Operation {
    #[serde(rename = "key", skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,

    #[serde(rename = "op")]
    pub op: String,

    #[serde(rename = "value", skip_serializing_if = "Vec::is_empty", default)]
    pub value: Vec<u8>,

    #[serde(rename = "docs", skip_serializing_if = "Vec::is_empty", default)]
    pub docs: Vec<OpDoc>,

    #[serde(skip)]
    pub entry: Option<Entry>, // Mudado para Option para indicar quando não está disponível
}

impl Operation {
    /// Cria uma nova operação básica
    pub fn new(key: Option<String>, op: String, value: Option<Vec<u8>>) -> Self {
        Self {
            key,
            op,
            value: value.unwrap_or_default(),
            docs: Vec::new(),
            entry: None,
        }
    }

    /// Cria uma nova operação com documentos (para operações batch)
    pub fn new_with_documents(
        key: Option<String>,
        op: String,
        docs: Vec<(String, Vec<u8>)>,
    ) -> Self {
        let docs = docs.into_iter().map(|(k, v)| OpDoc::new(k, v)).collect();
        Self {
            key,
            op,
            value: Vec::new(),
            docs,
            entry: None,
        }
    }

    /// Getters públicos para acessar os campos
    pub fn key(&self) -> Option<&String> {
        self.key.as_ref()
    }

    pub fn op(&self) -> &str {
        &self.op
    }

    pub fn value(&self) -> &[u8] {
        &self.value
    }

    pub fn docs(&self) -> &[OpDoc] {
        &self.docs
    }

    pub fn entry(&self) -> Option<&Entry> {
        self.entry.as_ref()
    }

    /// Define a entry após a criação da operação
    pub fn set_entry(&mut self, entry: Entry) {
        self.entry = Some(entry);
    }

    /// Verifica se a operação tem uma entry válida
    pub fn has_entry(&self) -> bool {
        self.entry.is_some()
    }

    /// Serializa a operação para JSON
    pub fn marshal(&self) -> std::result::Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }
}

/// Analisa um Entry do IPFS log para extrair os dados da operação.
pub fn parse_operation(entry: Entry) -> Result<Operation> {
    // Desserializa o payload JSON da entry para uma Operation
    let mut op: Operation = serde_json::from_str(entry.payload()).map_err(|err| {
        GuardianError::Store(format!("Incapaz de analisar o JSON da operação: {}", err))
    })?;

    // Atribui a entrada à operação após a desserialização bem-sucedida
    op.entry = Some(entry);

    Ok(op)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipfs_log::identity::Identity;

    fn create_test_identity() -> Identity {
        let signatures =
            crate::ipfs_log::identity::Signatures::new("test_id_sign", "test_pub_sign");
        Identity::new("test_peer", "test_pub_key", signatures)
    }

    #[test]
    fn test_op_doc_creation() {
        let doc = OpDoc::new("test_key".to_string(), b"test_value".to_vec());
        assert_eq!(doc.key(), "test_key");
        assert_eq!(doc.value(), b"test_value");
    }

    #[test]
    fn test_operation_new() {
        let op = Operation::new(
            Some("test_key".to_string()),
            "PUT".to_string(),
            Some(b"test_data".to_vec()),
        );

        assert_eq!(op.key(), Some(&"test_key".to_string()));
        assert_eq!(op.op(), "PUT");
        assert_eq!(op.value(), b"test_data");
        assert!(op.docs().is_empty());
        assert!(!op.has_entry());
    }

    #[test]
    fn test_operation_with_documents() {
        let docs = vec![
            ("key1".to_string(), b"value1".to_vec()),
            ("key2".to_string(), b"value2".to_vec()),
        ];

        let op = Operation::new_with_documents(None, "PUTALL".to_string(), docs);

        assert_eq!(op.key(), None);
        assert_eq!(op.op(), "PUTALL");
        assert!(op.value().is_empty());
        assert_eq!(op.docs().len(), 2);
        assert_eq!(op.docs()[0].key(), "key1");
        assert_eq!(op.docs()[1].key(), "key2");
    }

    #[test]
    fn test_operation_marshal() {
        let op = Operation::new(
            Some("test".to_string()),
            "PUT".to_string(),
            Some(b"data".to_vec()),
        );

        let json = op.marshal().expect("Failed to marshal operation");
        let json_str = String::from_utf8(json).expect("Invalid UTF-8");

        // Verifica se contém os campos esperados
        assert!(json_str.contains("\"key\":\"test\""));
        assert!(json_str.contains("\"op\":\"PUT\""));
    }

    #[test]
    fn test_operation_set_entry() {
        let mut op = Operation::new(
            Some("test".to_string()),
            "PUT".to_string(),
            Some(b"data".to_vec()),
        );

        assert!(!op.has_entry());

        let identity = create_test_identity();
        let entry = Entry::new(identity, "test_log", "test_payload", &[], None);

        op.set_entry(entry);
        assert!(op.has_entry());
        assert!(op.entry().is_some());
    }

    #[test]
    fn test_parse_operation() {
        // Cria uma operação e a serializa
        let original_op = Operation::new(
            Some("parse_test".to_string()),
            "GET".to_string(),
            Some(b"parse_data".to_vec()),
        );

        let json_data = original_op.marshal().expect("Failed to marshal");
        let json_str = String::from_utf8(json_data).expect("Invalid UTF-8");

        // Cria uma entry com o payload JSON
        let identity = create_test_identity();
        let entry = Entry::new(identity, "test_log", &json_str, &[], None);

        // Testa o parse
        let parsed_op = parse_operation(entry).expect("Failed to parse operation");

        assert_eq!(parsed_op.key(), Some(&"parse_test".to_string()));
        assert_eq!(parsed_op.op(), "GET");
        assert_eq!(parsed_op.value(), b"parse_data");
        assert!(parsed_op.has_entry());
    }
}
