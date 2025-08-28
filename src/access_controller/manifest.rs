use crate::error::{GuardianError, Result};
use cid::Cid;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use ipfs_api_backend_hyper::IpfsClient;
use slog::Logger;
use crate::access_controller::traits::AccessController;

/// equivalente a Manifest em go
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Manifest {
    /// O tipo do controlador de acesso (ex: "ipfs", "GuardianDB").
    #[serde(rename = "type")]
    pub r#type: String,

    /// Os parâmetros de configuração para este controlador de acesso.
    #[serde(rename = "params")]
    pub params: CreateAccessControllerOptions,
}

/// equivalente a CreateAccessControllerOptions em go
/// Contém as opções de configuração para um controlador de acesso.
#[derive(Debug, Clone)]
pub struct CreateAccessControllerOptions {
    pub skip_manifest: bool,
    pub address: Cid,
    pub r#type: String,
    pub name: String,
    
    // O campo `Access` não é renomeado na versão Go.
    // Usamos um Mutex para acesso concorrente seguro, como o `sync.RWMutex` do Go.
    access: Arc<Mutex<HashMap<String, Vec<String>>>>,
}

// Implementação manual de Serialize para sincronizar dados antes da serialização
impl Serialize for CreateAccessControllerOptions {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        
        let mut state = serializer.serialize_struct("CreateAccessControllerOptions", 5)?;
        state.serialize_field("skip_manifest", &self.skip_manifest)?;
        // Note: CID serialization may need custom implementation
        state.serialize_field("type", &self.r#type)?;
        state.serialize_field("name", &self.name)?;
        
        // Serializa os dados de acesso diretamente do Mutex
        if let Ok(access_guard) = self.access.lock() {
            if !access_guard.is_empty() {
                state.serialize_field("access", &*access_guard)?;
            }
        }
        
        state.end()
    }
}

// Implementação manual de Deserialize para sincronizar dados após a deserialização
impl<'de> Deserialize<'de> for CreateAccessControllerOptions {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{self, MapAccess, Visitor};
        use std::fmt;

        struct OptionsVisitor;

        impl<'de> Visitor<'de> for OptionsVisitor {
            type Value = CreateAccessControllerOptions;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("struct CreateAccessControllerOptions")
            }

            fn visit_map<V>(self, mut map: V) -> std::result::Result<CreateAccessControllerOptions, V::Error>
            where
                V: MapAccess<'de>,
            {
                let mut skip_manifest = false;
                let mut address = Cid::default();
                let mut type_field = String::new();
                let mut name = String::new();
                let mut access = HashMap::new();

                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "skip_manifest" => skip_manifest = map.next_value()?,
                        "address" => {
                            // Note: Custom CID deserialization may be needed
                            // For now, use default
                        }
                        "type" => type_field = map.next_value()?,
                        "name" => name = map.next_value()?,
                        "access" => access = map.next_value()?,
                        _ => { let _ = map.next_value::<serde::de::IgnoredAny>()?; }
                    }
                }

                Ok(CreateAccessControllerOptions {
                    skip_manifest,
                    address,
                    r#type: type_field,
                    name,
                    access: Arc::new(Mutex::new(access)),
                })
            }
        }

        deserializer.deserialize_struct(
            "CreateAccessControllerOptions",
            &["skip_manifest", "address", "type", "name", "access"],
            OptionsVisitor,
        )
    }
}

impl Default for CreateAccessControllerOptions {
    fn default() -> Self {
        Self {
            skip_manifest: false,
            address: Cid::default(),
            r#type: String::new(),
            name: String::new(),
            access: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

/// equivalente a ManifestParams em go
/// Uma trait que define a interface para os parâmetros de um manifesto.
pub trait ManifestParams: Send + Sync {
    fn skip_manifest(&self) -> bool;
    fn address(&self) -> Cid;
    fn set_address(&mut self, addr: Cid);
    fn get_type(&self) -> &str;
    fn set_type(&mut self, t: String);
    fn get_name(&self) -> &str;
    fn set_name(&mut self, name: String);
    
    // Métodos síncronos para tornar a trait dyn-compatible
    fn set_access(&mut self, role: String, allowed: Vec<String>);
    fn get_access(&self, role: &str) -> Option<Vec<String>>;
    fn get_all_access(&self) -> HashMap<String, Vec<String>>;
}

impl ManifestParams for CreateAccessControllerOptions {
    fn skip_manifest(&self) -> bool { self.skip_manifest }
    fn address(&self) -> Cid { self.address }
    fn set_address(&mut self, addr: Cid) { self.address = addr; }
    fn get_type(&self) -> &str { &self.r#type }
    fn set_type(&mut self, t: String) { self.r#type = t; }
    fn get_name(&self) -> &str { &self.name }
    fn set_name(&mut self, name: String) { self.name = name; }
    
    fn set_access(&mut self, role: String, allowed: Vec<String>) {
        let mut guard = self.access.lock().expect("Failed to acquire lock");
        guard.insert(role, allowed);
    }
    
    fn get_access(&self, role: &str) -> Option<Vec<String>> {
        let guard = self.access.lock().expect("Failed to acquire lock");
        guard.get(role).cloned()
    }

    fn get_all_access(&self) -> HashMap<String, Vec<String>> {
        let guard = self.access.lock().expect("Failed to acquire lock");
        guard.clone()
    }
}

impl CreateAccessControllerOptions {
    /// equivalente a NewManifestParams em go
    pub fn new(address: Cid, skip_manifest: bool, manifest_type: String) -> Self {
        Self {
            address,
            skip_manifest,
            r#type: manifest_type,
            ..Default::default()
        }
    }

    /// equivalente a NewEmptyManifestParams em go
    pub fn new_empty() -> Self {
        Default::default()
    }

    /// equivalente a NewSimpleManifestParams em go
    pub fn new_simple(manifest_type: String, access: HashMap<String, Vec<String>>) -> Self {
        Self {
            skip_manifest: true,
            r#type: manifest_type,
            access: Arc::new(Mutex::new(access)),
            ..Default::default()
        }
    }
    
    /// equivalente a CloneManifestParams em go
    pub fn from_params(params: &CreateAccessControllerOptions) -> Self {
        params.clone()
    }
}

/// equivalente a CreateManifest em go
/// Cria um novo manifesto e retorna seu CID.
pub async fn create(
    ipfs: Arc<IpfsClient>,
    controller_type: String,
    params: &CreateAccessControllerOptions,
) -> Result<Cid> {
    if params.skip_manifest() {
        return Ok(params.address());
    }

    // Valida que o controller_type não está vazio
    if controller_type.is_empty() {
        return Err(GuardianError::Store("Controller type cannot be empty".to_string()));
    }

    let manifest = Manifest {
        r#type: controller_type,
        params: CreateAccessControllerOptions {
            skip_manifest: params.skip_manifest(),
            address: params.address(),
            r#type: params.get_type().to_string(),
            name: params.get_name().to_string(),
            access: params.access.clone(),
        },
    };

    // Serializa o manifesto em CBOR
    let cbor_data = serde_cbor::to_vec(&manifest)
        .map_err(|e| GuardianError::Store(format!("Failed to serialize manifest: {}", e)))?;

    // Valida que os dados serializados não estão vazios
    if cbor_data.is_empty() {
        return Err(GuardianError::Store("Serialized manifest is empty".to_string()));
    }

    // Armazena no IPFS usando o cliente
    use ipfs_api_backend_hyper::IpfsApi;
    let response = ipfs.add(std::io::Cursor::new(cbor_data)).await
        .map_err(|e| GuardianError::Store(format!("Failed to store manifest in IPFS: {}", e)))?;

    // Valida que o IPFS retornou um hash válido
    if response.hash.is_empty() {
        return Err(GuardianError::Store("IPFS returned empty hash".to_string()));
    }

    // Converte o hash retornado para CID
    let cid = Cid::try_from(response.hash.as_str())
        .map_err(|e| GuardianError::Store(format!("Invalid CID returned from IPFS: {}", e)))?;

    Ok(cid)
}

/// equivalente a ResolveManifest em go
/// Recupera um manifesto a partir do seu endereço.
pub async fn resolve(
    ipfs: Arc<IpfsClient>,
    manifest_address: &str,
    params: &CreateAccessControllerOptions,
) -> Result<Manifest> {
    if params.skip_manifest() {
        if params.get_type().is_empty() {
            return Err(GuardianError::Store("Sem manifesto, o tipo do controlador de acesso é obrigatório".to_string()));
        }

        return Ok(Manifest {
            r#type: params.get_type().to_string(),
            params: params.clone(),
        });
    }

    // Valida que o endereço não está vazio
    if manifest_address.is_empty() {
        return Err(GuardianError::Store("Manifest address cannot be empty".to_string()));
    }

    // Remove o prefixo /ipfs/ se existir
    let hash = if let Some(stripped) = manifest_address.strip_prefix("/ipfs/") {
        stripped
    } else {
        manifest_address
    };

    let cid = Cid::try_from(hash)
        .map_err(|e| GuardianError::Store(format!("Invalid CID format: {}", e)))?;

    // Busca os dados do manifesto no IPFS
    use ipfs_api_backend_hyper::IpfsApi;
    use futures::TryStreamExt;
    
    let data_stream = ipfs.cat(&cid.to_string());
    let data_bytes: Vec<u8> = data_stream
        .try_collect::<Vec<_>>().await
        .map_err(|e| GuardianError::Store(format!("Failed to retrieve manifest from IPFS: {}", e)))?
        .into_iter()
        .flatten()
        .collect();

    // Valida que os dados não estão vazios
    if data_bytes.is_empty() {
        return Err(GuardianError::Store("Retrieved manifest data is empty".to_string()));
    }

    // Deserializa o manifesto a partir dos dados CBOR
    let manifest: Manifest = serde_cbor::from_slice(&data_bytes)
        .map_err(|e| GuardianError::Store(format!("Failed to deserialize manifest: {}", e)))?;

    // Validação adicional do manifesto
    if manifest.r#type.is_empty() {
        return Err(GuardianError::Store("Manifest type cannot be empty".to_string()));
    }

    Ok(manifest)
}

/// Função utilitária para criar e validar um manifesto
pub fn create_manifest_with_validation(
    controller_type: String,
    params: CreateAccessControllerOptions,
) -> Result<Manifest> {
    // Validações básicas
    if controller_type.is_empty() {
        return Err(GuardianError::Store("Controller type cannot be empty".to_string()));
    }

    if controller_type.len() > 255 {
        return Err(GuardianError::Store("Controller type too long (max 255 characters)".to_string()));
    }

    // Valida que o tipo é um dos tipos conhecidos
    let valid_types = ["ipfs", "GuardianDB", "simple"];
    if !valid_types.contains(&controller_type.as_str()) {
        return Err(GuardianError::Store(format!("Unknown controller type: {}", controller_type)));
    }

    Ok(Manifest {
        r#type: controller_type,
        params,
    })
}

// equivalente a WithLogger em go
// Função de alta ordem para configurar um logger (padrão de opção funcional).
// Retorna uma closure que pode ser aplicada a qualquer AccessController
pub fn with_logger(logger: Arc<slog::Logger>) -> Box<dyn Fn(&dyn AccessController) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> + Send + Sync> {
    Box::new(move |ac: &dyn AccessController| {
        let logger_clone = logger.clone();
        Box::pin(async move {
            ac.set_logger(logger_clone).await;
        })
    })
}

// Versão alternativa mais simples para uso direto
pub async fn apply_logger_to_controller(
    controller: &dyn AccessController, 
    logger: Arc<slog::Logger>
) -> Result<()> {
    controller.set_logger(logger).await;
    Ok(())
}

// O bloco `init()` do Go, que registra os tipos para serialização CBOR,
// é substituído em Rust pelo uso de atributos da crate `serde`, como `#[serde(rename = "...")]`,
// que são declarativos e aplicados diretamente nas definições das structs.