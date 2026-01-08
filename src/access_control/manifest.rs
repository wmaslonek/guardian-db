use crate::guardian::error::{GuardianError, Result};
use crate::p2p::network::client::IrohClient;
use iroh_blobs::Hash;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Manifest {
    /// O tipo do controlador de acesso (ex: "iroh", "GuardianDB").
    #[serde(rename = "type")]
    pub get_type: String,

    /// Os parâmetros de configuração para este controlador de acesso.
    #[serde(rename = "params")]
    pub params: CreateAccessControllerOptions,
}

/// Contém as opções de configuração para um controlador de acesso.
#[derive(Debug, Clone)]
pub struct CreateAccessControllerOptions {
    pub skip_manifest: bool,
    pub address: Hash,
    pub get_type: String,
    pub name: String,
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
        state.serialize_field("address", &hex::encode(self.address.as_bytes()))?; // Hash como hex
        state.serialize_field("type", &self.get_type)?;
        state.serialize_field("name", &self.name)?;
        // Serializa os dados de acesso diretamente do Mutex
        if let Ok(access_guard) = self.access.lock()
            && !access_guard.is_empty()
        {
            state.serialize_field("access", &*access_guard)?;
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
        use serde::de::{MapAccess, Visitor};
        use std::fmt;
        struct OptionsVisitor;
        impl<'de> Visitor<'de> for OptionsVisitor {
            type Value = CreateAccessControllerOptions;
            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("struct CreateAccessControllerOptions")
            }
            fn visit_map<V>(
                self,
                mut map: V,
            ) -> std::result::Result<CreateAccessControllerOptions, V::Error>
            where
                V: MapAccess<'de>,
            {
                let mut skip_manifest = false;
                let mut address = Hash::from([0u8; 32]);
                let mut type_field = String::new();
                let mut name = String::new();
                let mut access = HashMap::new();

                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "skip_manifest" => skip_manifest = map.next_value()?,
                        "address" => {
                            let hex_str: String = map.next_value()?;
                            address = hex::decode(&hex_str)
                                .ok()
                                .and_then(|bytes| {
                                    if bytes.len() == 32 {
                                        let mut arr = [0u8; 32];
                                        arr.copy_from_slice(&bytes);
                                        Some(Hash::from(arr))
                                    } else {
                                        None
                                    }
                                })
                                .unwrap_or_else(|| Hash::from([0u8; 32]));
                        }
                        "type" => type_field = map.next_value()?,
                        "name" => name = map.next_value()?,
                        "access" => access = map.next_value()?,
                        _ => {
                            let _ = map.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }
                Ok(CreateAccessControllerOptions {
                    skip_manifest,
                    address,
                    get_type: type_field,
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
            address: Hash::from([0u8; 32]),
            get_type: String::new(),
            name: String::new(),
            access: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

/// Define a interface para os parâmetros de um manifesto.
pub trait ManifestParams: Send + Sync {
    fn skip_manifest(&self) -> bool;
    fn address(&self) -> Hash;
    fn set_address(&mut self, addr: Hash);
    fn get_type(&self) -> &str;
    fn set_type(&mut self, t: String);
    fn get_name(&self) -> &str;
    fn set_name(&mut self, name: String);
    fn set_access(&mut self, role: String, allowed: Vec<String>);
    fn get_access(&self, role: &str) -> Option<Vec<String>>;
    fn get_all_access(&self) -> HashMap<String, Vec<String>>;

    /// Permite downcast seguro para implementações concretas
    fn as_any(&self) -> &dyn std::any::Any;
}

impl ManifestParams for CreateAccessControllerOptions {
    fn skip_manifest(&self) -> bool {
        self.skip_manifest
    }
    fn address(&self) -> Hash {
        self.address
    }
    fn set_address(&mut self, addr: Hash) {
        self.address = addr;
    }
    fn get_type(&self) -> &str {
        &self.get_type
    }
    fn set_type(&mut self, t: String) {
        self.get_type = t;
    }
    fn get_name(&self) -> &str {
        &self.name
    }
    fn set_name(&mut self, name: String) {
        self.name = name;
    }

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

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl CreateAccessControllerOptions {
    pub fn new(address: Hash, skip_manifest: bool, manifest_type: String) -> Self {
        Self {
            address,
            skip_manifest,
            get_type: manifest_type,
            ..Default::default()
        }
    }

    pub fn new_empty() -> Self {
        Default::default()
    }

    pub fn new_simple(manifest_type: String, access: HashMap<String, Vec<String>>) -> Self {
        Self {
            skip_manifest: true,
            get_type: manifest_type,
            access: Arc::new(Mutex::new(access)),
            ..Default::default()
        }
    }

    pub fn from_params(params: &CreateAccessControllerOptions) -> Self {
        params.clone()
    }
}

/// Cria um novo manifesto e retorna seu Hash.
pub async fn create(
    client: Arc<IrohClient>,
    controller_type: String,
    params: &CreateAccessControllerOptions,
) -> Result<Hash> {
    if params.skip_manifest() {
        return Ok(params.address());
    }

    // Valida que o controller_type não está vazio
    if controller_type.is_empty() {
        return Err(GuardianError::Store(
            "Controller type cannot be empty".to_string(),
        ));
    }

    let manifest = Manifest {
        get_type: controller_type,
        params: CreateAccessControllerOptions {
            skip_manifest: params.skip_manifest(),
            address: params.address(),
            get_type: params.get_type().to_string(),
            name: params.get_name().to_string(),
            access: params.access.clone(),
        },
    };

    // Serializa o manifesto em CBOR
    let cbor_data = serde_cbor::to_vec(&manifest)
        .map_err(|e| GuardianError::Store(format!("Failed to serialize manifest: {}", e)))?;

    // Valida que os dados serializados não estão vazios
    if cbor_data.is_empty() {
        return Err(GuardianError::Store(
            "Serialized manifest is empty".to_string(),
        ));
    }

    // Armazena no iroh usando o cliente nativo
    let response = client
        .add_bytes(cbor_data)
        .await
        .map_err(|e| GuardianError::Store(format!("Failed to store manifest in iroh: {}", e)))?;

    // Valida que o iroh retornou um hash válido
    if response.hash.is_empty() {
        return Err(GuardianError::Store("iroh returned empty hash".to_string()));
    }

    // Converte o hash hex string para Hash
    let hash_bytes = hex::decode(&response.hash)
        .map_err(|e| GuardianError::Store(format!("Invalid hex hash returned from iroh: {}", e)))?;

    if hash_bytes.len() != 32 {
        return Err(GuardianError::Store(format!(
            "Invalid hash length: expected 32 bytes, got {}",
            hash_bytes.len()
        )));
    }

    let mut hash_array = [0u8; 32];
    hash_array.copy_from_slice(&hash_bytes);
    let hash = Hash::from(hash_array);

    Ok(hash)
}

/// Recupera um manifesto a partir do seu endereço.
pub async fn resolve(
    client: Arc<IrohClient>,
    manifest_address: &str,
    params: &CreateAccessControllerOptions,
) -> Result<Manifest> {
    if params.skip_manifest() {
        if params.get_type().is_empty() {
            return Err(GuardianError::Store(
                "Sem manifesto, o tipo do controlador de acesso é obrigatório".to_string(),
            ));
        }

        return Ok(Manifest {
            get_type: params.get_type().to_string(),
            params: params.clone(),
        });
    }

    // Valida que o endereço não está vazio
    if manifest_address.is_empty() {
        return Err(GuardianError::Store(
            "Manifest address cannot be empty".to_string(),
        ));
    }

    // Remove o prefixo /iroh/ se existir
    let hash_str = if let Some(stripped) = manifest_address.strip_prefix("/iroh/") {
        stripped
    } else {
        manifest_address
    };

    // Busca os dados do manifesto no iroh usando hash hex
    let data_bytes = client
        .cat_bytes(hash_str)
        .await
        .map_err(|e| GuardianError::Store(format!("Failed to load manifest from Iroh: {}", e)))?;

    // Valida que os dados não estão vazios
    if data_bytes.is_empty() {
        return Err(GuardianError::Store(
            "Retrieved manifest data is empty".to_string(),
        ));
    }

    // Deserializa o manifesto a partir dos dados CBOR
    let manifest: Manifest = serde_cbor::from_slice(&data_bytes)
        .map_err(|e| GuardianError::Store(format!("Failed to deserialize manifest: {}", e)))?;

    // Validação adicional do manifesto
    if manifest.get_type.is_empty() {
        return Err(GuardianError::Store(
            "Manifest type cannot be empty".to_string(),
        ));
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
        return Err(GuardianError::Store(
            "Controller type cannot be empty".to_string(),
        ));
    }

    if controller_type.len() > 255 {
        return Err(GuardianError::Store(
            "Controller type too long (max 255 characters)".to_string(),
        ));
    }

    // Valida que o tipo é um dos tipos conhecidos
    let valid_types = ["iroh", "GuardianDB", "simple"];
    if !valid_types.contains(&controller_type.as_str()) {
        return Err(GuardianError::Store(format!(
            "Unknown controller type: {}",
            controller_type
        )));
    }

    Ok(Manifest {
        get_type: controller_type,
        params,
    })
}
