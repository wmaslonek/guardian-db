use crate::error::{GuardianError, Result};
use crate::iface::{
    DirectChannel, DirectChannelEmitter, DirectChannelFactory, DirectChannelOptions,
};
use crate::ipfs_core_api::{IpfsClient, PubsubStream};
use crate::pubsub::event::new_event_payload;
use async_trait::async_trait;
use futures::stream::StreamExt;
use libp2p::PeerId;
use slog::Logger;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

// Constantes do protocolo
const PROTOCOL: &str = "ipfs-pubsub-direct-channel/v1";

// Em Rust, a verificação de interface é feita no bloco `impl`.
// Aqui implementamos a trait `DirectChannel` para a nossa struct `Channels`.
#[async_trait]
impl DirectChannel for Channels {
    type Error = GuardianError;

    /// equivalente a Connect em go
    ///
    /// Inicia a conexão e o monitoramento de um peer específico. Se uma conexão
    /// com o peer já não existir, cria um novo tópico no pubsub, inscreve-se nele,
    /// e inicia uma tarefa em segundo plano (`monitor_topic`) para escutar por mensagens.
    async fn connect(&mut self, target: PeerId) -> std::result::Result<(), Self::Error> {
        let id = self.get_channel_id(&target);
        let mut subs = self.subs.write().await;

        // Só executa a lógica se não estivermos já inscritos no canal com este peer.
        if !subs.contains_key(&target) {
            info!(logger = ?self.logger, peer = %target, topic = %id, "Iniciando conexão e inscrição no canal.");

            // Cria um "token filho" para esta conexão específica.
            // Quando o token principal em `self.token` for cancelado (no `close`),
            // este token filho será cancelado automaticamente em cascata.
            let child_token = self.token.child_token();

            // Inscreve-se no tópico do pubsub através da nossa API IPFS 100% Rust.
            let stream = self.ipfs_client.pubsub_subscribe(&id).await?;

            // Armazena o token da nova subscrição no mapa.
            subs.insert(target, child_token.clone());

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
                debug!(logger = ?&self_clone.logger, "Monitor para {} encerrado e removido.", target);
            });
        }

        // Libera o lock de escrita antes de continuar.
        drop(subs);

        // Tenta se conectar diretamente ao peer na camada de swarm do IPFS.
        // A falha aqui é apenas um aviso, pois o pubsub pode funcionar mesmo sem uma conexão direta.
        if let Err(e) = self.ipfs_client.swarm_connect(&target).await {
            warn!(logger = ?self.logger, peer = %target, "Não foi possível conectar diretamente ao peer (aviso): {}", e);
        }

        // Aguarda até que o peer seja visível no tópico do pubsub.
        self.wait_for_peers(target, &id).await
    }

    /// equivalente a Send em go
    ///
    /// Publica uma mensagem (slice de bytes) no canal de comunicação
    /// estabelecido com o peer `p`.
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

    /// equivalente a Close em go
    ///
    /// Encerra todas as conexões e tarefas de monitoramento ativas,
    /// limpando todos os recursos associados ao `Channels`.
    async fn close(&mut self) -> std::result::Result<(), Self::Error> {
        info!(logger = ?self.logger, "Encerrando todos os canais e tarefas de monitoramento...");

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
}

// A struct principal que gerencia os canais
#[derive(Clone)]
pub struct Channels {
    subs: Arc<RwLock<HashMap<PeerId, CancellationToken>>>,
    self_id: PeerId,
    emitter: Arc<dyn DirectChannelEmitter<Error = GuardianError> + Send + Sync>,
    ipfs_client: Arc<IpfsClient>,
    logger: Arc<Logger>,
    // O token principal que controla o tempo de vida de toda a instância de Channels
    token: CancellationToken,
}

// equivalente a Connect em go
impl Channels {
    pub async fn connect(&self, target: PeerId) -> Result<()> {
        let id = self.get_channel_id(&target);
        let mut subs = self.subs.write().await;

        if !subs.contains_key(&target) {
            debug!(logger = ?self.logger, topic = %id, "inscrevendo-se no tópico");

            // Substitui c.ipfs.PubSub().Subscribe(ctx, id, options.PubSub.Discover(true))
            // A chamada é realizada via nossa API IPFS 100% Rust.
            // A sua gestão será feita dentro de `monitor_topic`.
            let stream = self.ipfs_client.pubsub_subscribe(&id).await?;

            let cancel_token = CancellationToken::new();

            subs.insert(target, cancel_token.clone());

            // Spawna a task para monitorar o tópico, equivalente à goroutine
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

        // Substitui c.ipfs.Swarm().Connect(ctx, peer.AddrInfo{ID: target})
        if let Err(e) = self.ipfs_client.swarm_connect(&target).await {
            warn!(logger = ?self.logger, peer = %target, "não foi possível conectar ao peer remoto: {}", e);
        }

        self.wait_for_peers(target, &id).await
    }

    // equivalente a Send em go
    pub async fn send(&self, p: PeerId, head: &[u8]) -> Result<()> {
        let id = {
            let subs = self.subs.read().await;
            if subs.contains_key(&p) {
                self.get_channel_id(&p)
            } else {
                self.get_channel_id(&p)
            }
        };

        // Substitui c.ipfs.PubSub().Publish(ctx, id, head)
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

    // equivalente a waitForPeers em go
    async fn wait_for_peers(&self, other_peer: PeerId, channel_id: &str) -> Result<()> {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        // Adiciona um timeout para não ficar em loop infinito
        let timeout = tokio::time::sleep(Duration::from_secs(30));
        tokio::pin!(timeout);

        loop {
            tokio::select! {
                _ = &mut timeout => {
                     return Err(GuardianError::Other(format!("timeout esperando pelo peer {} no canal {}", other_peer, channel_id).into()));
                }
                _ = interval.tick() => {
                    // Substitui c.ipfs.PubSub().Peers(ctx, options.PubSub.Topic(channelID))
                    match self.ipfs_client.pubsub_peers(channel_id).await {
                        Ok(peers) => {
                            if peers.iter().any(|p| p == &other_peer) {
                                debug!(logger = ?self.logger, "peer {} encontrado no pubsub", other_peer);
                                return Ok(());
                            }
                            debug!(logger = ?self.logger, "Peer não encontrado, tentando novamente...");
                        }
                        Err(e) => {
                            error!(logger = ?self.logger, "falha ao obter peers do pubsub: {}", e);
                            // Retorna o erro em caso de falha na chamada da nossa API IPFS
                            return Err(e.into());
                        }
                    }
                }
            }
        }
    }

    // Função auxiliar para geração de identificadores únicos de canal.
    // Implementa lógica pura e determinística para criar IDs de canais one-on-one.
    // Garante que o mesmo ID seja gerado independente da ordem dos peers.
    // equivalente a getChannelID em go
    fn get_channel_id(&self, p: &PeerId) -> String {
        let mut channel_id_peers = [self.self_id.to_string(), p.to_string()];
        channel_id_peers.sort();
        format!("/{}/{}", PROTOCOL, channel_id_peers.join("/"))
    }

    // equivalente a monitorTopic em go
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
                    debug!(logger = ?self.logger, remote = %p, "fechando monitor do tópico por cancelamento");
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
                                warn!(logger = ?self.logger, "não foi possível emitir payload do evento: {}", e);
                            }
                        },
                        Some(Err(e)) => {
                            error!(logger = ?self.logger, "erro no stream do pubsub: {}", e);
                            break;
                        },
                        // Stream finalizado
                        None => {
                             debug!(logger = ?self.logger, remote = %p, "stream do pubsub finalizado");
                             break;
                        }
                    }
                }
            }
        }
    }
}

// equivalente a NewChannelFactory em go
pub async fn new_channel_factory(ipfs_client: Arc<IpfsClient>) -> Result<DirectChannelFactory> {
    // Substitui ipfs.Key().Self(ctx)
    let self_id = ipfs_client
        .id()
        .await
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?
        .id;

    info!("ID do nó local: {}", self_id);

    let factory = move |emitter: Arc<dyn DirectChannelEmitter<Error = GuardianError>>,
                        opts: Option<DirectChannelOptions>| {
        let ipfs_client = ipfs_client.clone();
        let self_id = self_id.clone();

        Box::pin(async move {
            let opts = opts.unwrap_or_default();

            // Criar um logger simples - usando logger básico em vez de slog complexo
            let logger = opts.logger.unwrap_or_else(|| {
                use slog::o;
                let drain = slog::Discard;
                slog::Logger::root(drain, o!())
            });

            let ch = Arc::new(Channels {
                emitter,
                subs: Arc::new(RwLock::new(HashMap::new())),
                self_id,
                ipfs_client,
                logger: Arc::new(logger),
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

    Ok(Box::new(factory))
}
