use crate::error::{GuardianError, Result};
use crate::iface::{
    DirectChannel, DirectChannelEmitter, DirectChannelFactory, DirectChannelOptions,
};
use crate::ipfs_core_api::{IpfsClient, PubsubStream};
use crate::pubsub::event::new_event_payload;
use async_trait::async_trait;
use futures::stream::StreamExt;
use libp2p::PeerId;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{Span, debug, error, info, instrument, warn};

// Constantes do protocolo
const PROTOCOL: &str = "ipfs-pubsub-direct-channel/v1";

// Implemetação da trait `DirectChannel` para a struct `Channels`.
#[async_trait]
impl DirectChannel for Channels {
    type Error = GuardianError;

    /// Inicia a conexão e o monitoramento de um peer específico. Se uma conexão
    /// com o peer já não existir, cria um novo tópico no pubsub, inscreve-se nele,
    /// e inicia uma tarefa em segundo plano (`monitor_topic`) para escutar por mensagens.
    #[instrument(level = "debug", skip(self))]
    async fn connect(&mut self, target: PeerId) -> std::result::Result<(), Self::Error> {
        let id = self.get_channel_id(&target);
        let mut subs = self.subs.write().await;

        // Só executa a lógica se não estivermos já inscritos no canal com este peer.
        if let std::collections::hash_map::Entry::Vacant(e) = subs.entry(target) {
            info!(peer = %target, topic = %id, "Iniciando conexão e inscrição no canal.");

            // Cria um "token filho" para esta conexão específica.
            // Quando o token principal em `self.token` for cancelado (no `close`),
            // este token filho será cancelado automaticamente em cascata.
            let child_token = self.token.child_token();

            // Inscreve-se no tópico do pubsub através da nossa API IPFS 100% Rust.
            let stream = self.ipfs_client.pubsub_subscribe(&id).await?;

            // Armazena o token da nova subscrição no mapa.
            e.insert(child_token.clone());

            // Clona as referências necessárias para a nova tarefa assíncrona.
            let self_clone = self.clone();

            // Inicia a tarefa em segundo plano que irá monitorar o tópico.
            tokio::spawn(async move {
                // Passa o token filho para a tarefa de monitoramento.
                self_clone.monitor_topic(stream, target, child_token).await;

                // Após o término do monitoramento (seja por cancelamento ou fim do stream),
                // remove o peer do mapa de subscrições ativas para limpeza.
                let mut subs = self_clone.subs.write().await;
                subs.remove(&target);
                debug!("Monitor para {} encerrado e removido.", target);
            });
        }

        // Libera o lock de escrita antes de continuar.
        drop(subs);

        // Tenta se conectar diretamente ao peer na camada de swarm do IPFS.
        // A falha aqui é apenas um aviso, pois o pubsub pode funcionar mesmo sem uma conexão direta.
        if let Err(e) = self.ipfs_client.swarm_connect(&target).await {
            warn!(peer = %target, "Não foi possível conectar diretamente ao peer (aviso): {}", e);
        }

        // Aguarda até que o peer seja visível no tópico do pubsub.
        self.wait_for_peers(target, &id).await
    }

    /// Publica uma mensagem (slice de bytes) no canal de comunicação
    /// estabelecido com o peer `p`.
    #[instrument(level = "debug", skip(self, head))]
    async fn send(&mut self, p: PeerId, head: Vec<u8>) -> std::result::Result<(), Self::Error> {
        // Determina o ID do canal. Se já tivermos uma subscrição ativa,
        // reutiliza o ID do canal armazenado. Caso contrário, calcula-o.
        let id = {
            let subs = self.subs.read().await;
            if subs.contains_key(&p) {
                // Lógica para obter o ID do canal associado ao token, se necessário.
                // Neste caso, é mais simples recalcular.
                self.get_channel_id(&p)
            } else {
                self.get_channel_id(&p)
            }
        };

        // Publica os dados no tópico do pubsub via nossa API IPFS 100% Rust.
        self.ipfs_client.pubsub_publish(&id, &head).await?;

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

// A struct principal que gerencia os canais
#[derive(Clone)]
pub struct Channels {
    subs: Arc<RwLock<HashMap<PeerId, CancellationToken>>>,
    self_id: PeerId,
    emitter: Arc<dyn DirectChannelEmitter<Error = GuardianError> + Send + Sync>,
    ipfs_client: Arc<IpfsClient>,
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
    pub async fn connect(&self, target: PeerId) -> Result<()> {
        let _entered = self.span.enter();
        let id = self.get_channel_id(&target);
        let mut subs = self.subs.write().await;

        if let std::collections::hash_map::Entry::Vacant(e) = subs.entry(target) {
            debug!(topic = %id, "inscrevendo-se no tópico");

            // A chamada é realizada via API IPFS.
            // A sua gestão será feita dentro de `monitor_topic`.
            let stream = self.ipfs_client.pubsub_subscribe(&id).await?;

            let cancel_token = CancellationToken::new();

            e.insert(cancel_token.clone());

            // Spawna a task para monitorar o tópico
            let self_clone = self.clone();
            tokio::spawn(async move {
                self_clone.monitor_topic(stream, target, cancel_token).await;

                // Quando monitor_topic termina, remove o peer do cache
                let mut subs = self_clone.subs.write().await;
                subs.remove(&target);
            });
        }
        // Libera o lock de escrita antes das chamadas de rede
        drop(subs);

        if let Err(e) = self.ipfs_client.swarm_connect(&target).await {
            warn!(peer = %target, "não foi possível conectar ao peer remoto: {}", e);
        }

        self.wait_for_peers(target, &id).await
    }

    #[instrument(level = "debug", skip(self, head))]
    pub async fn send(&self, p: PeerId, head: &[u8]) -> Result<()> {
        let _entered = self.span.enter();
        let id = {
            let _subs = self.subs.read().await;
            self.get_channel_id(&p)
        };

        self.ipfs_client
            .pubsub_publish(&id, head)
            .await
            .map_err(|e| {
                GuardianError::Other(format!(
                    "falha ao publicar dados no pubsub via nossa API IPFS: {}",
                    e
                ))
            })?;

        Ok(())
    }

    #[instrument(level = "debug", skip(self))]
    async fn wait_for_peers(&self, other_peer: PeerId, channel_id: &str) -> Result<()> {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        // Adiciona um timeout para não ficar em loop infinito
        let timeout = tokio::time::sleep(Duration::from_secs(30));
        tokio::pin!(timeout);

        loop {
            tokio::select! {
                _ = &mut timeout => {
                     return Err(GuardianError::Network(format!("timeout esperando pelo peer {} no canal {}", other_peer, channel_id)));
                }
                _ = interval.tick() => {
                    match self.ipfs_client.pubsub_peers(channel_id).await {
                        Ok(peers) => {
                            if peers.iter().any(|p| p == &other_peer) {
                                debug!("peer {} encontrado no pubsub", other_peer);
                                return Ok(());
                            }
                            debug!("Peer não encontrado, tentando novamente...");
                        }
                        Err(e) => {
                            error!("falha ao obter peers do pubsub: {}", e);
                            // Retorna o erro em caso de falha na chamada da nossa API IPFS
                            return Err(e);
                        }
                    }
                }
            }
        }
    }

    // Função auxiliar para geração de identificadores únicos de canal.
    // Implementa lógica pura e determinística para criar IDs de canais one-on-one.
    // Garante que o mesmo ID seja gerado independente da ordem dos peers.
    #[instrument(level = "debug", skip(self))]
    fn get_channel_id(&self, p: &PeerId) -> String {
        let mut channel_id_peers = [self.self_id.to_string(), p.to_string()];
        channel_id_peers.sort();
        format!("/{}/{}", PROTOCOL, channel_id_peers.join("/"))
    }

    #[instrument(level = "debug", skip(self, stream, token))]
    async fn monitor_topic(
        &self,
        mut stream: PubsubStream,
        p: PeerId,
        token: CancellationToken, // Recebe o token (filho)
    ) {
        loop {
            tokio::select! {
                // Aguarda pelo sinal de cancelamento no token.
                // `biased;` pode ser usado para sempre checar o cancelamento primeiro.
                biased;
                _ = token.cancelled() => {
                    debug!(remote = %p, "fechando monitor do tópico por cancelamento");
                    break;
                },

                // Processa a próxima mensagem do stream
                maybe_msg = stream.next() => {
                    match maybe_msg {
                        Some(Ok(msg)) => {
                            // Garante que a mensagem venha de um peer diferente do self
                            if msg.from == self.self_id {
                                continue;
                            }

                            // Emite o payload do evento - mantém dados como Vec<u8>
                            let event = new_event_payload(msg.data, p);
                            if let Err(e) = self.emitter.emit(event).await {
                                warn!("não foi possível emitir payload do evento: {}", e);
                            }
                        },
                        Some(Err(e)) => {
                            error!("erro no stream do pubsub: {}", e);
                            break;
                        },
                        // Stream finalizado
                        None => {
                             debug!(remote = %p, "stream do pubsub finalizado");
                             break;
                        }
                    }
                }
            }
        }
    }
}

#[instrument(level = "debug", skip(ipfs_client))]
pub async fn new_channel_factory(ipfs_client: Arc<IpfsClient>) -> Result<DirectChannelFactory> {
    let self_id = ipfs_client
        .id()
        .await
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?
        .id;

    info!("ID do nó local: {}", self_id);

    let factory = move |emitter: Arc<dyn DirectChannelEmitter<Error = GuardianError>>,
                        _opts: Option<DirectChannelOptions>| {
        let ipfs_client = ipfs_client.clone();
        let self_id = self_id;

        Box::pin(async move {
            // Criar um span para o canal direto
            let span = tracing::info_span!("direct_channel", self_id = %self_id);

            let ch = Arc::new(Channels {
                emitter,
                subs: Arc::new(RwLock::new(HashMap::new())),
                self_id,
                ipfs_client,
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
