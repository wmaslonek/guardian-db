use cid::Cid;
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

/// Define os erros que podem ocorrer ao analisar um endereço GuardianDB.
/// Equivalente aos múltiplos `fmt.Errorf` e `errors.Wrap` no código Go.
#[derive(Debug, Error)]
pub enum AddressError {
    #[error("Endereço inválido: não foi possível decodificar o CID: {0}")]
    InvalidCid(#[from] cid::Error),

    #[error("Endereço inválido: formato incorreto ou vazio")]
    InvalidFormat,

    #[error("Não é um endereço GuardianDB válido: '{address}'")]
    ValidationFailed { address: String },
}

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
pub fn is_valid(addr: &str) -> Result<(), AddressError> {
    // Remove o prefixo, se existir. unwrap_or retorna o slice original se o prefixo não for encontrado.
    let trimmed = addr.strip_prefix("/GuardianDB/").unwrap_or(addr);

    // Pega a primeira parte, que deve ser o CID.
    let cid_part = trimmed.split('/').next().ok_or(AddressError::InvalidFormat)?;

    // A biblioteca `Cid::from_str` já falha para strings vazias, cobrindo as duas
    // verificações de erro do código Go original em uma única chamada.
    Cid::from_str(cid_part)?;

    Ok(())
}

/// Equivalente à função 'Parse' em Go.
///
/// Analisa uma string e retorna uma instância de `GuardianDBAddress` se for um endereço válido.
/// Em Rust, é mais idiomático retornar a struct concreta em vez de um `Box<dyn Address>`
/// para dar mais flexibilidade e melhor desempenho ao chamador.
pub fn parse(addr: &str) -> Result<GuardianDBAddress, AddressError> {
    // Primeiro, valida o endereço. Em caso de erro, o encapsulamos (wrap) como no código Go.
    if let Err(e) = is_valid(addr) {
        // O `errors.Wrap` em Go é simulado aqui retornando um erro mais descritivo.
        return Err(AddressError::ValidationFailed {
            address: addr.to_string(),
        });
    }

    let trimmed = addr.strip_prefix("/GuardianDB/").unwrap_or(addr);

    // Divide a string em no máximo duas partes: o CID e o resto (o caminho).
    // Isso é mais eficiente que `split` e depois `join`.
    let mut parts = trimmed.splitn(2, '/');

    // `is_valid` já garantiu que a primeira parte existe e é um CID válido.
    // O `unwrap` aqui é seguro.
    let cid_part = parts.next().unwrap();
    let root_cid = Cid::from_str(cid_part)?; // O erro é propagado com `?`.

    // A segunda parte, se existir, é o caminho. Caso contrário, é uma string vazia.
    let path = parts.next().unwrap_or("").to_string();

    Ok(GuardianDBAddress {
        root: root_cid,
        path,
    })
}