// TODO: Implementar utils quando BaseGuardianDB trait estiver completo
// Temporariamente comentado devido a dependências circulares complexas

use crate::error::{GuardianError, Result};
use cid::Cid;
use std::sync::Arc;

use crate::access_controller::manifest::CreateAccessControllerOptions;
use crate::iface::BaseGuardianDB;
use crate::access_controller::{traits::AccessController, traits::Option as AccessControllerOption};

/// equivalente a Create em go
/// Cria um novo controlador de acesso e retorna o CID do seu manifesto.
/// Temporariamente retorna erro devido a incompatibilidades de tipos complexas
pub async fn create(
    _db: Arc<dyn BaseGuardianDB<Error = Box<dyn std::error::Error + Send + Sync>>>,
    _controller_type: &str,
    _params: CreateAccessControllerOptions,
    _options: AccessControllerOption
) -> Result<Cid> {
    Err(GuardianError::Store("Utils create temporariamente não implementado - incompatibilidades de tipos".to_string()))
}


/// equivalente a Resolve em go
/// Resolve um controlador de acesso usando o endereço do seu manifesto.
/// Temporariamente retorna erro devido a incompatibilidades de tipos complexas
pub async fn resolve(
    _db: Arc<dyn BaseGuardianDB<Error = Box<dyn std::error::Error + Send + Sync>>>,
    _manifest_address: &str,
    _params: &CreateAccessControllerOptions,
    _options: AccessControllerOption
) -> Result<Arc<dyn AccessController>> {
    Err(GuardianError::Store("Utils resolve temporariamente não implementado - incompatibilidades de tipos".to_string()))
}

/// equivalente a EnsureAddress em go
/// Garante que um endereço de controlador de acesso termine com "/_access".
/// Se o sufixo não estiver presente, ele é adicionado.
pub fn ensure_address(address: &str) -> String {
    // A lógica em Go usa `strings.Split` e verifica a última parte.
    // `split('/').last()` em Rust emula esse comportamento.
    // Ex: "foo/bar/_access".split('/').last() -> Some("_access")
    // Ex: "foo/bar/_access/".split('/').last() -> Some("")
    if address.split('/').last() == Some("_access") {
        return address.to_string();
    }

    // Recria o comportamento de `path.Join(address, "/_access")`,
    // que lida com a presença ou ausência de uma barra no final.
    if address.ends_with('/') {
        format!("{}{}", address, "_access")
    } else {
        format!("{}/{}", address, "_access")
    }
}