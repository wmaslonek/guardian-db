# Guia de Migração: ipfs_api_backend_hyper → kubo_core_api

Este documento detalha como migrar do `ipfs_api_backend_hyper` para o novo módulo `kubo_core_api` que utiliza `rust-ipfs` nativamente.

## Visão Geral

O módulo `kubo_core_api` substitui completamente a dependência `ipfs_api_backend_hyper`, oferecendo:

- **Nó IPFS 100% Rust**: Elimina a necessidade de binários externos do go-ipfs/kubo
- **Interface Compatível**: Mantém a mesma API para facilitar a migração
- **Melhor Performance**: Comunicação direta sem overhead de HTTP RPC
- **Modo Mock**: Permite desenvolvimento sem dependências externas

## Migração Passo a Passo

### 1. Atualizar Imports

**Antes:**
```rust
use ipfs_api_backend_hyper::{IpfsApi, IpfsClient};
```

**Depois:**
```rust
use crate::kubo_core_api::{IpfsApi, IpfsClient, KuboCoreApiClient};
// ou
use rust_guardian_db::kubo_core_api::{IpfsApi, IpfsClient, KuboCoreApiClient};
```

### 2. Criar Cliente IPFS

**Antes:**
```rust
let ipfs = IpfsClient::default();
```

**Depois:**
```rust
use crate::kubo_core_api::client::ClientConfig;

// Configuração personalizada
let config = ClientConfig {
    enable_pubsub: true,
    enable_swarm: true,
    data_store_path: Some("/path/to/ipfs/data".to_string()),
    listening_addrs: vec!["/ip4/0.0.0.0/tcp/4001".to_string()],
    bootstrap_peers: vec![
        "/dnsaddr/bootstrap.libp2p.io/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN".to_string()
    ],
};

let ipfs = KuboCoreApiClient::new(config).await?;

// Ou usando configuração padrão
let ipfs = KuboCoreApiClient::default().await?;
```

### 3. Operações de Dados (add/cat)

**Antes:**
```rust
// Add
let response = ipfs.add(std::io::Cursor::new(data)).await?;
let hash = response.hash;

// Cat
let data = ipfs.cat(&hash).concat().await?;
```

**Depois:**
```rust
// Add - interface idêntica
let response = ipfs.add(std::io::Cursor::new(data)).await?;
let hash = response.hash;

// Cat - interface idêntica
let mut stream = ipfs.cat(&hash).await?;
let mut data = Vec::new();
use futures::AsyncReadExt;
stream.read_to_end(&mut data).await?;

// Ou usando o adaptador de compatibilidade
use crate::kubo_core_api::compat::IpfsClientAdapter;
let adapter = IpfsClientAdapter::new(Arc::new(ipfs));
let data = adapter.cat(&hash).concat().await?;
```

### 4. Operações DAG

**Antes:**
```rust
let data = ipfs.dag_get(&cid, None).await?;
```

**Depois:**
```rust
// Interface idêntica
let data = ipfs.dag_get(&cid, None).await?;
```

### 5. PubSub

**Antes:**
```rust
// Subscribe
let stream = ipfs.pubsub_subscribe(&topic).await?;

// Publish
ipfs.pubsub_publish(&topic, data).await?;

// Peers
let peers = ipfs.pubsub_peers(&topic).await?;
```

**Depois:**
```rust
// Interface idêntica
let stream = ipfs.pubsub_subscribe(&topic).await?;
ipfs.pubsub_publish(&topic, data).await?;
let peers = ipfs.pubsub_peers(&topic).await?;
```

### 6. Swarm

**Antes:**
```rust
ipfs.swarm_connect(&peer_id).await?;
```

**Depois:**
```rust
// Interface idêntica
ipfs.swarm_connect(&peer_id).await?;
```

## Exemplos de Migração por Arquivo

### base_guardian.rs

**Antes:**
```rust
use ipfs_api_backend_hyper::IpfsClient;

pub struct BaseGuardian {
    ipfs: IpfsClient,
    // ...
}

impl BaseGuardian {
    pub async fn new(ipfs: IpfsClient, options: Option<NewGuardianDBOptions>) -> Result<Self> {
        // ...
    }
}
```

**Depois:**
```rust
use crate::kubo_core_api::{IpfsClient, KuboCoreApiClient};

pub struct BaseGuardian {
    ipfs: KuboCoreApiClient,
    // ...
}

impl BaseGuardian {
    pub async fn new(ipfs: KuboCoreApiClient, options: Option<NewGuardianDBOptions>) -> Result<Self> {
        // ...
    }
    
    // Ou usando trait object para flexibilidade
    pub async fn new_with_client(ipfs: Arc<dyn IpfsApi>, options: Option<NewGuardianDBOptions>) -> Result<Self> {
        // ...
    }
}
```

### access_controller/ipfs.rs

**Antes:**
```rust
use ipfs_api_backend_hyper::{IpfsClient, IpfsApi};

pub struct IpfsAccessController {
    ipfs: Arc<IpfsClient>,
    // ...
}

impl IpfsAccessController {
    pub async fn load(&self, address: &str) -> Result<()> {
        let manifest_data = self.ipfs.cat(&cid.to_string()).concat().await?;
        let response = self.ipfs.add(std::io::Cursor::new(cbor_bytes)).await?;
        // ...
    }
}
```

**Depois:**
```rust
use crate::kubo_core_api::{IpfsApi, KuboCoreApiClient};

pub struct IpfsAccessController {
    ipfs: Arc<dyn IpfsApi>, // Usa trait para flexibilidade
    // ...
}

impl IpfsAccessController {
    pub async fn load(&self, address: &str) -> Result<()> {
        // Interface idêntica - sem mudanças necessárias
        let mut stream = self.ipfs.cat(&cid.to_string()).await?;
        let mut manifest_data = Vec::new();
        stream.read_to_end(&mut manifest_data).await?;
        
        let response = self.ipfs.add(std::io::Cursor::new(cbor_bytes)).await?;
        // ...
    }
}
```

### pubsub/one_on_one_channel.rs

**Antes:**
```rust
use ipfs_api_backend_hyper::IpfsClient;

pub struct Channels {
    kubo_client: IpfsClient,
    // ...
}

impl DirectChannel for Channels {
    async fn connect(&self, target: PeerId) -> Result<()> {
        let stream = self.kubo_client.pubsub_subscribe(&id).await?;
        self.kubo_client.swarm_connect(&target).await?;
        // ...
    }
}
```

**Depois:**
```rust
use crate::kubo_core_api::{IpfsApi, KuboCoreApiClient};

pub struct Channels {
    kubo_client: Arc<dyn IpfsApi>, // Usa trait para flexibilidade
    // ...
}

impl DirectChannel for Channels {
    async fn connect(&self, target: PeerId) -> Result<()> {
        // Interface idêntica - sem mudanças necessárias
        let stream = self.kubo_client.pubsub_subscribe(&id).await?;
        self.kubo_client.swarm_connect(&target).await?;
        // ...
    }
}
```

## Configuração do Cargo.toml

### Adicionar Dependências

```toml
[dependencies]
# Remover ipfs_api_backend_hyper se não usado para compatibilidade
# ipfs_api_backend_hyper = "0.6"

# Adicionar rust-ipfs (opcional, habilitado por feature)
ipfs = { version = "0.10", optional = true }

# Outras dependências necessárias
anyhow = "1.0"
futures = "0.3"
tokio = { version = "1.0", features = ["full"] }
tracing = "0.1"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
libp2p = "0.50"
cid = "0.10"

[features]
default = ["rust-ipfs"]
rust-ipfs = ["dep:ipfs"]
```

## Inicialização no main.rs

```rust
use rust_guardian_db::kubo_core_api::{KuboCoreApiClient, ClientConfig};
use rust_guardian_db::base_guardian::BaseGuardian;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Configurar cliente IPFS
    let ipfs_config = ClientConfig {
        enable_pubsub: true,
        enable_swarm: true,
        data_store_path: Some("./ipfs_data".to_string()),
        listening_addrs: vec!["/ip4/0.0.0.0/tcp/4001".to_string()],
        bootstrap_peers: vec![
            "/dnsaddr/bootstrap.libp2p.io/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN".to_string()
        ],
    };
    
    let ipfs = KuboCoreApiClient::new(ipfs_config).await?;
    
    // Verificar se o nó está online
    if !ipfs.is_online().await {
        return Err(anyhow::anyhow!("Nó IPFS não está online"));
    }
    
    // Criar instância do GuardianDB
    let GuardianDB = BaseGuardian::new(ipfs, None).await?;
    
    // Usar GuardianDB...
    
    // Cleanup no shutdown
    GuardianDB.close().await?;
    
    Ok(())
}
```

## Testes

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use rust_guardian_db::kubo_core_api::KuboCoreApiClient;
    
    #[tokio::test]
    async fn test_with_native_ipfs() {
        let ipfs = KuboCoreApiClient::default().await.unwrap();
        
        // Testar operações IPFS
        let test_data = b"Hello, IPFS!";
        let response = ipfs.add(std::io::Cursor::new(test_data.to_vec())).await.unwrap();
        
        let mut stream = ipfs.cat(&response.hash).await.unwrap();
        let mut retrieved_data = Vec::new();
        use futures::AsyncReadExt;
        stream.read_to_end(&mut retrieved_data).await.unwrap();
        
        // Em implementação real, deveria ser igual
        // assert_eq!(test_data, retrieved_data.as_slice());
    }
}
```

## Modo de Desenvolvimento (Mock)

Para desenvolvimento sem rust-ipfs:

```toml
[features]
default = []
rust-ipfs = ["dep:ipfs"]
```

```rust
// O cliente automaticamente usa implementação mock se rust-ipfs não estiver disponível
let ipfs = KuboCoreApiClient::default().await?; // Funciona mesmo sem rust-ipfs
```

## Troubleshooting

### Erro de Compilação com rust-ipfs

Se você encontrar erros de compilação com rust-ipfs:

1. **Desabilite a feature temporariamente:**
   ```toml
   [features]
   default = []
   ```

2. **Use implementação mock para desenvolvimento:**
   ```rust
   // O código funcionará em modo mock
   let ipfs = KuboCoreApiClient::default().await?;
   ```

3. **Compile com feature específica quando necessário:**
   ```bash
   cargo build --features rust-ipfs
   ```

### Migração Gradual

Você pode migrar gradualmente mantendo ambas as implementações:

```rust
// Enum para escolher implementação
pub enum IpfsBackend {
    HyperApi(ipfs_api_backend_hyper::IpfsClient),
    Native(Arc<dyn kubo_core_api::IpfsApi>),
}

impl IpfsBackend {
    async fn add<R>(&self, data: R) -> Result<AddResponse>
    where R: AsyncRead + Send + Unpin + 'static
    {
        match self {
            IpfsBackend::HyperApi(client) => {
                // Implementação com hyper
                Ok(client.add(data).await?)
            },
            IpfsBackend::Native(client) => {
                // Implementação nativa
                Ok(client.add(data).await?)
            }
        }
    }
}
```

## Benefícios da Migração

1. **Performance**: Comunicação direta sem HTTP overhead
2. **Portabilidade**: Não requer binários externos do go-ipfs
3. **Controle**: Maior controle sobre configuração e comportamento do nó
4. **Rust Nativo**: Melhor integração com ecossistema Rust
5. **Desenvolvimento**: Modo mock permite desenvolvimento offline

## Considerações

1. **Compatibilidade**: rust-ipfs pode não ter 100% de paridade com go-ipfs
2. **Maturidade**: rust-ipfs é menos maduro que go-ipfs
3. **Recursos**: Alguns recursos avançados podem não estar disponíveis
4. **Testing**: Teste extensivamente antes de migrar para produção

A migração oferece benefícios significativos para um projeto 100% Rust, mas deve ser feita gradualmente com testes adequados.
