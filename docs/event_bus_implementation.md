# Event Bus Implementation para Rust Guardian DB

## 🎯 Substituindo o Event Bus do Go

Este documento explica como substituímos o event bus do Go por uma implementação baseada em **Tokio broadcast channels**, oferecendo funcionalidade equivalente com performance superior e type safety.

## 🔄 Migração do Go para Rust

### Go (Original)
```go
// Go event bus usage
bus := eventbus.New()
emitter := bus.Emitter(new(EventType), eventbus.Stateful)
subscription := bus.Subscribe(new(EventType))

emitter.Emit(event)
```

### Rust (Nossa implementação)
```rust
// Rust event bus usage
let bus = EventBus::new();
let emitter = bus.emitter::<EventType>().await?;
let mut receiver = bus.subscribe::<EventType>().await?;

emitter.emit(event)?;
```

## ✨ Funcionalidades Implementadas

### 1. **Type-Safe Event System**
- **Go**: Usa `interface{}` e reflection
- **Rust**: Type safety em compile-time com generics

### 2. **Broadcast Channels**
- **Go**: Event bus com múltiplos subscribers
- **Rust**: `tokio::sync::broadcast` para pub/sub pattern

### 3. **Concurrent Access**
- **Go**: Mutex com goroutines
- **Rust**: `Arc<RwLock<>>` com async/await

### 4. **Backpressure Handling**
- **Go**: Blocking channels
- **Rust**: Configurable buffer size (default: 1024)

## 🚀 Vantagens da Nossa Implementação

### 1. **Performance**
```rust
// Zero-allocation para tipos que implementam Clone
// Broadcast channels são otimizados para múltiplos receivers
let emitter = bus.emitter::<MyEvent>().await?;
emitter.emit(event)?; // O(1) broadcast para todos os subscribers
```

### 2. **Type Safety**
```rust
// Compile-time guarantees - não há runtime panics por tipo incorreto
let string_emitter = bus.emitter::<String>().await?;
let int_emitter = bus.emitter::<i32>().await?;

// Erro de compilação se tentar emitir tipo errado
// string_emitter.emit(42); // ❌ Won't compile
```

### 3. **Async/Await Native**
```rust
// Integração natural com async Rust
async fn handle_events(bus: &EventBus) -> Result<()> {
    let mut receiver = bus.subscribe::<DatabaseEvent>().await?;
    
    while let Ok(event) = receiver.recv().await {
        // Process event asynchronously
        process_database_event(event).await?;
    }
    
    Ok(())
}
```

### 4. **Memory Safety**
```rust
// Automatic cleanup quando receivers são dropped
{
    let receiver = bus.subscribe::<MyEvent>().await?;
    // receiver é automaticamente cleaned up no fim do scope
}
// Memória liberada automaticamente
```

## 📋 API Reference

### EventBus
```rust
impl EventBus {
    /// Criar novo event bus
    pub fn new() -> Self
    
    /// Criar emitter type-safe para tipo T
    pub async fn emitter<T>() -> Result<Emitter<T>, Error>
    where T: Clone + Send + Sync + 'static
    
    /// Subscribe para receber eventos do tipo T
    pub async fn subscribe<T>() -> Result<broadcast::Receiver<T>, Error>
    where T: Clone + Send + Sync + 'static
}
```

### Emitter<T>
```rust
impl<T> Emitter<T> {
    /// Emitir evento para todos os subscribers
    pub fn emit(&self, event: T) -> Result<(), Error>
    
    /// Contagem de receivers ativos
    pub fn receiver_count(&self) -> usize
}
```

## 🔧 Casos de Uso

### 1. **Database Events**
```rust
#[derive(Clone, Debug)]
struct DatabaseEvent {
    operation: String,
    table: String,
    data: Vec<u8>,
}

let db_emitter = bus.emitter::<DatabaseEvent>().await?;
let mut db_receiver = bus.subscribe::<DatabaseEvent>().await?;

// Emit database changes
db_emitter.emit(DatabaseEvent {
    operation: "INSERT".to_string(),
    table: "users".to_string(),
    data: user_data,
})?;
```

### 2. **Network Events**
```rust
#[derive(Clone, Debug)]
struct NetworkEvent {
    peer_id: PeerId,
    event_type: NetworkEventType,
    payload: Vec<u8>,
}

let net_emitter = bus.emitter::<NetworkEvent>().await?;
let mut net_receiver = bus.subscribe::<NetworkEvent>().await?;
```

### 3. **System Events**
```rust
#[derive(Clone, Debug)]
enum SystemEvent {
    Shutdown,
    Restart,
    ConfigReload,
    HealthCheck(HealthStatus),
}

let sys_emitter = bus.emitter::<SystemEvent>().await?;
let mut sys_receiver = bus.subscribe::<SystemEvent>().await?;
```

## 🛠 Configuração Avançada

### Buffer Size Customization
```rust
// Para eventos high-frequency, aumentar buffer
pub struct EventBus {
    channels: Arc<RwLock<HashMap<TypeId, Box<dyn Any + Send + Sync>>>>,
    default_buffer_size: usize, // Adicionar configuração
}

impl EventBus {
    pub fn with_buffer_size(buffer_size: usize) -> Self {
        // Custom buffer size para diferentes casos de uso
    }
}
```

### Error Handling
```rust
// Handling receiver lagging
match receiver.recv().await {
    Ok(event) => process_event(event).await,
    Err(broadcast::error::RecvError::Lagged(count)) => {
        log::warn!("Receiver lagged by {} events", count);
        // Continue processing
    }
    Err(broadcast::error::RecvError::Closed) => {
        log::info!("Event bus channel closed");
        break;
    }
}
```

## 🔄 Migração Guide

### Passo 1: Substituir imports
```rust
// Antes
use crate::go_event_bus::{Bus, Emitter};

// Depois  
use crate::pubsub::event::{EventBus, Emitter};
```

### Passo 2: Atualizar inicialização
```rust
// Antes
let bus = Bus::new();

// Depois
let bus = EventBus::new();
```

### Passo 3: Converter emitters
```rust
// Antes (Go-style)
let emitter = bus.emitter(EventType::new()).unwrap();

// Depois (Rust-style)
let emitter = bus.emitter::<EventType>().await?;
```

### Passo 4: Atualizar subscribers
```rust
// Antes (Go-style)  
let subscription = bus.subscribe(EventType::new());

// Depois (Rust-style)
let mut receiver = bus.subscribe::<EventType>().await?;
```

## 📊 Performance Benchmarks

```rust
// Exemplo de benchmark (adicionar ao projeto)
#[cfg(test)]
mod benchmarks {
    use super::*;
    use criterion::{criterion_group, criterion_main, Criterion};
    
    fn benchmark_event_emission(c: &mut Criterion) {
        c.bench_function("emit_1000_events", |b| {
            b.iter(|| {
                // Benchmark code
            })
        });
    }
    
    criterion_group!(benches, benchmark_event_emission);
    criterion_main!(benches);
}
```

## 🧪 Testing

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_event_bus_basic_functionality() {
        let bus = EventBus::new();
        let emitter = bus.emitter::<String>().await.unwrap();
        let mut receiver = bus.subscribe::<String>().await.unwrap();
        
        emitter.emit("test".to_string()).unwrap();
        
        let received = receiver.recv().await.unwrap();
        assert_eq!(received, "test");
    }
    
    #[tokio::test]
    async fn test_multiple_subscribers() {
        let bus = EventBus::new();
        let emitter = bus.emitter::<i32>().await.unwrap();
        
        let mut receiver1 = bus.subscribe::<i32>().await.unwrap();
        let mut receiver2 = bus.subscribe::<i32>().await.unwrap();
        
        emitter.emit(42).unwrap();
        
        assert_eq!(receiver1.recv().await.unwrap(), 42);
        assert_eq!(receiver2.recv().await.unwrap(), 42);
    }
}
```

## 🚧 Próximos Passos

1. **Adicionar métricas**: Contadores de eventos emitidos/recebidos
2. **Persistent events**: Opcional para eventos críticos
3. **Event filtering**: Subscribers com filtros
4. **Dead letter queue**: Para eventos não processados
5. **Event replay**: Capacidade de replay de eventos

Esta implementação oferece uma base sólida e performante para substituir completamente o event bus do Go, mantendo a API familiar mas aproveitando as vantagens do sistema de tipos do Rust.
