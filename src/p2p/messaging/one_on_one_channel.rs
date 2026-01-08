use crate::guardian::error::{GuardianError, Result};
use crate::p2p::network::IrohClient;
use crate::p2p::network::core::gossip::EpidemicPubSub;
use crate::p2p::new_event_payload;
use crate::traits::{
    DirectChannel, DirectChannelEmitter, DirectChannelFactory, DirectChannelOptions,
    PubSubInterface, PubSubTopic,
};
use async_trait::async_trait;
use futures::stream::StreamExt;
use iroh::NodeId;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{Span, debug, error, info, instrument, warn};

// Constantes do protocolo
const PROTOCOL: &str = "pubsub-direct-channel/v1";

// Implemetação da trait `DirectChannel` para a struct `Channels`.
#[async_trait]
impl DirectChannel for Channels {
    type Error = GuardianError;

    /// Inicia a conexão e o monitoramento de um peer específico. Se uma conexão
    /// com o peer já não existir, cria um novo tópico no pubsub, inscreve-se nele,
    /// e inicia uma tarefa em segundo plano (`monitor_topic`) para escutar por mensagens.
    #[instrument(level = "debug", skip(self))]
    async fn connect(&mut self, target: NodeId) -> std::result::Result<(), Self::Error> {
        let id = self.get_channel_id(&target);
        let mut subs = self.subs.write().await;

        // Só executa a lógica se não estivermos já inscritos no canal com este peer.
        if let std::collections::hash_map::Entry::Vacant(e) = subs.entry(target) {
            info!(peer = %target, topic = %id, "Iniciando conexão P2P e inscrição no canal via iroh-gossip.");

            // Cria um "token filho" para esta conexão específica.
            // Quando o token principal em `self.token` for cancelado (no `close`),
            // este token filho será cancelado automaticamente em cascata.
            let child_token = self.token.child_token();

            // Subscreve ao tópico via EpidemicPubSub
            let topic = self.epidemic_pubsub.topic_subscribe(&id).await?;

            // Armazena token e tópico
            let sub_info = SubscriptionInfo {
                token: child_token.clone(),
                topic: topic.clone(),
            };
            e.insert(sub_info);

            // Clona as referências necessárias para a nova tarefa assíncrona.
            let self_clone = self.clone();

            // Inicia a tarefa em segundo plano que irá monitorar o tópico.
            tokio::spawn(async move {
                // Passa o tópico e token filho para a tarefa de monitoramento.
                self_clone.monitor_topic(topic, target, child_token).await;

                // Após o término do monitoramento (seja por cancelamento ou fim do stream),
                // remove o peer do mapa de subscrições ativas para limpeza.
                let mut subs = self_clone.subs.write().await;
                subs.remove(&target);
                debug!("Monitor para {} encerrado e removido.", target);
            });
        }

        // Libera o lock de escrita antes de continuar.
        drop(subs);

        // Nota: No Iroh, conexões P2P são estabelecidas automaticamente via discovery.
        // iroh-gossip usa Epidemic Broadcast Trees para propagação eficiente.
        debug!(peer = %target, "Canal P2P configurado via iroh-gossip. Conexão será estabelecida automaticamente.");

        // Aguarda até que o peer seja visível no tópico do pubsub.
        self.wait_for_peers(target, &id).await
    }

    /// Publica uma mensagem (slice de bytes) no canal de comunicação P2P
    /// estabelecido com o peer `p` via iroh-gossip.
    #[instrument(level = "debug", skip(self, head))]
    async fn send(&mut self, p: NodeId, head: Vec<u8>) -> std::result::Result<(), Self::Error> {
        // Obtém o tópico da subscrição ativa
        let topic = {
            let subs = self.subs.read().await;
            subs.get(&p).map(|info| info.topic.clone()).ok_or_else(|| {
                GuardianError::Other(format!(
                    "Peer {} não está conectado. Chame connect() primeiro.",
                    p
                ))
            })?
        };

        // Publica os dados no tópico via iroh-gossip
        topic.publish(head).await?;

        Ok(())
    }

    /// Encerra todas as conexões e tarefas de monitoramento ativas,
    /// limpando todos os recursos associados ao `Channels`.
    #[instrument(level = "debug", skip(self))]
    async fn close(&mut self) -> std::result::Result<(), Self::Error> {
        info!("Encerrando todos os canais e tarefas de monitoramento...");

        // Com um único chamado, cancelamos o token principal.
        // Esta ação se propaga e cancela TODOS os tokens filhos que foram
        // passados para as tarefas `monitor_topic`, sinalizando que elas
        // devem parar de forma limpa e cooperativa.
        self.token.cancel();

        // Limpa o mapa de subscrições.
        self.subs.write().await.clear();

        // Fecha o emissor de eventos.
        self.emitter.close().await?;

        Ok(())
    }

    /// Versão de close() que funciona com referência compartilhada (&self).
    /// Permite fechar o canal quando usado dentro de Arc<>.
    #[instrument(level = "debug", skip(self))]
    async fn close_shared(&self) -> std::result::Result<(), Self::Error> {
        info!("Encerrando todos os canais (referência compartilhada)...");

        // Cancela o token principal para parar todas as tarefas de monitoramento
        self.token.cancel();

        // Limpa o mapa de subscrições
        self.subs.write().await.clear();

        // Fecha o emissor de eventos
        self.emitter.close().await?;

        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// Informações de subscrição para cada peer
struct SubscriptionInfo {
    #[allow(dead_code)] // Mantido para controle de ciclo de vida
    token: CancellationToken,
    topic: Arc<dyn PubSubTopic<Error = GuardianError>>,
}

// A struct principal que gerencia os canais
#[derive(Clone)]
pub struct Channels {
    subs: Arc<RwLock<HashMap<NodeId, SubscriptionInfo>>>,
    self_id: NodeId,
    emitter: Arc<dyn DirectChannelEmitter<Error = GuardianError> + Send + Sync>,
    epidemic_pubsub: Arc<EpidemicPubSub>,
    span: Span,
    // O token principal que controla o tempo de vida de toda a instância de Channels
    token: CancellationToken,
}

impl Channels {
    /// Retorna uma referência ao span de tracing para instrumentação
    pub fn span(&self) -> &Span {
        &self.span
    }

    #[instrument(level = "debug", skip(self))]
    pub async fn connect(&self, target: NodeId) -> Result<()> {
        let _entered = self.span.enter();
        let id = self.get_channel_id(&target);
        let mut subs = self.subs.write().await;

        if let std::collections::hash_map::Entry::Vacant(e) = subs.entry(target) {
            debug!(topic = %id, "inscrevendo-se no tópico via iroh-gossip (P2P)");

            // Subscreve ao tópico via EpidemicPubSub
            let topic = self.epidemic_pubsub.topic_subscribe(&id).await?;

            let cancel_token = CancellationToken::new();

            let sub_info = SubscriptionInfo {
                token: cancel_token.clone(),
                topic: topic.clone(),
            };
            e.insert(sub_info);

            // Spawna a task para monitorar o tópico
            let self_clone = self.clone();
            tokio::spawn(async move {
                self_clone.monitor_topic(topic, target, cancel_token).await;

                // Quando monitor_topic termina, remove o peer do cache
                let mut subs = self_clone.subs.write().await;
                subs.remove(&target);
            });
        }
        // Libera o lock de escrita antes das chamadas de rede
        drop(subs);

        // Nota: No Iroh, conexões P2P são estabelecidas automaticamente via discovery.
        debug!(peer = %target, "Canal P2P configurado via iroh-gossip. Conexão será estabelecida automaticamente.");

        self.wait_for_peers(target, &id).await
    }

    #[instrument(level = "debug", skip(self, head))]
    pub async fn send(&self, p: NodeId, head: &[u8]) -> Result<()> {
        let _entered = self.span.enter();

        // Obtém o tópico da subscrição ativa
        let topic = {
            let subs = self.subs.read().await;
            subs.get(&p).map(|info| info.topic.clone()).ok_or_else(|| {
                GuardianError::Other(format!(
                    "Peer {} não está conectado. Chame connect() primeiro.",
                    p
                ))
            })?
        };

        // Publica via iroh-gossip
        topic.publish(head.to_vec()).await.map_err(|e| {
            GuardianError::Other(format!("falha ao publicar dados via iroh-gossip: {}", e))
        })?;

        Ok(())
    }

    #[instrument(level = "debug", skip(self))]
    async fn wait_for_peers(&self, other_peer: NodeId, channel_id: &str) -> Result<()> {
        // Com iroh-gossip, os peers são descobertos automaticamente via Epidemic Broadcast Trees.
        // O protocolo gossip propaga mensagens mesmo sem peers imediatamente visíveis.

        debug!(peer = %other_peer, channel = %channel_id,
               "Canal P2P via iroh-gossip configurado. Peers serão descobertos automaticamente.");

        // iroh-gossip gerencia descoberta de peers automaticamente
        Ok(())
    }

    // Função auxiliar para geração de identificadores únicos de canal.
    // Implementa lógica pura e determinística para criar IDs de canais one-on-one.
    // Garante que o mesmo ID seja gerado independente da ordem dos peers.
    #[instrument(level = "debug", skip(self))]
    fn get_channel_id(&self, p: &NodeId) -> String {
        let mut channel_id_peers = [self.self_id.to_string(), p.to_string()];
        channel_id_peers.sort();
        format!("/{}/{}", PROTOCOL, channel_id_peers.join("/"))
    }

    #[instrument(level = "debug", skip(self, topic, token))]
    async fn monitor_topic(
        &self,
        topic: Arc<dyn PubSubTopic<Error = GuardianError>>,
        p: NodeId,
        token: CancellationToken, // Recebe o token (filho)
    ) {
        // Obtém stream de mensagens do tópico via iroh-gossip
        let mut stream = match topic.watch_messages().await {
            Ok(s) => s,
            Err(e) => {
                error!("Erro ao obter stream de mensagens do tópico: {}", e);
                return;
            }
        };

        loop {
            tokio::select! {
                // Aguarda pelo sinal de cancelamento no token.
                // `biased;` pode ser usado para sempre checar o cancelamento primeiro.
                biased;
                _ = token.cancelled() => {
                    debug!(remote = %p, "fechando monitor do tópico P2P por cancelamento");
                    break;
                },

                // Processa a próxima mensagem do stream (iroh-gossip)
                maybe_msg = stream.next() => {
                    match maybe_msg {
                        Some(msg) => {
                            // Emite o payload do evento - msg.content já é Vec<u8>
                            let event = new_event_payload(msg.content, p);
                            if let Err(e) = self.emitter.emit(event).await {
                                warn!("não foi possível emitir payload do evento: {}", e);
                            }
                        },
                        // Stream finalizado
                        None => {
                             debug!(remote = %p, "stream P2P do iroh-gossip finalizado");
                             break;
                        }
                    }
                }
            }
        }
    }
}

#[instrument(level = "debug", skip(client))]
pub async fn new_channel_factory(client: Arc<IrohClient>) -> Result<DirectChannelFactory> {
    let self_id = client
        .id()
        .await
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?
        .id;

    info!(
        "ID do nó local: {} (comunicação P2P via iroh-gossip)",
        self_id
    );

    // Cria EpidemicPubSub para comunicação P2P
    let backend = client.backend().clone();
    let epidemic_pubsub = Arc::new(
        backend
            .create_pubsub_interface()
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?,
    );

    let factory = move |emitter: Arc<dyn DirectChannelEmitter<Error = GuardianError>>,
                        _opts: Option<DirectChannelOptions>| {
        let epidemic_pubsub = epidemic_pubsub.clone();
        let self_id = self_id;

        Box::pin(async move {
            // Criar um span para o canal direto
            let span = tracing::info_span!("direct_channel_p2p", self_id = %self_id);

            let ch = Arc::new(Channels {
                emitter,
                subs: Arc::new(RwLock::new(HashMap::new())),
                self_id,
                epidemic_pubsub,
                span,
                token: CancellationToken::new(),
            });

            Ok(ch as Arc<dyn DirectChannel<Error = GuardianError>>)
        })
            as Pin<
                Box<
                    dyn Future<
                            Output = std::result::Result<
                                Arc<dyn DirectChannel<Error = GuardianError>>,
                                Box<dyn std::error::Error + Send + Sync>,
                            >,
                        > + Send,
                >,
            >
    };

    Ok(Arc::new(factory))
}
