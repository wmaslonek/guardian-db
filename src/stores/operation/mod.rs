use crate::guardian::error::{GuardianError, Result};
use crate::guardian::serializer;
use crate::log::entry::Entry;
use serde::{Deserialize, Serialize};

pub mod traits;

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
    #[serde(rename = "key")]
    pub key: Option<String>,

    #[serde(rename = "op")]
    pub op: String,

    #[serde(rename = "value", default)]
    pub value: Vec<u8>,

    #[serde(rename = "docs", default)]
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

    /// Serializa a operação para bytes usando postcard
    pub fn marshal(&self) -> Result<Vec<u8>> {
        serializer::serialize(self)
    }
}

/// Analisa um Entry do Log para extrair os dados da operação.
///
/// **NOTA:** Entry.payload agora é Vec<u8> (migrado na Fase 3).
pub fn parse_operation(entry: Entry) -> Result<Operation> {
    // Payload contém dados base64-encoded, decodifica primeiro
    let payload_bytes = entry.payload();

    // Decodifica base64 para obter os bytes originais da serialização Postcard
    use base64::{Engine as _, engine::general_purpose};
    let decoded_bytes = general_purpose::STANDARD
        .decode(payload_bytes)
        .map_err(|err| GuardianError::Store(format!("Unable to decode base64 payload: {}", err)))?;

    // Desserializa o payload usando postcard
    let mut op: Operation = serializer::deserialize(&decoded_bytes).map_err(|err| {
        GuardianError::Store(format!("Unable to parse operation payload: {}", err))
    })?;

    // Atribui a entrada à operação após a desserialização bem-sucedida
    op.entry = Some(entry);

    Ok(op)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::identity::Identity;

    fn create_test_identity() -> Identity {
        let signatures = crate::log::identity::Signatures::new("test_id_sign", "test_pub_sign");
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
            Some(b"value".to_vec()),
        );

        let bytes = op.marshal().expect("Failed to marshal operation");

        // Postcard é binário, então apenas verificamos que não está vazio
        assert!(!bytes.is_empty());
        println!("✅ Marshaled operation: {} bytes", bytes.len());
    }

    #[test]
    fn test_operation_marshal_determinism() {
        let op = Operation::new(
            Some("determinism_test".to_string()),
            "PUT".to_string(),
            Some(b"test_data".to_vec()),
        );

        // Serializa 3 vezes
        let hashes: Vec<String> = (0..3)
            .map(|_| {
                let bytes = op.marshal().expect("Marshal failed");
                blake3::hash(&bytes).to_hex().to_string()
            })
            .collect();

        // Todos os hashes devem ser idênticos
        let first = &hashes[0];
        for hash in &hashes[1..] {
            assert_eq!(first, hash, "Operation serialization not deterministic!");
        }

        println!("✅ Operation deterministic hash: {}", first);
    }

    #[test]
    fn test_operation_set_entry() {
        let mut op = Operation::new(
            Some("test".to_string()),
            "PUT".to_string(),
            Some(b"value".to_vec()),
        );

        let identity = create_test_identity();
        let entry = Entry::new(identity, "test_log", b"test_payload", &[], None);

        op.set_entry(entry);
        assert!(op.has_entry());

        println!("✅ Operation set_entry successful");
    }

    #[test]
    fn test_operation_roundtrip() {
        let original = Operation::new_with_documents(
            Some("batch_key".to_string()),
            "PUTALL".to_string(),
            vec![
                ("doc1".to_string(), b"value1".to_vec()),
                ("doc2".to_string(), b"value2".to_vec()),
                ("doc3".to_string(), b"value3".to_vec()),
            ],
        );

        // Serializa
        let bytes = original.marshal().expect("Marshal failed");

        // Desserializa
        let deserialized: Operation = serializer::deserialize(&bytes).expect("Deserialize failed");

        // Valida
        assert_eq!(original.key(), deserialized.key());
        assert_eq!(original.op(), deserialized.op());
        assert_eq!(original.docs().len(), deserialized.docs().len());

        for (orig_doc, deser_doc) in original.docs().iter().zip(deserialized.docs().iter()) {
            assert_eq!(orig_doc.key(), deser_doc.key());
            assert_eq!(orig_doc.value(), deser_doc.value());
        }

        println!("✅ Complex operation roundtrip successful");
    }

    #[test]
    fn test_parse_operation() {
        // Cria uma operação e a serializa usando postcard
        let original_op = Operation::new(
            Some("parse_test".to_string()),
            "GET".to_string(),
            Some(b"parse_data".to_vec()),
        );

        let postcard_data = original_op.marshal().expect("Failed to marshal");

        // Codifica em base64 como faz add_operation
        use base64::{Engine as _, engine::general_purpose};
        let base64_data = general_purpose::STANDARD.encode(&postcard_data);

        // Cria uma entry com o payload base64
        let identity = create_test_identity();
        let entry = Entry::new(identity, "test_log", base64_data.as_bytes(), &[], None);

        // Testa o parse
        let parsed_op = parse_operation(entry).expect("Failed to parse operation");

        assert_eq!(parsed_op.key(), Some(&"parse_test".to_string()));
        assert_eq!(parsed_op.op(), "GET");
        assert_eq!(parsed_op.value(), b"parse_data");
        assert!(parsed_op.has_entry());
    }
}
