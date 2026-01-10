/// Wrapper para o protocolo iroh-docs para integração com GuardianDB
///
/// Este módulo fornece uma camada de abstração sobre o protocolo iroh-docs,
/// permitindo armazenamento chave-valor distribuído com sincronização automática
/// e resolução de conflitos Last-Write-Wins.
use crate::guardian::error::{GuardianError, Result};
use bytes::Bytes;
use iroh_docs::{AuthorId, NamespaceId, api::Doc, protocol::Docs, store::Query, sync::Entry};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

// Import do IrohBackend para acesso ao Docs
use super::IrohBackend;

/// Cliente iroh-docs que gerencia documentos e autores
///
/// Este wrapper simplifica o uso do protocolo iroh-docs para KV stores,
/// encapsulando a lógica de criação de documentos, gerenciamento de autores
/// e operações básicas de leitura/escrita.
#[derive(Clone)]
pub struct WillowDocs {
    /// Instância do protocolo Docs
    docs: Arc<Docs>,
    /// Author padrão usado para todas as operações de escrita
    default_author: Option<AuthorId>,
}

impl WillowDocs {
    /// Cria um novo cliente iroh-docs a partir do IrohBackend
    ///
    /// # Argumentos
    /// * `backend` - Referência ao IrohBackend que contém o Docs configurado
    ///
    /// # Retorna
    /// Ok(WillowDocs) sem author padrão configurado, Err se Docs não estiver inicializado
    pub async fn new(backend: Arc<IrohBackend>) -> Result<Self> {
        // Obtém Docs do backend
        let docs_lock_guard = backend.get_docs().await?;
        let docs_lock = docs_lock_guard.read().await;
        let docs = docs_lock
            .as_ref()
            .ok_or_else(|| GuardianError::Other("Docs não inicializado no backend".into()))?
            .clone();
        drop(docs_lock);

        Ok(Self {
            docs: Arc::new(docs),
            default_author: None,
        })
    }

    /// Inicializa o author padrão para este cliente
    ///
    /// Tenta usar o author padrão do sistema. Se não existir, cria um novo.
    /// Este author será usado para todas as operações de escrita.
    ///
    /// # Retorna
    /// Ok(AuthorId) se bem-sucedido, Err se falhar ao criar/obter author
    pub async fn init_default_author(&mut self) -> Result<AuthorId> {
        match self.docs.author_default().await {
            Ok(author_id) => {
                info!("Using existing default author: {:?}", author_id);
                self.default_author = Some(author_id);
                Ok(author_id)
            }
            Err(_) => {
                // Se não existe author padrão, cria um novo
                match self.docs.author_create().await {
                    Ok(author_id) => {
                        info!("Created new default author: {:?}", author_id);
                        // Tenta definir como padrão
                        if let Err(e) = self.docs.author_set_default(author_id).await {
                            warn!("Failed to set default author: {:?}", e);
                        }
                        self.default_author = Some(author_id);
                        Ok(author_id)
                    }
                    Err(e) => {
                        error!("Failed to create author: {:?}", e);
                        Err(GuardianError::Storage(format!(
                            "Failed to create author: {:?}",
                            e
                        )))
                    }
                }
            }
        }
    }

    /// Retorna o author padrão, inicializando se necessário
    ///
    /// # Retorna
    /// Ok(AuthorId) do author padrão, Err se falhar ao inicializar
    pub async fn get_or_init_author(&mut self) -> Result<AuthorId> {
        if let Some(author) = self.default_author {
            Ok(author)
        } else {
            self.init_default_author().await
        }
    }

    /// Cria um novo documento (replica)
    ///
    /// # Retorna
    /// Ok(Doc) - Handle para o documento criado
    /// Err(GuardianError) - Se falhar ao criar o documento
    pub async fn create_doc(&self) -> Result<Doc> {
        match self.docs.create().await {
            Ok(doc) => {
                info!("Created new document: {:?}", doc.id());
                Ok(doc)
            }
            Err(e) => {
                error!("Failed to create document: {:?}", e);
                Err(GuardianError::Storage(format!(
                    "Failed to create document: {:?}",
                    e
                )))
            }
        }
    }

    /// Abre um documento existente pelo NamespaceId
    ///
    /// # Argumentos
    /// * `namespace_id` - ID do namespace (documento) a ser aberto
    ///
    /// # Retorna
    /// Ok(Some(Doc)) se o documento existir, Ok(None) se não existir, Err em caso de erro
    pub async fn open_doc(&self, namespace_id: NamespaceId) -> Result<Option<Doc>> {
        match self.docs.open(namespace_id).await {
            Ok(doc_option) => {
                if let Some(ref doc) = doc_option {
                    debug!("Opened existing document: {:?}", doc.id());
                }
                Ok(doc_option)
            }
            Err(e) => {
                error!("Failed to open document: {:?}", e);
                Err(GuardianError::Storage(format!(
                    "Failed to open document: {:?}",
                    e
                )))
            }
        }
    }

    /// Fecha um documento
    ///
    /// # Argumentos
    /// * `doc` - Referência ao documento a ser fechado
    ///
    /// # Retorna
    /// Ok(()) se bem-sucedido, Err em caso de erro
    pub async fn close_doc(&self, doc: &Doc) -> Result<()> {
        match doc.close().await {
            Ok(_) => {
                debug!("Closed document: {:?}", doc.id());
                Ok(())
            }
            Err(e) => {
                error!("Failed to close document: {:?}", e);
                Err(GuardianError::Storage(format!(
                    "Failed to close document: {:?}",
                    e
                )))
            }
        }
    }

    /// Remove um documento permanentemente
    ///
    /// # Argumentos
    /// * `namespace_id` - ID do namespace (documento) a ser removido
    ///
    /// # Retorna
    /// Ok(()) se bem-sucedido, Err em caso de erro
    pub async fn drop_doc(&self, namespace_id: NamespaceId) -> Result<()> {
        match self.docs.drop_doc(namespace_id).await {
            Ok(_) => {
                info!("Dropped document: {:?}", namespace_id);
                Ok(())
            }
            Err(e) => {
                error!("Failed to drop document: {:?}", e);
                Err(GuardianError::Storage(format!(
                    "Failed to drop document: {:?}",
                    e
                )))
            }
        }
    }

    /// Define um valor para uma chave em um documento
    ///
    /// # Argumentos
    /// * `doc` - Referência ao documento
    /// * `author_id` - ID do author para esta operação
    /// * `key` - Chave (será convertida para Bytes)
    /// * `value` - Valor (será convertido para Bytes)
    ///
    /// # Retorna
    /// Ok(Hash) - Hash do conteúdo armazenado, Err em caso de erro
    pub async fn set_bytes(
        &self,
        doc: &Doc,
        author_id: AuthorId,
        key: impl Into<Bytes>,
        value: impl Into<Bytes>,
    ) -> Result<iroh_blobs::Hash> {
        match doc.set_bytes(author_id, key, value).await {
            Ok(hash) => {
                debug!("Set bytes in document {:?}: hash={:?}", doc.id(), hash);
                // Converte de iroh_docs::Hash (0.92.0) para iroh_blobs::Hash (0.94.0)
                // Ambas compartilham a mesma estrutura de hash (BLAKE3, 32 bytes), então
                // a conversão é segura através dos bytes
                let hash_bytes = hash.as_bytes();
                let result_hash = iroh_blobs::Hash::from_bytes(*hash_bytes);
                Ok(result_hash)
            }
            Err(e) => {
                error!("Failed to set bytes: {:?}", e);
                Err(GuardianError::Storage(format!(
                    "Failed to set bytes: {:?}",
                    e
                )))
            }
        }
    }

    /// Remove uma chave de um documento
    ///
    /// # Argumentos
    /// * `doc` - Referência ao documento
    /// * `author_id` - ID do author para esta operação
    /// * `key` - Chave a ser removida (prefixo)
    ///
    /// # Retorna
    /// Ok(usize) - Número de entradas deletadas, Err em caso de erro
    pub async fn del(
        &self,
        doc: &Doc,
        author_id: AuthorId,
        key: impl Into<Bytes>,
    ) -> Result<usize> {
        match doc.del(author_id, key).await {
            Ok(count) => {
                debug!("Deleted {} entries from document {:?}", count, doc.id());
                Ok(count)
            }
            Err(e) => {
                error!("Failed to delete: {:?}", e);
                Err(GuardianError::Storage(format!("Failed to delete: {:?}", e)))
            }
        }
    }

    /// Obtém uma única entrada de um documento
    ///
    /// # Argumentos
    /// * `doc` - Referência ao documento
    /// * `query` - Query para buscar a entrada
    ///
    /// # Retorna
    /// Ok(Some(Entry)) se encontrado, Ok(None) se não encontrado, Err em caso de erro
    pub async fn get_one(&self, doc: &Doc, query: impl Into<Query>) -> Result<Option<Entry>> {
        match doc.get_one(query).await {
            Ok(entry_option) => {
                if let Some(ref entry) = entry_option {
                    debug!(
                        "Got entry from document {:?}: key={:?}",
                        doc.id(),
                        String::from_utf8_lossy(entry.key())
                    );
                }
                Ok(entry_option)
            }
            Err(e) => {
                error!("Failed to get entry: {:?}", e);
                Err(GuardianError::Storage(format!(
                    "Failed to get entry: {:?}",
                    e
                )))
            }
        }
    }

    /// Obtém múltiplas entradas de um documento usando uma query
    ///
    /// # Argumentos
    /// * `doc` - Referência ao documento
    /// * `query` - Query para filtrar as entradas
    ///
    /// # Retorna
    /// Ok(Vec<Entry>) - Lista de entradas encontradas, Err em caso de erro
    pub async fn get_many(&self, doc: &Doc, query: impl Into<Query>) -> Result<Vec<Entry>> {
        use futures::StreamExt;

        match doc.get_many(query).await {
            Ok(stream) => {
                let entries: Vec<Entry> = stream
                    .filter_map(|result| async move {
                        match result {
                            Ok(entry) => Some(entry),
                            Err(e) => {
                                warn!("Error reading entry from stream: {:?}", e);
                                None
                            }
                        }
                    })
                    .collect()
                    .await;

                debug!("Got {} entries from document {:?}", entries.len(), doc.id());
                Ok(entries)
            }
            Err(e) => {
                error!("Failed to get entries: {:?}", e);
                Err(GuardianError::Storage(format!(
                    "Failed to get entries: {:?}",
                    e
                )))
            }
        }
    }

    /// Retorna a instância Docs subjacente para operações avançadas
    pub fn docs(&self) -> &Arc<Docs> {
        &self.docs
    }

    /// Retorna o AuthorId padrão (sem inicializar se não existir)
    ///
    /// # Retorna
    /// Ok(AuthorId) se existe um author padrão, Err caso contrário
    pub async fn default_author_id(&self) -> Result<AuthorId> {
        if let Some(author) = self.default_author {
            Ok(author)
        } else {
            // Tenta obter o author padrão do docs
            self.docs.author_default().await.map_err(|e| {
                GuardianError::Storage(format!("No default author configured: {:?}", e))
            })
        }
    }
}

#[cfg(test)]
mod tests {
    // Note: Testes completos requerem um ambiente Iroh configurado
    // Estes são testes de unidade básicos para verificar a interface

    #[tokio::test]
    async fn test_client_creation() {
        // Teste básico de criação - requer Docs mock ou skip
        // Este teste serve como documentação da API esperada
    }
}
