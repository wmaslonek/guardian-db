use crate::access_controller::manifest::{CreateAccessControllerOptions, ManifestParams};
use crate::access_controller::{
    simple::SimpleAccessController, traits::AccessController,
    traits::Option as AccessControllerOption,
};
use crate::error::{GuardianError, Result};
use crate::iface::BaseGuardianDB;
use cid::Cid;
use std::sync::Arc;
use tracing::{debug, error, info, instrument, warn};

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
#[instrument(skip(db, params, _options), fields(controller_type = %controller_type))]
pub async fn create(
    db: Arc<dyn BaseGuardianDB<Error = GuardianError>>,
    controller_type: &str,
    params: CreateAccessControllerOptions,
    _options: AccessControllerOption,
) -> Result<Cid> {
    info!(target: "access_controller_utils", controller_type = %controller_type, "Creating access controller");

    // Validação do tipo de controlador
    let controller_type_normalized = controller_type.to_lowercase();
    match controller_type_normalized.as_str() {
        "simple" | "guardian" | "ipfs" => {}
        _ => {
            warn!(target: "access_controller_utils", controller_type = %controller_type, "Unknown access controller type");
            return Err(GuardianError::Store(format!(
                "Unknown access controller type: {}",
                controller_type
            )));
        }
    }

    // Cria o controlador baseado no tipo
    let controller = create_controller(&controller_type_normalized, params.clone()).await?;

    // Salva o controlador e obtém o manifesto
    let _manifest_params = controller.save().await?;

    // Garante que o endereço termine com "/_access"
    let access_address = ensure_address(&controller_type_normalized);

    debug!(target: "access_controller_utils",
        controller_type = %controller_type,
        address = %access_address,
        "Access controller created successfully"
    );

    // Cria manifesto real no IPFS
    let ipfs_client = db.ipfs();

    // Cria o manifesto
    let manifest_cid = crate::access_controller::manifest::create(
        ipfs_client,
        controller_type_normalized,
        &params,
    )
    .await
    .map_err(|e| {
        error!(target: "access_controller_utils", error = %e, "Failed to create manifest in IPFS");
        GuardianError::Store(format!(
            "Failed to create access controller manifest: {}",
            e
        ))
    })?;

    info!(target: "access_controller_utils",
        cid = %manifest_cid,
        controller_type = %controller_type,
        address = %access_address,
        "Access controller manifest created in IPFS"
    );

    Ok(manifest_cid)
}

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
#[instrument(skip(db, params, _options), fields(manifest_address = %manifest_address))]
pub async fn resolve(
    db: Arc<dyn BaseGuardianDB<Error = GuardianError>>,
    manifest_address: &str,
    params: &CreateAccessControllerOptions,
    _options: AccessControllerOption,
) -> Result<Arc<dyn AccessController>> {
    info!(target: "access_controller_utils", manifest_address = %manifest_address, "Resolving access controller");

    // Garante que o endereço termine com "/_access"
    let access_address = ensure_address(manifest_address);

    // Valida o endereço
    if access_address.is_empty() {
        return Err(GuardianError::Store(
            "Manifest address cannot be empty".to_string(),
        ));
    }

    debug!(target: "access_controller_utils", address = %access_address, "Loading access controller manifest");

    // Carrega manifesto real do IPFS
    let ipfs_client = db.ipfs();

    // Tenta carregar o manifesto do IPFS
    let manifest_result =
        crate::access_controller::manifest::resolve(ipfs_client, &access_address, params).await;

    let controller_type = match manifest_result {
        Ok(manifest) => {
            debug!(target: "access_controller_utils",
                controller_type = %manifest.get_type,
                address = %access_address,
                "Loaded controller type from IPFS manifest"
            );
            manifest.get_type
        }
        Err(e) => {
            warn!(target: "access_controller_utils",
                error = %e,
                address = %access_address,
                "Failed to load manifest from IPFS, falling back to inference"
            );
            // Fallback: infere o tipo como antes se não conseguir carregar do IPFS
            infer_controller_type(&access_address, params)
        }
    };

    debug!(target: "access_controller_utils",
        controller_type = %controller_type,
        address = %access_address,
        "Controller type determined"
    );

    // Cria o controlador baseado no tipo real ou inferido
    let controller = create_controller(&controller_type, params.clone()).await?;

    // Carrega o estado do controlador usando o endereço
    if let Err(e) = controller.load(&access_address).await {
        warn!(target: "access_controller_utils",
            error = %e,
            address = %access_address,
            "Failed to load controller state, using defaults"
        );
    }

    info!(target: "access_controller_utils",
        controller_type = %controller_type,
        address = %access_address,
        "Access controller resolved successfully"
    );

    Ok(controller)
}

/// Garante que um endereço de controlador de acesso termine com "/_access".
/// Se o sufixo não estiver presente, ele é adicionado.
///
/// # Argumentos
/// * `address` - Endereço a ser validado/corrigido
///
/// # Retorna
/// * `String` - Endereço com sufixo "/_access" garantido
pub fn ensure_address(address: &str) -> String {
    // Remove espaços em branco das extremidades
    let address = address.trim();
    // Se o endereço está vazio, retorna apenas "_access"
    if address.is_empty() {
        return "_access".to_string();
    }
    // Verifica a última parte.
    // `split('/').next_back()` é mais eficiente que last() para DoubleEndedIterator.
    // Ex: "foo/bar/_access".split('/').next_back() -> Some("_access")
    // Ex: "foo/bar/_access/".split('/').next_back() -> Some("")
    if address.split('/').next_back() == Some("_access") {
        return address.to_string();
    }
    // Lida com a presença ou ausência de uma barra no final.
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
///
/// # Retorna
/// * `Ok(Arc<dyn AccessController>)` - Controlador criado
/// * `Err(GuardianError)` - Erro durante a criação
#[instrument(skip(params))]
async fn create_controller(
    controller_type: &str,
    params: CreateAccessControllerOptions,
) -> Result<Arc<dyn AccessController>> {
    debug!(target: "access_controller_utils", controller_type = %controller_type, "Creating access controller instance");

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
            let controller = SimpleAccessController::new(initial_keys);
            Ok(Arc::new(controller) as Arc<dyn AccessController>)
        }
        "ipfs" => {
            debug!(target: "access_controller_utils", "Creating IpfsAccessController");
            // Implementação com fallback temporário
            // ***TODO: Implementar IpfsAccessController real quando IPFS client estiver disponível neste contexto
            warn!(target: "access_controller_utils",
                requested_type = %controller_type,
                "IpfsAccessController requires IPFS client, using SimpleAccessController with ipfs semantics"
            );

            let initial_keys = if params.get_all_access().is_empty() {
                let mut default_permissions = std::collections::HashMap::new();
                default_permissions.insert("write".to_string(), vec!["*".to_string()]);
                default_permissions.insert("admin".to_string(), vec!["*".to_string()]);
                Some(default_permissions)
            } else {
                Some(params.get_all_access())
            };
            let controller = SimpleAccessController::new(initial_keys);
            Ok(Arc::new(controller) as Arc<dyn AccessController>)
        }
        "guardian" => {
            debug!(target: "access_controller_utils", "Creating GuardianDBAccessController");
            // Implementação com fallback temporário
            // ***TODO: Implementar GuardianDBAccessController real quando trait bounds estiverem resolvidos
            warn!(target: "access_controller_utils",
                requested_type = %controller_type,
                "GuardianDBAccessController requires complex trait bounds, using SimpleAccessController with guardian semantics"
            );

            let initial_keys = if params.get_all_access().is_empty() {
                let mut default_permissions = std::collections::HashMap::new();
                default_permissions.insert("write".to_string(), vec!["*".to_string()]);
                default_permissions.insert("admin".to_string(), vec!["*".to_string()]);
                Some(default_permissions)
            } else {
                Some(params.get_all_access())
            };
            let controller = SimpleAccessController::new(initial_keys);
            Ok(Arc::new(controller) as Arc<dyn AccessController>)
        }
        _ => {
            error!(target: "access_controller_utils", controller_type = %controller_type, "Unsupported access controller type");
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
