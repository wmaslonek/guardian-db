# IPFS Core API - Migração e Refatoração

## Visão Geral

O módulo `kubo_core_api` foi refatorado e reorganizado em `ipfs_core_api` com uma arquitetura modular e melhor organização de código.

## Nova Estrutura

```
ipfs_core_api/
├── mod.rs          # Módulo principal com re-exports
├── types.rs        # Definições de tipos e estruturas
├── client.rs       # Cliente principal IPFS
├── config.rs       # Configurações do cliente  
├── errors.rs       # Tratamento de erros específicos
└── compat.rs       # Camada de compatibilidade
```

## Migração do Código

### Antes (kubo_core_api)
```rust
use crate::kubo_core_api::client::KuboCoreApiClient;
use crate::kubo_core_api::compat::IpfsClientAdapter;

let client = KuboCoreApiClient::default().await?;
```

### Depois (ipfs_core_api)
```rust
use crate::ipfs_core_api::{IpfsClient, ClientConfig};
use crate::ipfs_core_api::compat::IpfsClientAdapter;

let client = IpfsClient::default().await?;
// ou
let config = ClientConfig::development();
let client = IpfsClient::new(config).await?;
```

### Compatibilidade Mantida
```rust
// Ainda funciona para compatibilidade
use crate::ipfs_core_api::KuboCoreApiClient; // Alias para IpfsClient
let client = KuboCoreApiClient::default().await?;
```

## Principais Melhorias

### 1. Organização Modular
- **types.rs**: Todos os tipos centralizados com validações
- **config.rs**: Configurações robustas para dev/prod/test
- **errors.rs**: Sistema de erros específico para IPFS
- **client.rs**: Cliente principal com funcionalidades completas
- **compat.rs**: Compatibilidade total com código existente

### 2. Configurações Predefinidas
```rust
// Desenvolvimento
let client = IpfsClient::development().await?;

// Produção
let client = IpfsClient::production().await?;

// Testes
let client = IpfsClient::testing().await?;

// Offline
let config = ClientConfig::offline();
let client = IpfsClient::new(config).await?;
```

### 3. Validação de Configuração
```rust
let mut config = ClientConfig::default();
config.validate()?; // Verifica consistência
```

### 4. Tratamento de Erros Específicos
```rust
use crate::ipfs_core_api::errors::IpfsError;

match error {
    IpfsError::NetworkError(_) => /* recuperável */,
    IpfsError::DataNotFound(_) => /* não encontrado */,
    IpfsError::TimeoutError(_) => /* tente novamente */,
    _ => /* outros erros */,
}
```

### 5. Tipos com Validação
```rust
let response = AddResponse::new(hash, size);
let size_bytes = response.size_bytes()?; // Converte string para usize

let node_info = NodeInfo::mock(peer_id);
assert!(node_info.is_mock()); // Detecta se é mock

let message = PubsubMessage::new(from, topic, data);
let text = message.data_as_string()?; // Converte para UTF-8
```

## Funcionalidades Adicionais

### 1. Operações de Pin
```rust
// Pin objeto
let pin_response = client.pin_add(&hash, true).await?;

// Listar pins
let pins = client.pin_ls(Some(PinType::Recursive)).await?;

// Remover pin
let rm_response = client.pin_rm(&hash).await?;
```

### 2. Estatísticas do Repositório
```rust
let stats = client.repo_stat().await?;
println!("Objetos: {}, Tamanho: {} bytes", 
         stats.num_objects, stats.repo_size);
```

### 3. Gestão de Estado
```rust
// Verificar se está online
if client.is_online().await {
    println!("Cliente IPFS online");
}

// Obter configuração
let config = client.config();

// Obter node ID
let node_id = client.node_id();

// Shutdown graceful
client.shutdown().await?;
```

## Compatibilidade com Código Existente

### 1. Adaptador Completo
```rust
use crate::ipfs_core_api::compat::IpfsClientAdapter;

// Migração sem mudanças de código
let adapter = IpfsClientAdapter::from_str("http://localhost:5001").await?;

// Mesmo API do ipfs_api_backend_hyper
let response = adapter.add(data).await?;
let stream = adapter.cat(&hash).await;
let bytes = stream.concat().await?;
```

### 2. Macro de Migração
```rust
use crate::migrate_ipfs_client;

// Substitui automaticamente
let client = migrate_ipfs_client!(old_http_client);
```

### 3. Trait de Compatibilidade
```rust
use crate::ipfs_core_api::compat::IpfsClientCompat;

// Métodos compat para migração gradual
let data = client.cat_compat(&hash).await?;
```

## Testes e Validação

```bash
# Testa módulo completo
cargo test ipfs_core_api --all-features

# Testa compatibilidade
cargo test ipfs_core_api::compat

# Testa configurações
cargo test ipfs_core_api::config
```

## Roadmap de Migração

### Fase 1: ✅ Completa
- [x] Refatoração modular
- [x] Tipos centralizados
- [x] Configurações robustas
- [x] Compatibilidade total

### Fase 2: Em Progresso
- [ ] Migração gradual do código existente
- [ ] Atualização da documentação
- [ ] Exemplos atualizados

### Fase 3: Futuro
- [ ] Deprecação do kubo_core_api
- [ ] Otimizações de performance
- [ ] Integração com libp2p real

## Notas Importantes

1. **kubo_core_api ainda disponível**: Mantido para compatibilidade durante migração
2. **Funcionalidade idêntica**: Mesma API, melhor organização
3. **Testes abrangentes**: Cada módulo tem sua própria suíte de testes
4. **Configuração flexível**: Suporte para dev, prod, test e offline
5. **Migração gradual**: Código existente continua funcionando

## Exemplos de Uso

Veja os arquivos de exemplo na pasta `examples/` para demonstrações completas da nova API.
