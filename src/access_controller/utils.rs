use crate::access_controller::manifest::{CreateAccessControllerOptions, ManifestParams};
use crate::access_controller::{
    simple::SimpleAccessController, traits::AccessController,
    traits::Option as AccessControllerOption,
};
use crate::error::{GuardianError, Result};
use crate::iface::BaseGuardianDB;
use cid::Cid;
use slog::{debug, error, info, warn};
use std::sync::Arc;

/// Equivalente a Create em go
/// Cria um novo controlador de acesso e retorna o CID do seu manifesto.
///
/// # Argumentos
/// * `db` - Instância do BaseGuardianDB
/// * `controller_type` - Tipo do controlador ("simple", "guardian", "ipfs")
/// * `params` - Parâmetros de configuração do controlador
/// * `options` - Opções adicionais para criação
///
/// # Retorna
/// * `Ok(Cid)` - CID do manifesto criado
/// * `Err(GuardianError)` - Erro durante a criação
pub async fn create(
    db: Arc<dyn BaseGuardianDB<Error = GuardianError>>,
    controller_type: &str,
    params: CreateAccessControllerOptions,
    _options: AccessControllerOption,
) -> Result<Cid> {
    let logger = db.logger().clone();

    info!(logger, "Creating access controller";
        "type" => controller_type
    );

    // Validação do tipo de controlador
    let controller_type_normalized = controller_type.to_lowercase();
    match controller_type_normalized.as_str() {
        "simple" | "guardian" | "ipfs" => {}
        _ => {
            warn!(logger, "Unknown access controller type"; "type" => controller_type);
            return Err(GuardianError::Store(format!(
                "Unknown access controller type: {}",
                controller_type
            )));
        }
    }

    // Cria o controlador baseado no tipo
    let controller =
        create_controller(&controller_type_normalized, params.clone(), logger.clone()).await?;

    // Salva o controlador e obtém o manifesto
    let _manifest = controller.save().await?;

    // Garante que o endereço termine com "/_access"
    let access_address = ensure_address(&controller_type_normalized);

    debug!(logger, "Access controller created successfully";
        "type" => controller_type,
        "address" => &access_address
    );

    // Por enquanto, retorna um CID placeholder baseado no endereço
    // Em uma implementação completa, isso seria o CID real do manifesto no IPFS
    let cid_str = format!("Qm{:0>44}", access_address.len());
    match Cid::try_from(cid_str.as_str()) {
        Ok(cid) => {
            info!(logger, "Access controller manifest created";
                "cid" => %cid,
                "type" => controller_type
            );
            Ok(cid)
        }
        Err(e) => {
            error!(logger, "Failed to create CID for manifest"; "error" => %e);
            Err(GuardianError::Store(format!(
                "Failed to create manifest CID: {}",
                e
            )))
        }
    }
}

/// Equivalente a Resolve em go
/// Resolve um controlador de acesso usando o endereço do seu manifesto.
///
/// # Argumentos
/// * `db` - Instância do BaseGuardianDB
/// * `manifest_address` - Endereço do manifesto do controlador
/// * `params` - Parâmetros de configuração
/// * `options` - Opções adicionais para resolução
///
/// # Retorna
/// * `Ok(Arc<dyn AccessController>)` - Controlador de acesso resolvido
/// * `Err(GuardianError)` - Erro durante a resolução
pub async fn resolve(
    db: Arc<dyn BaseGuardianDB<Error = GuardianError>>,
    manifest_address: &str,
    params: &CreateAccessControllerOptions,
    _options: AccessControllerOption,
) -> Result<Arc<dyn AccessController>> {
    let logger = db.logger().clone();

    info!(logger, "Resolving access controller";
        "manifest_address" => manifest_address
    );

    // Garante que o endereço termine com "/_access"
    let access_address = ensure_address(manifest_address);

    // Valida o endereço
    if access_address.is_empty() {
        return Err(GuardianError::Store(
            "Manifest address cannot be empty".to_string(),
        ));
    }

    debug!(logger, "Loading access controller manifest";
        "address" => &access_address
    );

    // Por enquanto, vamos inferir o tipo do controlador baseado no endereço/parâmetros
    // Em uma implementação completa, isso seria carregado do manifesto no IPFS
    let controller_type = infer_controller_type(&access_address, params);

    debug!(logger, "Inferred controller type";
        "type" => &controller_type,
        "address" => &access_address
    );

    // Cria o controlador baseado no tipo inferido
    let controller = create_controller(&controller_type, params.clone(), logger.clone()).await?;

    // Carrega o estado do controlador usando o endereço
    if let Err(e) = controller.load(&access_address).await {
        warn!(logger, "Failed to load controller state, using defaults";
            "error" => %e,
            "address" => &access_address
        );
    }

    info!(logger, "Access controller resolved successfully";
        "type" => &controller_type,
        "address" => &access_address
    );

    Ok(controller)
}

/// Equivalente a EnsureAddress em go
/// Garante que um endereço de controlador de acesso termine com "/_access".
/// Se o sufixo não estiver presente, ele é adicionado.
///
/// # Argumentos
/// * `address` - Endereço a ser validado/corrigido
///
/// # Retorna
/// * `String` - Endereço com sufixo "/_access" garantido
///
/// # Exemplos
/// ```
/// use guardian_db::access_controller::utils::ensure_address;
///
/// assert_eq!(ensure_address("foo/bar"), "foo/bar/_access");
/// assert_eq!(ensure_address("foo/bar/_access"), "foo/bar/_access");
/// assert_eq!(ensure_address("foo/bar/"), "foo/bar/_access");
/// assert_eq!(ensure_address(""), "_access");
/// ```
pub fn ensure_address(address: &str) -> String {
    // Remove espaços em branco das extremidades
    let address = address.trim();

    // Se o endereço está vazio, retorna apenas "_access"
    if address.is_empty() {
        return "_access".to_string();
    }

    // A lógica em Go usa `strings.Split` e verifica a última parte.
    // `split('/').next_back()` é mais eficiente que last() para DoubleEndedIterator.
    // Ex: "foo/bar/_access".split('/').next_back() -> Some("_access")
    // Ex: "foo/bar/_access/".split('/').next_back() -> Some("")
    if address.split('/').next_back() == Some("_access") {
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

/// Função auxiliar para criar um controlador baseado no tipo
///
/// # Argumentos
/// * `controller_type` - Tipo do controlador ("simple", "guardian", "ipfs")
/// * `params` - Parâmetros de configuração
/// * `logger` - Logger para registro de eventos
///
/// # Retorna
/// * `Ok(Arc<dyn AccessController>)` - Controlador criado
/// * `Err(GuardianError)` - Erro durante a criação
async fn create_controller(
    controller_type: &str,
    params: CreateAccessControllerOptions,
    logger: slog::Logger,
) -> Result<Arc<dyn AccessController>> {
    debug!(logger, "Creating access controller instance"; "type" => controller_type);

    match controller_type {
        "simple" => {
            let initial_keys = if params.get_all_access().is_empty() {
                // Se não há permissões definidas, cria permissões padrão
                let mut default_permissions = std::collections::HashMap::new();
                default_permissions.insert("write".to_string(), vec!["*".to_string()]);
                Some(default_permissions)
            } else {
                Some(params.get_all_access())
            };

            let controller = SimpleAccessController::new(logger.clone(), initial_keys);
            Ok(Arc::new(controller) as Arc<dyn AccessController>)
        }
        "guardian" | "ipfs" => {
            // Por enquanto, vamos criar um SimpleAccessController como fallback
            // até que os outros tipos estejam completamente implementados
            warn!(logger, "Controller type not fully implemented, using simple fallback";
                "requested_type" => controller_type
            );

            let initial_keys = if params.get_all_access().is_empty() {
                let mut default_permissions = std::collections::HashMap::new();
                default_permissions.insert("write".to_string(), vec!["*".to_string()]);
                Some(default_permissions)
            } else {
                Some(params.get_all_access())
            };

            let controller = SimpleAccessController::new(logger.clone(), initial_keys);
            Ok(Arc::new(controller) as Arc<dyn AccessController>)
        }
        _ => {
            error!(logger, "Unsupported access controller type"; "type" => controller_type);
            Err(GuardianError::Store(format!(
                "Unsupported access controller type: {}",
                controller_type
            )))
        }
    }
}

/// Função auxiliar para inferir o tipo de controlador baseado no endereço/parâmetros
///
/// # Argumentos
/// * `address` - Endereço do manifesto
/// * `params` - Parâmetros de configuração
///
/// # Retorna
/// * `String` - Tipo do controlador inferido
fn infer_controller_type(address: &str, params: &CreateAccessControllerOptions) -> String {
    // Verifica se há um tipo explícito nos parâmetros
    let explicit_type = params.get_type();
    if !explicit_type.is_empty() {
        return explicit_type.to_string();
    }

    // Infere baseado no endereço
    if address.contains("/guardian/") || address.contains("guardian_") {
        return "guardian".to_string();
    }

    if address.contains("/ipfs/") || address.contains("ipfs_") {
        return "ipfs".to_string();
    }

    // Padrão para SimpleAccessController
    "simple".to_string()
}

/// Valida um endereço de controlador de acesso
///
/// # Argumentos
/// * `address` - Endereço a ser validado
///
/// # Retorna
/// * `Ok(())` - Endereço válido
/// * `Err(GuardianError)` - Endereço inválido
pub fn validate_address(address: &str) -> Result<()> {
    if address.trim().is_empty() {
        return Err(GuardianError::Store("Address cannot be empty".to_string()));
    }

    // Verifica caracteres inválidos
    if address.contains("..") || address.contains("//") {
        return Err(GuardianError::Store(
            "Address contains invalid path components".to_string(),
        ));
    }

    // Verifica comprimento máximo
    if address.len() > 1000 {
        return Err(GuardianError::Store(
            "Address is too long (max 1000 characters)".to_string(),
        ));
    }

    Ok(())
}

/// Lista os tipos de controladores de acesso disponíveis
///
/// # Retorna
/// * `Vec<String>` - Lista dos tipos disponíveis
pub fn list_available_types() -> Vec<String> {
    vec![
        "simple".to_string(),
        "guardian".to_string(),
        "ipfs".to_string(),
    ]
}

/// Verifica se um tipo de controlador é suportado
///
/// # Argumentos
/// * `controller_type` - Tipo a ser verificado
///
/// # Retorna
/// * `bool` - true se suportado, false caso contrário
pub fn is_supported_type(controller_type: &str) -> bool {
    list_available_types().contains(&controller_type.to_lowercase())
}
