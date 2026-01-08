use crate::guardian::error::{GuardianError, Result};
use iroh_blobs::Hash;
use std::fmt;

pub trait Address: fmt::Display + fmt::Debug + Send + Sync {
    /// Retorna o hash raiz do banco de dados.
    fn get_root(&self) -> Hash;

    /// Retorna o caminho do banco de dados.
    fn get_path(&self) -> &str;

    /// Helper method for equality comparison
    fn equals(&self, other: &dyn Address) -> bool;
}

/// SimpleAddress é uma implementação básica da trait Address
/// para fins de teste e prototipagem
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimpleAddress {
    path: String,
}

impl SimpleAddress {
    /// Cria um novo SimpleAddress com o caminho especificado
    pub fn new<S: Into<String>>(path: S) -> Self {
        Self { path: path.into() }
    }
}

impl Address for SimpleAddress {
    fn get_root(&self) -> Hash {
        // Retorna um hash zero para teste
        Hash::from([0u8; 32])
    }

    fn get_path(&self) -> &str {
        &self.path
    }

    fn equals(&self, other: &dyn Address) -> bool {
        self.get_path() == other.get_path()
    }
}

impl fmt::Display for SimpleAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardianDBAddress {
    root: Hash,
    path: String,
}

impl GuardianDBAddress {
    /// Cria um novo GuardianDBAddress com Hash e caminho especificados
    pub fn new(root: Hash, path: String) -> Self {
        Self { root, path }
    }

    /// Cria um GuardianDBAddress apenas com Hash (sem caminho)
    pub fn from_hash(root: Hash) -> Self {
        Self {
            root,
            path: String::new(),
        }
    }

    /// Verifica se o endereço tem um caminho associado
    pub fn has_path(&self) -> bool {
        !self.path.is_empty()
    }

    /// Retorna o endereço raiz (sem caminho)
    pub fn root_address(&self) -> Self {
        Self {
            root: self.root,
            path: String::new(),
        }
    }

    /// Adiciona ou modifica o caminho do endereço
    pub fn with_path<P: Into<String>>(mut self, path: P) -> Self {
        self.path = path.into();
        self
    }
}

impl Address for GuardianDBAddress {
    fn get_root(&self) -> Hash {
        self.root
    }

    fn get_path(&self) -> &str {
        &self.path
    }

    fn equals(&self, other: &dyn Address) -> bool {
        self.get_root() == other.get_root() && self.get_path() == other.get_path()
    }
}

/// Converte o endereço para sua representação em string, como "/GuardianDB/{hex_hash}/caminho".
impl fmt::Display for GuardianDBAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let base = format!("/GuardianDB/{}", hex::encode(self.root.as_bytes()));
        if self.path.is_empty() {
            write!(f, "{}", base)
        } else {
            write!(f, "{}/{}", base, self.path)
        }
    }
}

/// Verifica se uma dada string é um endereço GuardianDB sintaticamente válido.
pub fn is_valid(addr: &str) -> Result<()> {
    // Verifica se a string está vazia
    if addr.trim().is_empty() {
        return Err(GuardianError::InvalidArgument(
            "Endereço não pode estar vazio".to_string(),
        ));
    }

    // Remove o prefixo, se existir. unwrap_or retorna o slice original se o prefixo não for encontrado.
    let trimmed = addr.strip_prefix("/GuardianDB/").unwrap_or(addr);

    // Verifica se após remover o prefixo ainda há conteúdo
    if trimmed.is_empty() {
        return Err(GuardianError::InvalidArgument(
            "Endereço inválido: apenas o prefixo GuardianDB encontrado".to_string(),
        ));
    }

    // Pega a primeira parte, que deve ser o hash em hex (64 caracteres).
    let hash_part = trimmed.split('/').next().ok_or_else(|| {
        GuardianError::InvalidArgument("Endereço inválido: formato incorreto ou vazio".to_string())
    })?;

    // Verifica se o hash não está vazio
    if hash_part.is_empty() {
        return Err(GuardianError::InvalidArgument(
            "Endereço inválido: hash não pode estar vazio".to_string(),
        ));
    }

    // Valida que é um hash hex válido de 64 caracteres (32 bytes)
    if hash_part.len() != 64 {
        return Err(GuardianError::InvalidArgument(format!(
            "Endereço inválido: hash deve ter 64 caracteres hex, encontrado {}",
            hash_part.len()
        )));
    }

    // Tenta decodificar como hex
    hex::decode(hash_part).map_err(|e| {
        GuardianError::InvalidArgument(format!(
            "Endereço inválido: hash hex inválido '{}': {}",
            hash_part, e
        ))
    })?;

    Ok(())
}

/// Analisa uma string e retorna uma instância de `GuardianDBAddress` se for um endereço válido.
pub fn parse(addr: &str) -> Result<GuardianDBAddress> {
    // Primeiro, valida o endereço. Em caso de erro, retorna um erro mais descritivo.
    if is_valid(addr).is_err() {
        return Err(GuardianError::InvalidArgument(format!(
            "Não é um endereço GuardianDB válido: '{}'",
            addr
        )));
    }

    let trimmed = addr.strip_prefix("/GuardianDB/").unwrap_or(addr);

    // Divide a string em no máximo duas partes: o hash e o resto (o caminho).
    // Isso é mais eficiente que `split` e depois `join`.
    let mut parts = trimmed.splitn(2, '/');

    // `is_valid` já garantiu que a primeira parte existe e é um hash válido.
    // O `unwrap` aqui é seguro.
    let hash_part = parts.next().unwrap();

    // Decodifica hex para bytes
    let hash_bytes = hex::decode(hash_part).map_err(|e| {
        GuardianError::InvalidArgument(format!(
            "Endereço inválido: não foi possível decodificar o hash: {}",
            e
        ))
    })?;

    // Converte para array de 32 bytes
    if hash_bytes.len() != 32 {
        return Err(GuardianError::InvalidArgument(format!(
            "Endereço inválido: hash deve ter 32 bytes, encontrado {}",
            hash_bytes.len()
        )));
    }

    let mut hash_array = [0u8; 32];
    hash_array.copy_from_slice(&hash_bytes);
    let root_hash = Hash::from_bytes(hash_array);

    // A segunda parte, se existir, é o caminho. Caso contrário, é uma string vazia.
    let path = parts.next().unwrap_or("").to_string();

    Ok(GuardianDBAddress {
        root: root_hash,
        path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh_blobs::Hash;

    // Hash de teste (32 bytes = 64 chars hex)
    fn test_hash() -> Hash {
        Hash::from([0x12; 32])
    }

    fn test_hash_string() -> String {
        hex::encode(test_hash().as_bytes())
    }

    #[test]
    fn test_is_valid_success() {
        // Hash válido de teste (64 chars hex)
        let valid_addr = format!("/GuardianDB/{}", test_hash_string());
        assert!(is_valid(&valid_addr).is_ok());

        // Sem prefixo também deve funcionar
        let without_prefix = test_hash_string();
        assert!(is_valid(&without_prefix).is_ok());

        // Com caminho
        let with_path = format!("/GuardianDB/{}/path/to/resource", test_hash_string());
        assert!(is_valid(&with_path).is_ok());
    }

    #[test]
    fn test_is_valid_failures() {
        // String vazia
        assert!(is_valid("").is_err());
        assert!(is_valid("   ").is_err());

        // Apenas prefixo
        assert!(is_valid("/GuardianDB/").is_err());

        // Hash inválido (não é hex válido)
        assert!(is_valid("/GuardianDB/invalid-hash").is_err());
        assert!(is_valid("invalid-hash").is_err());

        // Hash muito curto
        assert!(is_valid("/GuardianDB/123abc").is_err());
    }

    #[test]
    fn test_parse_success() {
        let addr_str = format!("/GuardianDB/{}/path/to/resource", test_hash_string());
        let parsed = parse(&addr_str).unwrap();

        assert_eq!(parsed.get_path(), "path/to/resource");
        assert_eq!(parsed.to_string(), addr_str);
    }

    #[test]
    fn test_parse_without_path() {
        let addr_str = format!("/GuardianDB/{}", test_hash_string());
        let parsed = parse(&addr_str).unwrap();

        assert_eq!(parsed.get_path(), "");
        assert_eq!(parsed.to_string(), addr_str);
    }

    #[test]
    fn test_guardian_db_address_methods() {
        let hash = test_hash();

        // Test from_hash
        let addr = GuardianDBAddress::from_hash(hash);
        assert_eq!(addr.get_root(), hash);
        assert_eq!(addr.get_path(), "");
        assert!(!addr.has_path());

        // Test with_path
        let addr_with_path = addr.with_path("test/path");
        assert_eq!(addr_with_path.get_path(), "test/path");
        assert!(addr_with_path.has_path());

        // Test root_address
        let root_addr = addr_with_path.root_address();
        assert_eq!(root_addr.get_path(), "");
        assert!(!root_addr.has_path());
    }

    #[test]
    fn test_address_equality() {
        let hash = test_hash();
        let addr1 = GuardianDBAddress::new(hash, "path".to_string());
        let addr2 = GuardianDBAddress::new(hash, "path".to_string());
        let addr3 = GuardianDBAddress::new(hash, "different".to_string());

        assert!(addr1.equals(&addr2));
        assert!(!addr1.equals(&addr3));
    }
}
