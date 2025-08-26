use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use serde_json;
use crate::error::{GuardianError, Result};
use crate::eqlabs_ipfs_log::entry::Entry;

// Trait que define o comportamento de um documento de operação (OpDoc).
pub trait OpDocTrait {
    fn key(&self) -> &str;
    fn value(&self) -> &[u8];
}

// Trait que define o comportamento de uma operação.
// Em Rust, é idiomático retornar referências (&str, &[u8]) em vez de
// tomar posse dos dados (String, Vec<u8>) em getters.
pub trait OperationTrait {
    fn key(&self) -> Option<&String>;
    fn op(&self) -> &str;
    fn value(&self) -> &[u8];
    fn docs(&self) -> &[OpDoc];
    fn entry(&self) -> &Entry;
    fn marshal(&self) -> std::result::Result<Vec<u8>, serde_json::Error>;
}


/// Representa um documento dentro de uma operação (equivalente a `opDoc` em Go).
/// Os atributos `#[serde(rename = "...")]` garantem que os nomes dos campos
/// no JSON sejam os mesmos que no código Go.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OpDoc {
    #[serde(rename = "key")]
    key: String,
    
    #[serde(rename = "value")]
    value: Vec<u8>,
}

impl OpDocTrait for OpDoc {
    fn key(&self) -> &str {
        &self.key
    }

    fn value(&self) -> &[u8] {
        &self.value
    }
}


/// Representa uma operação a ser adicionada ao log (equivalente a `operation` em Go).
/// `Option<T>` é o equivalente idiomático em Rust para ponteiros que podem ser nulos em Go (ex: *string).
/// `#[serde(skip_serializing_if = "...")]` simula o comportamento de `omitempty` do Go.
/// `#[serde(skip)]` impede que o campo `entry` seja serializado, equivalente a `json:"-"`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Operation {
    #[serde(rename = "key", skip_serializing_if = "Option::is_none")]
    key: Option<String>,
    
    #[serde(rename = "op")]
    op: String,
    
    #[serde(rename = "value", skip_serializing_if = "Vec::is_empty")]
    value: Vec<u8>,
    
    #[serde(rename = "docs", skip_serializing_if = "Vec::is_empty")]
    docs: Vec<OpDoc>,
    
    #[serde(skip)]
    entry: Entry, // Assumindo que este campo é preenchido após a criação
}

/// Abaixo equivalente a GetDocs em go
///
/// Este método faz parte do `impl OperationTrait for Operation`.
/// A versão em Rust é mais eficiente, pois retorna uma referência de slice (`&[OpDoc]`)
/// em vez de alocar e preencher um novo vetor como a versão em Go.
/// equivalente a Marshal em go
///
/// Este método também faz parte do `impl OperationTrait for Operation`.
/// A biblioteca `serde` simplifica enormemente a serialização. Ao derivar `Serialize`
// na struct `Operation`, o `serde_json` já sabe como convertê-la para JSON.
impl Operation {
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

    pub fn entry(&self) -> &Entry {
        &self.entry
    }
    
    pub fn marshal(&self) -> std::result::Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }
}

// Implementação do Trait para a struct Operation
impl OperationTrait for Operation {
    fn key(&self) -> Option<&String> {
        self.key.as_ref()
    }

    fn op(&self) -> &str {
        &self.op
    }

    fn value(&self) -> &[u8] {
        &self.value
    }

    fn docs(&self) -> &[OpDoc] {
        &self.docs
    }

    fn entry(&self) -> &Entry {
        &self.entry
    }
    
    fn marshal(&self) -> std::result::Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }
}

/// equivalente a NewOpDoc em go
pub fn new_op_doc(key: String, value: Vec<u8>) -> OpDoc {
    OpDoc {
        key,
        value,
    }
}


/// equivalente a NewOperation em go
/// Retorna uma `Box<dyn OperationTrait>` para espelhar o retorno da interface em Go.
pub fn new_operation(key: Option<String>, op: String, value: Vec<u8>) -> Box<dyn OperationTrait> {
    Box::new(Operation {
        key,
        op,
        value,
        docs: Vec::new(),
        // O campo `entry` é deixado com seu valor padrão, pois não é fornecido na criação.
        // Uma implementação real poderia exigir um `entry` padrão ou `Option<IpfslLogEntry>`.
        entry: Entry::empty(), 
    })
}

impl Operation {
    /// Cria uma nova instância diretamente
    pub fn new(key: Option<String>, op: String, value: Option<Vec<u8>>) -> Self {
        Self {
            key,
            op,
            value: value.unwrap_or_default(),
            docs: Vec::new(),
            entry: Entry::empty(),
        }
    }

    /// Cria uma nova operação com documentos
    pub fn new_with_documents(key: Option<String>, op: String, docs: Vec<(String, Vec<u8>)>) -> Self {
        let docs = docs.into_iter().map(|(k, v)| OpDoc { key: k, value: v }).collect();
        Self {
            key,
            op,
            value: Vec::new(),
            docs,
            entry: Entry::empty(),
        }
    }
}


/// equivalente a NewOperationWithDocuments em go
pub fn new_operation_with_documents(key: Option<String>, op: String, docs: HashMap<String, Vec<u8>>) -> Box<dyn OperationTrait> {
    let _docs: Vec<OpDoc> = docs.into_iter().map(|(k, v)| {
        OpDoc { key: k, value: v }
    }).collect();

    Box::new(Operation {
        key,
        op,
        value: Vec::new(), // O original em Go não inicializa `value` neste caso
        docs: _docs,
        entry: Entry::empty(), // Valor padrão
    })
}

/// equivalente a ParseOperation em go
///
/// Analisa um IpfslLogEntry para extrair os dados da operação.
/// Retorna um Result, que é a forma idiomática do Rust para lidar com erros,
/// substituindo o padrão (valor, erro) do Go.
pub fn parse_operation(e: Entry) -> Result<Operation> {
    // Em Go, `json.Unmarshal` modifica a variável `op`.
    // Em Rust, `serde_json::from_slice` retorna uma nova instância da struct em caso de sucesso.
    let mut op: Operation = serde_json::from_str(e.payload())
        .map_err(|err| GuardianError::Store(format!("incapaz de analisar o json da operação: {}", err)))?;

    // Atribui a entrada à operação após a desserialização bem-sucedida.
    op.entry = e;

    Ok(op)
}