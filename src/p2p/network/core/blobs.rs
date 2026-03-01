/// Cliente wrapper para iroh-blobs
///
/// Fornece uma interface simplificada para operações de blob storage
/// content-addressed usando BLAKE3 hashes.
///
/// Este cliente utiliza o store compartilhado do IrohBackend, garantindo
/// consistência e evitando duplicação de armazenamento.
use crate::guardian::error::{GuardianError, Result};
use bytes::Bytes;
use futures::StreamExt;
use iroh::NodeId;
use iroh::endpoint::Endpoint;
use iroh_blobs::{Hash as BlobHash, HashAndFormat, store::fs::FsStore};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, instrument, warn};

/// Cliente para operações com iroh-blobs
///
/// Suporta operações locais e download P2P de blobs de peers remotos
/// quando o Endpoint está configurado.
#[derive(Clone)]
pub struct BlobStore {
    /// Store do iroh-blobs compartilhado (filesystem-based)
    store: Arc<RwLock<FsStore>>,
    /// Endpoint do Iroh para download P2P de blobs (opcional)
    endpoint: Option<Endpoint>,
}

impl BlobStore {
    /// Cria uma nova instância do cliente iroh-blobs usando store compartilhado
    ///
    /// # Argumentos
    /// * `store` - Store compartilhado do IrohBackend
    ///
    /// # Exemplo
    /// ```no_run
    /// use std::sync::Arc;
    /// use tokio::sync::RwLock;
    /// use iroh_blobs::store::fs::FsStore;
    /// use guardian_db::p2p::network::core::BlobStore;
    ///
    /// # async fn example(fs_store: FsStore) {
    /// let store = Arc::new(RwLock::new(fs_store));
    /// let blobs_client = BlobStore::new(store);
    /// # }
    /// ```
    #[instrument(level = "debug", skip(store))]
    pub fn new(store: Arc<RwLock<FsStore>>) -> Self {
        debug!("Criando BlobStore com store compartilhado (sem P2P download)");
        Self {
            store,
            endpoint: None,
        }
    }

    /// Cria uma nova instância com suporte a download P2P via Endpoint
    ///
    /// O Endpoint permite baixar blobs de peers remotos usando o protocolo
    /// iroh-blobs nativo (QUIC + BLAKE3 verified streaming).
    #[instrument(level = "debug", skip(store, endpoint))]
    pub fn new_with_endpoint(store: Arc<RwLock<FsStore>>, endpoint: Endpoint) -> Self {
        debug!("Criando BlobStore com store compartilhado + P2P download");
        Self {
            store,
            endpoint: Some(endpoint),
        }
    }

    /// Adiciona um documento (bytes) ao blob store
    ///
    /// Retorna o Hash BLAKE3 do conteúdo armazenado.
    #[instrument(level = "debug", skip(self, data))]
    pub async fn add_document(&self, data: Bytes) -> Result<BlobHash> {
        let store = self.store.read().await;

        // Adiciona bytes ao store usando nova API
        let outcome = store.blobs().add_bytes(data.clone()).await.map_err(|e| {
            GuardianError::Other(format!("Erro ao adicionar bytes ao blob store: {}", e))
        })?;

        let hash = outcome.hash;

        // Cria tag permanente para proteger contra GC
        // Formato: doc_<hash_hex>
        let tag_name = format!("doc_{}", hex::encode(hash.as_bytes()));

        store
            .tags()
            .set(tag_name.as_bytes(), HashAndFormat::raw(hash))
            .await
            .map_err(|e| GuardianError::Other(format!("Erro ao criar tag permanente: {}", e)))?;

        debug!(
            "Documento adicionado ao blob store: {} ({} bytes)",
            hex::encode(hash.as_bytes()),
            data.len()
        );

        Ok(hash)
    }

    /// Recupera um documento do blob store pelo hash
    #[instrument(level = "debug", skip(self))]
    pub async fn get_document(&self, hash: &BlobHash) -> Result<Bytes> {
        let store = self.store.read().await;

        // Usa nova API: blobs().get_bytes() - requer Hash owned
        let data = store
            .blobs()
            .get_bytes(*hash)
            .await
            .map_err(|e| GuardianError::Other(format!("Erro ao buscar blob: {}", e)))?;

        debug!(
            "Documento recuperado do blob store: {} ({} bytes)",
            hex::encode(hash.as_bytes()),
            data.len()
        );

        Ok(data)
    }

    /// Recupera um documento do blob store, tentando download P2P se não encontrado localmente
    ///
    /// Se o blob não existe no store local e um peer provider é fornecido,
    /// tenta baixar do peer remoto usando o protocolo iroh-blobs.
    #[instrument(level = "debug", skip(self))]
    pub async fn get_or_download(&self, hash: &BlobHash, providers: &[NodeId]) -> Result<Bytes> {
        // Tenta buscar localmente primeiro
        let store = self.store.read().await;
        match store.blobs().get_bytes(*hash).await {
            Ok(data) => {
                debug!(
                    "Documento encontrado localmente: {} ({} bytes)",
                    hex::encode(hash.as_bytes()),
                    data.len()
                );
                return Ok(data);
            }
            Err(_) => {
                debug!(
                    "Documento não encontrado localmente: {}, tentando P2P download",
                    hex::encode(hash.as_bytes())
                );
            }
        }
        drop(store);

        // Tenta download P2P
        self.download_from_peers(hash, providers).await?;

        // Agora busca do store local (deve estar lá após download)
        let store = self.store.read().await;
        let data = store.blobs().get_bytes(*hash).await.map_err(|e| {
            GuardianError::Other(format!("Blob não encontrado após download P2P: {}", e))
        })?;

        // Cria tag permanente para proteger contra GC
        let tag_name = format!("doc_{}", hex::encode(hash.as_bytes()));
        store
            .tags()
            .set(tag_name.as_bytes(), HashAndFormat::raw(*hash))
            .await
            .ok();

        debug!(
            "Documento baixado via P2P: {} ({} bytes)",
            hex::encode(hash.as_bytes()),
            data.len()
        );

        Ok(data)
    }

    /// Baixa um blob de peers remotos usando o Downloader do iroh-blobs
    #[instrument(level = "debug", skip(self))]
    pub async fn download_from_peers(&self, hash: &BlobHash, providers: &[NodeId]) -> Result<()> {
        let endpoint = self.endpoint.as_ref().ok_or_else(|| {
            GuardianError::Other("Endpoint não disponível para download P2P de blobs".to_string())
        })?;

        if providers.is_empty() {
            return Err(GuardianError::Other(
                "Nenhum provider fornecido para download P2P".to_string(),
            ));
        }

        let store = self.store.read().await;
        let downloader = store.downloader(endpoint);

        let providers_vec: Vec<NodeId> = providers.to_vec();
        info!(
            "Iniciando download P2P do blob {} de {} provider(s)",
            hex::encode(hash.as_bytes()),
            providers_vec.len()
        );

        let progress = downloader.download(*hash, providers_vec);
        let mut stream = progress
            .stream()
            .await
            .map_err(|e| GuardianError::Other(format!("Erro ao iniciar download P2P: {}", e)))?;

        while let Some(item) = stream.next().await {
            match &item {
                iroh_blobs::api::downloader::DownloadProgressItem::Error(e) => {
                    return Err(GuardianError::Other(format!("Erro no download P2P: {}", e)));
                }
                iroh_blobs::api::downloader::DownloadProgressItem::DownloadError => {
                    return Err(GuardianError::Other("Download P2P falhou".to_string()));
                }
                iroh_blobs::api::downloader::DownloadProgressItem::PartComplete { .. } => {
                    debug!("Download P2P: parte concluída");
                }
                iroh_blobs::api::downloader::DownloadProgressItem::Progress(bytes) => {
                    debug!("Download P2P: {} bytes recebidos", bytes);
                }
                _ => {}
            }
        }

        info!("Download P2P concluído: {}", hex::encode(hash.as_bytes()));
        Ok(())
    }

    /// Verifica se um documento existe no blob store
    #[instrument(level = "debug", skip(self))]
    pub async fn has_document(&self, hash: &BlobHash) -> Result<bool> {
        let store = self.store.read().await;

        // Usa nova API: blobs().has() - requer Hash owned
        let has_blob = store.blobs().has(*hash).await.unwrap_or(false);

        Ok(has_blob)
    }

    /// Deleta um documento do blob store
    ///
    /// Remove a tag de proteção e opcionalmente deleta o blob físico.
    #[instrument(level = "debug", skip(self))]
    pub async fn delete_document(&self, hash: &BlobHash) -> Result<()> {
        let store = self.store.read().await;

        // Remove tag de proteção
        let tag_name = format!("doc_{}", hex::encode(hash.as_bytes()));

        store
            .tags()
            .delete(tag_name.as_bytes())
            .await
            .map_err(|e| {
                warn!("Erro ao deletar tag de documento: {}", e);
                GuardianError::Other(format!("Erro ao deletar tag: {}", e))
            })?;

        // Nota: O blob físico será removido pelo GC quando não houver
        // mais referências. Isso evita deleção acidental de blobs compartilhados.

        debug!(
            "Tag de documento removida: {}",
            hex::encode(hash.as_bytes())
        );

        Ok(())
    }

    /// Lista todos os documentos tagueados no blob store
    ///
    /// Retorna pares (hash, tamanho) para todos os documentos.
    #[instrument(level = "debug", skip(self))]
    pub async fn list_documents(&self) -> Result<Vec<(BlobHash, u64)>> {
        use futures::stream::StreamExt;

        let store = self.store.read().await;
        let mut documents = Vec::new();

        // Usa nova API: tags().list_prefix() para listar tags com prefixo "doc_"
        let mut tags_stream = store
            .tags()
            .list_prefix(b"doc_")
            .await
            .map_err(|e| GuardianError::Other(format!("Erro ao obter tags: {}", e)))?;

        while let Some(tag_result) = tags_stream.next().await {
            match tag_result {
                Ok(tag_info) => {
                    let hash = tag_info.hash;
                    // Retorna size 0 por enquanto - API do iroh-blobs não fornece fácil acesso ao size
                    documents.push((hash, 0));
                }
                Err(e) => {
                    warn!("Erro ao processar tag durante listagem: {}", e);
                }
            }
        }

        debug!("Listados {} documentos no blob store", documents.len());

        Ok(documents)
    }

    /// Executa garbage collection manual
    ///
    /// Remove blobs não referenciados por nenhuma tag.
    #[instrument(level = "debug", skip(self))]
    pub async fn gc(&self) -> Result<u64> {
        use futures::stream::StreamExt;

        let store = self.store.read().await;

        // Coleta todos os hashes protegidos por tags
        let mut protected_hashes = std::collections::BTreeSet::new();
        let mut tags_stream = store
            .tags()
            .list()
            .await
            .map_err(|e| GuardianError::Other(format!("Erro ao obter tags para GC: {}", e)))?;

        while let Some(tag_result) = tags_stream.next().await {
            if let Ok(tag_info) = tag_result {
                protected_hashes.insert(tag_info.hash);
            }
        }

        debug!("GC: {} hashes protegidos por tags", protected_hashes.len());

        // NOTA: A API 0.94.0 gerencia GC automaticamente via FsStore
        // GC manual não é exposto diretamente na nova API
        // O GC roda periodicamente em background

        debug!("GC é gerenciado automaticamente pelo FsStore");

        Ok(0) // Retorna 0 pois GC é automático
    }

    /// Retorna true se o BlobStore tem suporte a download P2P
    pub fn has_p2p_support(&self) -> bool {
        self.endpoint.is_some()
    }

    /// Cria uma instância de teste com store temporário
    #[cfg(test)]
    pub async fn memory() -> Result<Self> {
        // Cria um diretório temporário
        let temp_dir =
            std::env::temp_dir().join(format!("iroh-blobs-test-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&temp_dir).await.map_err(|e| {
            GuardianError::Other(format!("Erro ao criar diretório temporário: {}", e))
        })?;

        // Carrega FsStore no diretório temporário
        let store = FsStore::load(&temp_dir)
            .await
            .map_err(|e| GuardianError::Other(format!("Erro ao criar store temporário: {}", e)))?;

        Ok(Self::new(Arc::new(RwLock::new(store))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_add_and_get_document() {
        let blobs_client = BlobStore::memory().await.unwrap();

        let data = Bytes::from("Hello, iroh-blobs!");
        let hash = blobs_client.add_document(data.clone()).await.unwrap();

        let retrieved = blobs_client.get_document(&hash).await.unwrap();
        assert_eq!(data, retrieved);
    }

    #[tokio::test]
    async fn test_has_document() {
        let blobs_client = BlobStore::memory().await.unwrap();

        let data = Bytes::from("Test data");
        let hash = blobs_client.add_document(data).await.unwrap();

        assert!(blobs_client.has_document(&hash).await.unwrap());
    }

    #[tokio::test]
    async fn test_delete_document() {
        let blobs_client = BlobStore::memory().await.unwrap();

        let data = Bytes::from("To be deleted");
        let hash = blobs_client.add_document(data).await.unwrap();

        blobs_client.delete_document(&hash).await.unwrap();

        // Após deletar a tag, o GC pode remover o blob
        // Mas imediatamente após delete_document, ainda pode existir
        // até o GC rodar
    }

    #[tokio::test]
    async fn test_list_documents() {
        let blobs_client = BlobStore::memory().await.unwrap();

        let data1 = Bytes::from("Document 1");
        let data2 = Bytes::from("Document 2");

        blobs_client.add_document(data1).await.unwrap();
        blobs_client.add_document(data2).await.unwrap();

        let docs = blobs_client.list_documents().await.unwrap();
        assert_eq!(docs.len(), 2);
    }
}
