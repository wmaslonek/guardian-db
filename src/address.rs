use crate::error::{GuardianError, Result};
use cid::Cid;
use std::fmt;
use std::str::FromStr;

/// Equivalente à interface 'Address' em Go.
///
/// O método `String()` da interface Go é implementado em Rust através do trait `std::fmt::Display`.
pub trait Address: fmt::Display + fmt::Debug + Send + Sync {
    /// Equivalente à função 'GetRoot' em Go.
    /// Retorna o CID raiz do banco de dados.
    fn get_root(&self) -> Cid;

    /// Equivalente à função 'GetPath' em Go.
    /// Retorna o caminho do banco de dados.
    fn get_path(&self) -> &str;

    /// Helper method for equality comparison
    fn equals(&self, other: &dyn Address) -> bool;
}

/// Equivalente à struct 'address' em Go.
///
/// Esta é a implementação concreta da trait `Address`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardianDBAddress {
    root: Cid,
    path: String,
}

impl GuardianDBAddress {
    /// Cria um novo GuardianDBAddress com CID e caminho especificados
    pub fn new(root: Cid, path: String) -> Self {
        Self { root, path }
    }

    /// Cria um GuardianDBAddress apenas com CID (sem caminho)
    pub fn from_cid(root: Cid) -> Self {
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
            root: self.root.clone(),
            path: String::new(),
        }
    }

    /// Adiciona ou modifica o caminho do endereço
    pub fn with_path<P: Into<String>>(mut self, path: P) -> Self {
        self.path = path.into();
        self
    }
}

/// Implementação da trait Address.
impl Address for GuardianDBAddress {
    fn get_root(&self) -> Cid {
        // A struct Cid em Rust implementa `Clone`, então podemos retornar uma cópia.
        self.root.clone()
    }

    fn get_path(&self) -> &str {
        &self.path
    }

    fn equals(&self, other: &dyn Address) -> bool {
        self.get_root() == other.get_root() && self.get_path() == other.get_path()
    }
}

/// Equivalente à função 'String()' do tipo 'address' em Go.
///
/// Converte o endereço para sua representação em string, como "/GuardianDB/CID/caminho".
impl fmt::Display for GuardianDBAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let base = format!("/GuardianDB/{}", self.root);
        if self.path.is_empty() {
            write!(f, "{}", base)
        } else {
            write!(f, "{}/{}", base, self.path)
        }
    }
}

/// Equivalente à função 'IsValid' em Go.
///
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

    // Pega a primeira parte, que deve ser o CID.
    let cid_part = trimmed.split('/').next().ok_or_else(|| {
        GuardianError::InvalidArgument("Endereço inválido: formato incorreto ou vazio".to_string())
    })?;

    // Verifica se o CID não está vazio
    if cid_part.is_empty() {
        return Err(GuardianError::InvalidArgument(
            "Endereço inválido: CID não pode estar vazio".to_string(),
        ));
    }

    // A biblioteca `Cid::from_str` já falha para strings vazias, cobrindo as duas
    // verificações de erro do código Go original em uma única chamada.
    Cid::from_str(cid_part).map_err(|e| {
        GuardianError::InvalidArgument(format!(
            "Endereço inválido: não foi possível decodificar o CID '{}': {}",
            cid_part, e
        ))
    })?;

    Ok(())
}

/// Equivalente à função 'Parse' em Go.
///
/// Analisa uma string e retorna uma instância de `GuardianDBAddress` se for um endereço válido.
/// Em Rust, é mais idiomático retornar a struct concreta em vez de um `Box<dyn Address>`
/// para dar mais flexibilidade e melhor desempenho ao chamador.
pub fn parse(addr: &str) -> Result<GuardianDBAddress> {
    // Primeiro, valida o endereço. Em caso de erro, retorna um erro mais descritivo.
    if let Err(_) = is_valid(addr) {
        return Err(GuardianError::InvalidArgument(format!(
            "Não é um endereço GuardianDB válido: '{}'",
            addr
        )));
    }

    let trimmed = addr.strip_prefix("/GuardianDB/").unwrap_or(addr);

    // Divide a string em no máximo duas partes: o CID e o resto (o caminho).
    // Isso é mais eficiente que `split` e depois `join`.
    let mut parts = trimmed.splitn(2, '/');

    // `is_valid` já garantiu que a primeira parte existe e é um CID válido.
    // O `unwrap` aqui é seguro.
    let cid_part = parts.next().unwrap();
    let root_cid = Cid::from_str(cid_part).map_err(|e| {
        GuardianError::InvalidArgument(format!(
            "Endereço inválido: não foi possível decodificar o CID: {}",
            e
        ))
    })?;

    // A segunda parte, se existir, é o caminho. Caso contrário, é uma string vazia.
    let path = parts.next().unwrap_or("").to_string();

    Ok(GuardianDBAddress {
        root: root_cid,
        path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cid::Cid;
    use std::str::FromStr;

    #[test]
    fn test_is_valid_success() {
        // CID válido de teste
        let valid_addr = "/GuardianDB/QmYyQSo1c1Ym7orWxLYvCrM2EmxFTANf8wXmmE7DWjhx5N";
        assert!(is_valid(valid_addr).is_ok());

        // Sem prefixo também deve funcionar
        let without_prefix = "QmYyQSo1c1Ym7orWxLYvCrM2EmxFTANf8wXmmE7DWjhx5N";
        assert!(is_valid(without_prefix).is_ok());

        // Com caminho
        let with_path =
            "/GuardianDB/QmYyQSo1c1Ym7orWxLYvCrM2EmxFTANf8wXmmE7DWjhx5N/path/to/resource";
        assert!(is_valid(with_path).is_ok());
    }

    #[test]
    fn test_is_valid_failures() {
        // String vazia
        assert!(is_valid("").is_err());
        assert!(is_valid("   ").is_err());

        // Apenas prefixo
        assert!(is_valid("/GuardianDB/").is_err());

        // CID inválido
        assert!(is_valid("/GuardianDB/invalid-cid").is_err());
        assert!(is_valid("invalid-cid").is_err());
    }

    #[test]
    fn test_parse_success() {
        let addr_str =
            "/GuardianDB/QmYyQSo1c1Ym7orWxLYvCrM2EmxFTANf8wXmmE7DWjhx5N/path/to/resource";
        let parsed = parse(addr_str).unwrap();

        assert_eq!(parsed.get_path(), "path/to/resource");
        assert_eq!(parsed.to_string(), addr_str);
    }

    #[test]
    fn test_parse_without_path() {
        let addr_str = "/GuardianDB/QmYyQSo1c1Ym7orWxLYvCrM2EmxFTANf8wXmmE7DWjhx5N";
        let parsed = parse(addr_str).unwrap();

        assert_eq!(parsed.get_path(), "");
        assert_eq!(parsed.to_string(), addr_str);
    }

    #[test]
    fn test_guardian_db_address_methods() {
        let cid = Cid::from_str("QmYyQSo1c1Ym7orWxLYvCrM2EmxFTANf8wXmmE7DWjhx5N").unwrap();

        // Test from_cid
        let addr = GuardianDBAddress::from_cid(cid.clone());
        assert_eq!(addr.get_root(), cid);
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
        let cid = Cid::from_str("QmYyQSo1c1Ym7orWxLYvCrM2EmxFTANf8wXmmE7DWjhx5N").unwrap();
        let addr1 = GuardianDBAddress::new(cid.clone(), "path".to_string());
        let addr2 = GuardianDBAddress::new(cid.clone(), "path".to_string());
        let addr3 = GuardianDBAddress::new(cid.clone(), "different".to_string());

        assert!(addr1.equals(&addr2));
        assert!(!addr1.equals(&addr3));
    }
}
