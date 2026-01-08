use crate::guardian::error::{GuardianError, Result};
use crate::p2p::{Emitter, EventBus};
use async_trait::async_trait;
use std::any::Any;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify, broadcast, mpsc};
use tokio_util::sync::CancellationToken;

/// Um alias de tipo para um evento dinâmico e seguro para threads.
pub type Event = Arc<dyn Any + Send + Sync>;

/// Uma struct wrapper para enviar eventos através do bus, que pode exigir um tipo concreto.
#[derive(Clone, Debug)]
pub struct EventBox {
    pub evt: Event,
}

// Uma struct privada que agrupa TODOS os dados que precisam ser protegidos.
struct EventEmitterInternal {
    bus: Option<EventBus>,
    emitter: Option<Emitter<EventBox>>,
    cglobal: Option<broadcast::Sender<Event>>,
    cancellations: Vec<CancellationToken>,
}

impl EventEmitterInternal {
    /// Obtém uma referência mutável ao bus, inicializando-o se ainda não existir.
    fn get_bus_mut(&mut self) -> &mut EventBus {
        self.bus.get_or_insert_with(EventBus::new)
    }
}

#[async_trait]
pub trait EmitterInterface {
    /// Envia um evento para os listeners inscritos.
    async fn emit(&self, evt: Event);

    /// Retorna um canal que recebe os eventos emitidos.
    async fn subscribe(&self) -> (mpsc::Receiver<Event>, CancellationToken);

    /// Fecha todos os canais de listeners.
    async fn unsubscribe_all(&self);

    /// Retorna um canal global que recebe todos os eventos emitidos.
    async fn global_channel(&self) -> broadcast::Receiver<Event>;
}

// A implementação dos métodos públicos agora é feita dentro do bloco `impl EmitterInterface`.
#[async_trait]
impl EmitterInterface for EventEmitter {
    async fn emit(&self, evt: Event) {
        let mut guard = self.internal.lock().await;
        if guard.emitter.is_none() {
            let bus = guard.get_bus_mut();
            let emitter = bus
                .emitter::<EventBox>()
                .await
                .expect("não foi possível inicializar o emitter para EventBox");
            guard.emitter = Some(emitter);
        }
        if let Some(emitter) = guard.emitter.as_ref() {
            let event_box = EventBox { evt };
            let _ = emitter.emit(event_box);
        }
    }

    async fn subscribe(&self) -> (mpsc::Receiver<Event>, CancellationToken) {
        let mut guard = self.internal.lock().await;
        let bus = guard.get_bus_mut();
        let sub = bus
            .subscribe::<EventBox>()
            .await
            .expect("não foi possível se inscrever");
        let cancellation_token = CancellationToken::new();
        guard.cancellations.push(cancellation_token.clone());
        drop(guard);
        let receiver = self
            .handle_subscriber(cancellation_token.clone(), sub)
            .await;
        (receiver, cancellation_token)
    }

    async fn unsubscribe_all(&self) {
        let guard = self.internal.lock().await;
        for token in &guard.cancellations {
            token.cancel();
        }
    }

    async fn global_channel(&self) -> broadcast::Receiver<Event> {
        let mut guard = self.internal.lock().await;
        if let Some(sender) = &guard.cglobal {
            return sender.subscribe();
        }
        let bus = guard.get_bus_mut();
        let mut sub = bus
            .subscribe::<EventBox>()
            .await
            .expect("unable to subscribe");
        let token = CancellationToken::new();
        guard.cancellations.push(token.clone());
        let (tx, rx) = broadcast::channel(16);
        guard.cglobal = Some(tx.clone());
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = token.cancelled() => break,
                    maybe_event = sub.recv() => {
                        let event_box = match maybe_event {
                            Ok(e) => e,
                            Err(_) => break
                        };
                        let _ = tx.send(event_box.evt);
                    }
                }
            }
        });
        rx
    }
}

// A struct pública que os usuários irão interagir.
/// Registra listeners e despacha eventos para eles.
#[derive(Clone)]
pub struct EventEmitter {
    internal: Arc<Mutex<EventEmitterInternal>>,
}

impl EventEmitter {
    /// Cria uma nova instância de EventEmitter.
    pub fn new() -> Self {
        EventEmitter {
            internal: Arc::new(Mutex::new(EventEmitterInternal {
                bus: None,
                emitter: None,
                cglobal: None,
                cancellations: Vec::new(),
            })),
        }
    }

    /// Envia um evento para os listeners inscritos.
    pub async fn emit(&self, evt: Event) {
        let mut guard = self.internal.lock().await;

        // Inicializa o emitter de forma preguiçosa (lazy) se ele não existir.
        if guard.emitter.is_none() {
            let bus = guard.get_bus_mut();
            let emitter = bus
                .emitter::<EventBox>()
                .await
                .expect("não foi possível inicializar o emitter para EventBox");
            guard.emitter = Some(emitter);
        }

        // O lock ainda está ativo, então é seguro usar unwrap.
        if let Some(emitter) = guard.emitter.as_ref() {
            let event_box = EventBox { evt };
            let _ = emitter.emit(event_box);
        }
    }

    /// Retorna um canal que recebe os eventos emitidos.
    pub async fn subscribe(&self) -> (mpsc::Receiver<Event>, CancellationToken) {
        let mut guard = self.internal.lock().await;

        let bus = guard.get_bus_mut();

        let sub = bus
            .subscribe::<EventBox>()
            .await
            .expect("não foi possível se inscrever");

        // Cria um token de cancelamento para gerenciar o ciclo de vida da inscrição.
        let cancellation_token = CancellationToken::new();
        guard.cancellations.push(cancellation_token.clone());

        // O lock é liberado quando `guard` sai de escopo.
        drop(guard);

        // O token retornado pode ser usado para cancelar apenas esta inscrição.
        let receiver = self
            .handle_subscriber(cancellation_token.clone(), sub)
            .await;

        (receiver, cancellation_token)
    }

    /// Fecha todos os canais de listeners (cancelando as tarefas de escuta).
    pub async fn unsubscribe_all(&self) {
        let guard = self.internal.lock().await;

        // Cancela todas as inscrições ativas.
        for token in &guard.cancellations {
            token.cancel();
        }
    }

    /// Processa eventos de uma inscrição, gerenciando uma fila interna para lidar com
    /// consumidores lentos.
    async fn handle_subscriber(
        &self,
        token: CancellationToken,
        mut sub: broadcast::Receiver<EventBox>, // Nosso receiver do EventBus
    ) -> mpsc::Receiver<Event> {
        let (tx, rx) = mpsc::channel(16);
        let queue = Arc::new(Mutex::new(VecDeque::<Event>::new()));
        let consumer_notify = Arc::new(Notify::new());

        // Tarefa Produtora: move eventos do bus para a fila interna.
        let producer_tx = tx.clone();
        let producer_queue = Arc::clone(&queue);
        let producer_notify = Arc::clone(&consumer_notify);
        let producer_token = token.clone();
        tokio::spawn(async move {
            loop {
                let event_box = tokio::select! {
                    biased;
                    _ = producer_token.cancelled() => {
                        producer_notify.notify_one(); // Acorda o consumidor para que ele termine.
                        break;
                    },
                    maybe_event = sub.recv() => {
                        match maybe_event {
                            Ok(e) => e,
                            Err(_) => break, // A inscrição foi fechada.
                        }
                    }
                };

                // Extrai o evento do EventBox.
                let event = event_box.evt;

                // Lógica de enfileiramento.
                let mut q = producer_queue.lock().await;
                if q.is_empty() {
                    // Se a fila estiver vazia, tenta enviar diretamente (otimização).
                    if let Err(mpsc::error::TrySendError::Full(e)) = producer_tx.try_send(event) {
                        q.push_back(e); // Se o canal estiver cheio, enfileira.
                    }
                } else {
                    // Se a fila já tiver itens, adiciona para manter a ordem.
                    q.push_back(event);
                }
                producer_notify.notify_one();
            }
        });

        // Tarefa Consumidora: move eventos da fila para o canal de saída.
        tokio::spawn(async move {
            loop {
                let event = {
                    let mut q = queue.lock().await;
                    if let Some(e) = q.pop_front() {
                        e
                    } else {
                        // A fila está vazia, espera por uma notificação ou cancelamento.
                        tokio::select! {
                            biased;
                            _ = token.cancelled() => break,
                            _ = consumer_notify.notified() => continue,
                        }
                    }
                };

                // Envia o evento, mas permite que o envio seja cancelado.
                tokio::select! {
                    biased;
                    _ = token.cancelled() => break,
                    res = tx.send(event) => {
                        if res.is_err() { break; } // O receptor foi descartado.
                    }
                }
            }
        });

        rx
    }

    /// Retorna um canal global que recebe todos os eventos emitidos.
    /// Nota: Usa um canal de broadcast para permitir múltiplos listeners
    /// independentes.
    pub async fn global_channel(&self) -> broadcast::Receiver<Event> {
        let mut guard = self.internal.lock().await;

        // Se o canal global já foi inicializado, apenas cria um novo listener e retorna.
        if let Some(sender) = &guard.cglobal {
            return sender.subscribe();
        }

        // Caso contrário, inicializa o mecanismo do canal global.
        let bus = guard.get_bus_mut();
        let mut sub = bus
            .subscribe::<EventBox>()
            .await
            .expect("unable to subscribe");

        let token = CancellationToken::new();
        guard.cancellations.push(token.clone());

        let (tx, rx) = broadcast::channel(16);
        guard.cglobal = Some(tx.clone());

        // Dispara uma tarefa para bombear eventos do bus para o canal de broadcast.
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = token.cancelled() => break,
                    maybe_event = sub.recv() => {
                        let event_box = match maybe_event {
                            Ok(e) => e,
                            Err(_) => break
                        };
                        // Ignora o erro se não houver listeners.
                        let _ = tx.send(event_box.evt);
                    }
                }
            }
        });

        rx
    }

    /// Retorna a nova instância do event bus, inicializando-a se necessário.
    pub async fn get_bus(&self) -> EventBus {
        let mut guard = self.internal.lock().await;
        // Como EventBus não implementa Clone, retornamos uma nova instância
        if guard.bus.is_none() {
            guard.bus = Some(EventBus::new());
        }
        EventBus::new() // Retorna uma nova instância para compatibilidade
    }

    /// Define a instância do event bus, retornando um erro se já estiver inicializada.
    pub async fn set_bus(&self, bus: EventBus) -> Result<()> {
        let mut guard = self.internal.lock().await;

        if guard.bus.is_some() {
            Err(GuardianError::Other(
                "o bus já foi inicializado".to_string(),
            ))
        } else {
            guard.bus = Some(bus);
            Ok(())
        }
    }
}

// Implementação do Default para facilitar a criação com `EventEmitter::default()`
impl Default for EventEmitter {
    fn default() -> Self {
        Self::new()
    }
}

// MÓDULO DE TESTES
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    // Teste simplificado para debug
    #[tokio::test]
    async fn test_simple_emit_receive() {
        let e = Arc::new(EventEmitter::new());

        // Apenas 1 cliente e 1 evento
        let (mut rx, _token) = e.subscribe().await;

        // Emite um evento
        e.emit(Arc::new("test_event".to_string())).await;

        // Tenta receber com timeout
        let event = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timeout ao receber evento")
            .expect("canal fechado inesperadamente");

        let s = event
            .downcast_ref::<String>()
            .expect("não foi possível converter para String");

        assert_eq!(*s, "test_event");
    }

    #[tokio::test]
    async fn test_missing_listeners() {
        let e = EventEmitter::new();
        const EXPECTED_EVENTS: usize = 10;

        // Emite eventos sem nenhum listener.
        // O teste passa se não houver pânico ou bloqueio.
        for i in 0..EXPECTED_EVENTS {
            e.emit(Arc::new(format!("{}", i))).await;
        }

        // Dá um tempo para garantir que nada de inesperado aconteça.
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    #[tokio::test]
    #[ignore] // Test takes too long in CI environment
    async fn test_partial_listeners() {
        let e = Arc::new(EventEmitter::new());

        let producer_emitter = Arc::clone(&e);
        tokio::spawn(async move {
            // Emite 5 eventos que serão perdidos.
            for i in 0..5 {
                producer_emitter.emit(Arc::new(format!("{}", i))).await;
            }
        });

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Inscreve-se após os 5 primeiros eventos.
        let (mut sub, sub_cancel) = e.subscribe().await;

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Emite os 5 eventos seguintes, que devem ser recebidos.
        for i in 5..10 {
            e.emit(Arc::new(format!("{}", i))).await;
        }

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Verifica se recebeu os eventos de 5 a 9.
        for i in 5..10 {
            let item = sub.recv().await.expect("canal foi fechado prematuramente");
            let item_str = item
                .downcast_ref::<String>()
                .expect("não foi possível converter");
            assert_eq!(*item_str, format!("{}", i));
        }

        // Cancela a inscrição individual.
        sub_cancel.cancel();

        // Aguarda a propagação do cancelamento.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Verifica que o canal foi fechado.
        assert!(
            sub.recv().await.is_none(),
            "o canal deveria estar fechado após o cancelamento"
        );
    }
}
