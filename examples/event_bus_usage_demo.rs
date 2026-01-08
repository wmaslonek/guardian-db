use guardian_db::guardian::error::Result;
use guardian_db::p2p::{EventBus, PayloadEmitter};
use guardian_db::traits::EventPubSubPayload;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomEvent {
    pub message: String,
    pub timestamp: u64,
}

#[derive(Debug, Clone)]
pub struct DatabaseEvent {
    pub action: String,
    pub data: Vec<u8>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // 1. Criar o Event Bus
    let event_bus = EventBus::new();

    // 2. Exemplo de uso com PayloadEmitter
    println!("=== PayloadEmitter Example ===");

    let payload_emitter = PayloadEmitter::new(&event_bus).await?;
    let mut payload_receiver = event_bus.subscribe::<EventPubSubPayload>().await?;

    // Spawn tarefa para escutar eventos
    tokio::spawn(async move {
        while let Ok(event) = payload_receiver.recv().await {
            println!("Received payload event from peer: {}", event.peer);
            println!("Payload size: {} bytes", event.payload.len());
        }
    });

    // Emitir evento
    let test_payload = EventPubSubPayload {
        payload: b"Hello, World!".to_vec(),
        peer: iroh::SecretKey::generate(rand_core::OsRng).public(),
    };
    payload_emitter.emit_payload(test_payload)?;

    // 3. Exemplo com eventos customizados
    println!("=== Custom Events Example ===");

    let custom_emitter = event_bus.emitter::<CustomEvent>().await?;
    let mut custom_receiver = event_bus.subscribe::<CustomEvent>().await?;

    // Spawn tarefa para escutar eventos customizados
    tokio::spawn(async move {
        while let Ok(event) = custom_receiver.recv().await {
            println!("Custom event: {} at {}", event.message, event.timestamp);
        }
    });

    // Emitir eventos customizados
    for i in 0..3 {
        let custom_event = CustomEvent {
            message: format!("Event number {}", i),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        };
        custom_emitter.emit(custom_event)?;
    }

    // 4. Exemplo com múltiplos subscribers
    println!("=== Multiple Subscribers Example ===");

    let db_emitter = event_bus.emitter::<DatabaseEvent>().await?;

    // Criar múltiplos receivers
    let mut db_receiver1 = event_bus.subscribe::<DatabaseEvent>().await?;
    let mut db_receiver2 = event_bus.subscribe::<DatabaseEvent>().await?;

    // Spawn tarefas para múltiplos listeners
    tokio::spawn(async move {
        while let Ok(event) = db_receiver1.recv().await {
            println!(
                "DB Listener 1 - Action: {}, Data size: {}",
                event.action,
                event.data.len()
            );
        }
    });

    tokio::spawn(async move {
        while let Ok(event) = db_receiver2.recv().await {
            println!("DB Listener 2 - Processing action: {}", event.action);
        }
    });

    // Emitir eventos para database
    for action in &["CREATE", "UPDATE", "DELETE"] {
        let db_event = DatabaseEvent {
            action: action.to_string(),
            data: format!("data for {}", action).into_bytes(),
        };
        db_emitter.emit(db_event)?;
    }

    // 5. Demonstrar contagem de receivers
    println!("=== Receiver Count Example ===");

    let counter_emitter = event_bus.emitter::<String>().await?;
    let _receiver1 = event_bus.subscribe::<String>().await?;
    let _receiver2 = event_bus.subscribe::<String>().await?;
    let _receiver3 = event_bus.subscribe::<String>().await?;

    println!(
        "Active receivers for String events: {}",
        counter_emitter.receiver_count()
    );

    // Aguardar um pouco para garantir que todas as mensagens sejam processadas
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    println!("Event Bus example completed successfully!");

    Ok(())
}
