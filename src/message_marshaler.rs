use crate::error::GuardianError;
use crate::iface::{MessageExchangeHeads, MessageMarshaler};
use serde_json;

/// Wrapper que adapta serde_json::Error para GuardianError
pub struct GuardianJSONMarshaler {
    inner: JSONMarshaler,
}

impl Default for GuardianJSONMarshaler {
    fn default() -> Self {
        Self::new()
    }
}

impl GuardianJSONMarshaler {
    pub fn new() -> Self {
        Self {
            inner: JSONMarshaler::new(),
        }
    }
}

impl MessageMarshaler for GuardianJSONMarshaler {
    type Error = GuardianError;

    fn marshal(&self, m: &MessageExchangeHeads) -> std::result::Result<Vec<u8>, Self::Error> {
        self.inner
            .marshal(m)
            .map_err(|e| GuardianError::Other(format!("Marshal error: {}", e)))
    }

    fn unmarshal(&self, data: &[u8]) -> std::result::Result<MessageExchangeHeads, Self::Error> {
        self.inner
            .unmarshal(data)
            .map_err(|e| GuardianError::Other(format!("Unmarshal error: {}", e)))
    }
}

/// Uma struct vazia que implementa o trait `MessageMarshaler`
/// para codificar e decodificar mensagens no formato JSON.
#[derive(Default, Debug, Clone, Copy)]
pub struct JSONMarshaler;

impl JSONMarshaler {
    pub fn new() -> Self {
        Self
    }
}

// Implementação do trait `MessageMarshaler` para a struct `JSONMarshaler`.
impl MessageMarshaler for JSONMarshaler {
    // O tipo de erro retornado pelas funções é o erro padrão de `serde_json`.
    type Error = serde_json::Error;

    /// Serializa uma struct `MessageExchangeHeads` para um vetor de bytes (`Vec<u8>`)
    /// usando `serde_json::to_vec`.
    fn marshal(&self, m: &MessageExchangeHeads) -> std::result::Result<Vec<u8>, Self::Error> {
        serde_json::to_vec(m)
    }

    /// Desserializa um slice de bytes (`&[u8]`) para uma struct `MessageExchangeHeads`
    /// usando `serde_json::from_slice`.
    fn unmarshal(&self, data: &[u8]) -> std::result::Result<MessageExchangeHeads, Self::Error> {
        serde_json::from_slice(data)
    }
}
