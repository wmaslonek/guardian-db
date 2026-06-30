use crate::guardian::error::{GuardianError, Result};
use crate::p2p::network::client::IrohClient;
use iroh_blobs::Hash;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A serializable manifest describing an access controller, persisted to Iroh.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Manifest {
    /// The access controller type (e.g. "iroh", "GuardianDB").
    #[serde(rename = "type")]
    pub get_type: String,

    /// The configuration parameters for this access controller.
    #[serde(rename = "params")]
    pub params: CreateAccessControllerOptions,
}

/// Holds the configuration options for an access controller.
#[derive(Debug, Clone)]
pub struct CreateAccessControllerOptions {
    pub skip_manifest: bool,
    pub address: Hash,
    pub get_type: String,
    pub name: String,
    access: Arc<Mutex<HashMap<String, Vec<String>>>>,
}

// Manual Serialize implementation to sync the data before serialization.
impl Serialize for CreateAccessControllerOptions {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("CreateAccessControllerOptions", 5)?;
        state.serialize_field("skip_manifest", &self.skip_manifest)?;
        state.serialize_field("address", &hex::encode(self.address.as_bytes()))?; // Hash as hex.
        state.serialize_field("type", &self.get_type)?;
        state.serialize_field("name", &self.name)?;
        // Serialize the access data directly from the Mutex.
        if let Ok(access_guard) = self.access.lock()
            && !access_guard.is_empty()
        {
            state.serialize_field("access", &*access_guard)?;
        }
        state.end()
    }
}

// Manual Deserialize implementation to sync the data after deserialization.
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

/// Defines the interface for a manifest's parameters.
pub trait ManifestParams: Send + Sync {
    /// Whether manifest creation/resolution should be skipped.
    fn skip_manifest(&self) -> bool;
    /// The address (hash) the manifest points to.
    fn address(&self) -> Hash;
    /// Sets the address (hash).
    fn set_address(&mut self, addr: Hash);
    /// The controller type.
    fn get_type(&self) -> &str;
    /// Sets the controller type.
    fn set_type(&mut self, t: String);
    /// The manifest name.
    fn get_name(&self) -> &str;
    /// Sets the manifest name.
    fn set_name(&mut self, name: String);
    /// Sets the list of allowed keys for a role.
    fn set_access(&mut self, role: String, allowed: Vec<String>);
    /// Returns the allowed keys for a role, if any.
    fn get_access(&self, role: &str) -> Option<Vec<String>>;
    /// Returns the full role-to-keys access map.
    fn get_all_access(&self) -> HashMap<String, Vec<String>>;

    /// Allows safe downcasting to concrete implementations.
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
    /// Builds options with the given address, skip-manifest flag and type.
    pub fn new(address: Hash, skip_manifest: bool, manifest_type: String) -> Self {
        Self {
            address,
            skip_manifest,
            get_type: manifest_type,
            ..Default::default()
        }
    }

    /// Builds empty options with default values.
    pub fn new_empty() -> Self {
        Default::default()
    }

    /// Builds options with `skip_manifest` set and a preconfigured access map.
    pub fn new_simple(manifest_type: String, access: HashMap<String, Vec<String>>) -> Self {
        Self {
            skip_manifest: true,
            get_type: manifest_type,
            access: Arc::new(Mutex::new(access)),
            ..Default::default()
        }
    }

    /// Clones an existing options value.
    pub fn from_params(params: &CreateAccessControllerOptions) -> Self {
        params.clone()
    }

    /// Builds access-controller options for a **read-only replication** topology.
    ///
    /// Only the nodes whose ids are in `writers` may write (`write` role); every node may read
    /// and replicate (`read: ["*"]`). The ids must be the hex-encoded iroh `EndpointId` of the
    /// writer nodes (`hex::encode(node_id.as_bytes())`), matching what the ticket exchange
    /// compares against the TLS-authenticated requester.
    ///
    /// This is the recommended secure configuration for the "one/two writers, many readers"
    /// pattern: writer nodes hand out read-only `DocTicket`s to readers (no namespace secret),
    /// so a compromised reader cannot originate writes. Pair it with
    /// [`crate::traits::CreateDBOptions::read_only`] on the reader nodes so they also refuse
    /// local writes and never create their own namespace.
    pub fn read_only_replication(writers: Vec<String>) -> Self {
        let mut access = HashMap::new();
        access.insert("write".to_string(), writers);
        access.insert("read".to_string(), vec!["*".to_string()]);
        Self::new_simple("simple".to_string(), access)
    }
}

/// Creates a new manifest and returns its Hash.
///
/// When `skip_manifest` is set, the existing address is returned without
/// persisting anything; otherwise the manifest is serialized to CBOR and stored
/// in Iroh.
pub async fn create(
    client: Arc<IrohClient>,
    controller_type: String,
    params: &CreateAccessControllerOptions,
) -> Result<Hash> {
    if params.skip_manifest() {
        return Ok(params.address());
    }

    // Validate that controller_type is not empty.
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

    // Serialize the manifest to CBOR.
    let cbor_data = serde_cbor::to_vec(&manifest)
        .map_err(|e| GuardianError::Store(format!("Failed to serialize manifest: {}", e)))?;

    // Validate that the serialized data is not empty.
    if cbor_data.is_empty() {
        return Err(GuardianError::Store(
            "Serialized manifest is empty".to_string(),
        ));
    }

    // Store it in Iroh using the native client.
    let response = client
        .add_bytes(cbor_data)
        .await
        .map_err(|e| GuardianError::Store(format!("Failed to store manifest in iroh: {}", e)))?;

    // Validate that Iroh returned a valid hash.
    if response.hash.is_empty() {
        return Err(GuardianError::Store("iroh returned empty hash".to_string()));
    }

    // Convert the hex hash string into a Hash.
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

/// Resolves a manifest from its address.
///
/// When `skip_manifest` is set, an in-memory manifest is returned directly from
/// the params (the controller type is required in that case); otherwise the
/// manifest is fetched from Iroh and deserialized from CBOR.
pub async fn resolve(
    client: Arc<IrohClient>,
    manifest_address: &str,
    params: &CreateAccessControllerOptions,
) -> Result<Manifest> {
    if params.skip_manifest() {
        if params.get_type().is_empty() {
            return Err(GuardianError::Store(
                "Without a manifest, the access controller type is required".to_string(),
            ));
        }

        return Ok(Manifest {
            get_type: params.get_type().to_string(),
            params: params.clone(),
        });
    }

    // Validate that the address is not empty.
    if manifest_address.is_empty() {
        return Err(GuardianError::Store(
            "Manifest address cannot be empty".to_string(),
        ));
    }

    // Strip the /iroh/ prefix if present.
    let hash_str = if let Some(stripped) = manifest_address.strip_prefix("/iroh/") {
        stripped
    } else {
        manifest_address
    };

    // Fetch the manifest data from Iroh using the hex hash.
    let data_bytes = client
        .cat_bytes(hash_str)
        .await
        .map_err(|e| GuardianError::Store(format!("Failed to load manifest from Iroh: {}", e)))?;

    // Validate that the data is not empty.
    if data_bytes.is_empty() {
        return Err(GuardianError::Store(
            "Retrieved manifest data is empty".to_string(),
        ));
    }

    // Deserialize the manifest from the CBOR data.
    let manifest: Manifest = serde_cbor::from_slice(&data_bytes)
        .map_err(|e| GuardianError::Store(format!("Failed to deserialize manifest: {}", e)))?;

    // Additional manifest validation.
    if manifest.get_type.is_empty() {
        return Err(GuardianError::Store(
            "Manifest type cannot be empty".to_string(),
        ));
    }

    Ok(manifest)
}

/// Utility function to create and validate a manifest.
pub fn create_manifest_with_validation(
    controller_type: String,
    params: CreateAccessControllerOptions,
) -> Result<Manifest> {
    // Basic validations.
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

    // Validate that the type is one of the known types.
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
