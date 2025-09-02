use crate::error::Result as DbResult;
use std::fmt::{Display, Formatter, Result as FmtResult};

/// Trait principal para operações de datastore
///
/// Fornece uma interface assíncrona para operações CRUD básicas
/// e queries com filtros avançados.
#[async_trait::async_trait]
pub trait Datastore: Send + Sync + std::any::Any {
    /// Recupera um valor associado à chave
    async fn get(&self, key: &[u8]) -> DbResult<Option<Vec<u8>>>;

    /// Armazena um valor com a chave especificada
    async fn put(&self, key: &[u8], value: &[u8]) -> DbResult<()>;

    /// Verifica se uma chave existe no datastore
    async fn has(&self, key: &[u8]) -> DbResult<bool>;

    /// Remove uma chave e seu valor do datastore
    async fn delete(&self, key: &[u8]) -> DbResult<()>;

    /// Executa uma query com filtros e retorna resultados paginados
    async fn query(&self, query: &Query) -> DbResult<Results>;

    /// Retorna todas as chaves com um determinado prefixo
    async fn list_keys(&self, prefix: &[u8]) -> DbResult<Vec<Key>>;

    /// Método auxiliar para downcast
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Representa uma chave hierárquica no datastore
///
/// Permite navegar por uma estrutura de diretórios com operações
/// de pai/filho e conversões para diferentes formatos.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Key {
    segments: Vec<String>,
}

impl Key {
    /// Cria uma nova chave a partir de um caminho
    ///
    /// # Exemplos
    /// ```
    /// let key = Key::new("/users/alice/profile");
    /// let key = Key::new("config/database/host");
    /// ```
    pub fn new<S: Into<String>>(path: S) -> Self {
        let s = path.into();
        let segments = s
            .split('/')
            .filter(|p| !p.is_empty())
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty()) // Remove segmentos vazios após trim
            .collect();
        Self { segments }
    }

    /// Cria uma chave raiz vazia
    #[allow(dead_code)]
    pub fn root() -> Self {
        Self { segments: vec![] }
    }

    /// Cria uma chave filha adicionando um segmento
    pub fn child<S: Into<String>>(&self, name: S) -> Self {
        let child_name = name.into().trim().to_string();
        if child_name.is_empty() {
            return self.clone();
        }

        let mut segs = self.segments.clone();
        segs.push(child_name);
        Self { segments: segs }
    }

    /// Retorna a chave pai, se existir
    #[allow(dead_code)]
    pub fn parent(&self) -> Option<Self> {
        if self.segments.is_empty() {
            None
        } else {
            let mut segs = self.segments.clone();
            segs.pop();
            Some(Self { segments: segs })
        }
    }

    /// Verifica se a chave está vazia (raiz)
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Retorna o último segmento da chave
    pub fn name(&self) -> Option<&str> {
        self.segments.last().map(|s| s.as_str())
    }

    /// Retorna todos os segmentos da chave
    pub fn segments(&self) -> &[String] {
        &self.segments
    }

    /// Retorna a profundidade da chave (número de segmentos)
    pub fn depth(&self) -> usize {
        self.segments.len()
    }

    /// Verifica se esta chave é descendente de outra
    pub fn is_descendant_of(&self, other: &Key) -> bool {
        if other.segments.len() >= self.segments.len() {
            return false;
        }

        self.segments[..other.segments.len()] == other.segments
    }

    /// Converte para string com formato de caminho
    pub fn as_str(&self) -> String {
        if self.segments.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", self.segments.join("/"))
        }
    }

    /// Converte para bytes UTF-8
    #[allow(dead_code)]
    pub fn as_bytes(&self) -> Vec<u8> {
        self.as_str().into_bytes()
    }
}

impl Display for Key {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        f.write_str(&self.as_str())
    }
}

impl From<&str> for Key {
    fn from(path: &str) -> Self {
        Key::new(path)
    }
}

impl From<String> for Key {
    fn from(path: String) -> Self {
        Key::new(path)
    }
}

/// Ordem de classificação para queries
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Order {
    #[default]
    Asc,
    Desc,
}

/// Configuração de query para busca no datastore
///
/// Permite filtrar por prefixo, limitar resultados e definir ordenação.
#[derive(Clone, Debug, Default)]
pub struct Query {
    /// Prefixo para filtrar chaves (None = todas as chaves)
    pub prefix: Option<Key>,
    /// Número máximo de resultados (None = sem limite)
    pub limit: Option<usize>,
    /// Ordem de classificação
    pub order: Order,
    /// Offset para paginação
    pub offset: Option<usize>,
}

impl Query {
    /// Cria um builder para construir queries complexas
    pub fn builder() -> QueryBuilder {
        QueryBuilder::default()
    }

    /// Cria uma query simples com apenas um prefixo
    pub fn with_prefix<K: Into<Key>>(prefix: K) -> Self {
        Self {
            prefix: Some(prefix.into()),
            limit: None,
            order: Order::default(),
            offset: None,
        }
    }

    /// Cria uma query que retorna todos os itens
    #[allow(dead_code)]
    pub fn all() -> Self {
        Self::default()
    }
}

/// Builder para construir queries de forma fluente
#[derive(Default)]
pub struct QueryBuilder {
    prefix: Option<Key>,
    limit: Option<usize>,
    order: Order,
    offset: Option<usize>,
}

impl QueryBuilder {
    /// Define o prefixo para filtrar chaves
    pub fn prefix<K: Into<Key>>(mut self, prefix: K) -> Self {
        self.prefix = Some(prefix.into());
        self
    }

    /// Define o número máximo de resultados
    pub fn limit(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }

    /// Define a ordem de classificação
    #[allow(dead_code)]
    pub fn order(mut self, o: Order) -> Self {
        self.order = o;
        self
    }

    /// Define o offset para paginação
    pub fn offset(mut self, n: usize) -> Self {
        self.offset = Some(n);
        self
    }

    /// Constrói a query final
    pub fn build(self) -> Query {
        Query {
            prefix: self.prefix,
            limit: self.limit,
            order: self.order,
            offset: self.offset,
        }
    }
}

/// Item de resultado de uma query
///
/// Contém uma chave e seu valor associado.
#[derive(Clone, Debug)]
pub struct ResultItem {
    pub key: Key,
    pub value: Vec<u8>,
}

impl ResultItem {
    /// Cria um novo item de resultado
    pub fn new(key: Key, value: Vec<u8>) -> Self {
        Self { key, value }
    }

    /// Converte o valor para string UTF-8, se possível
    pub fn value_as_string(&self) -> std::result::Result<String, std::string::FromUtf8Error> {
        String::from_utf8(self.value.clone())
    }

    /// Verifica se o valor está vazio
    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    /// Retorna o tamanho do valor em bytes
    pub fn size(&self) -> usize {
        self.value.len()
    }
}

/// Coleção de resultados de uma query
pub type Results = Vec<ResultItem>;

/// Extensões úteis para trabalhar com Results
pub trait ResultsExt {
    /// Filtra resultados por tamanho mínimo do valor
    fn filter_by_min_size(&self, min_size: usize) -> Results;

    /// Retorna apenas as chaves dos resultados
    fn keys(&self) -> Vec<Key>;

    /// Retorna o número total de bytes de todos os valores
    fn total_size(&self) -> usize;
}

impl ResultsExt for Results {
    fn filter_by_min_size(&self, min_size: usize) -> Results {
        self.iter()
            .filter(|item| item.value.len() >= min_size)
            .cloned()
            .collect()
    }

    fn keys(&self) -> Vec<Key> {
        self.iter().map(|item| item.key.clone()).collect()
    }

    fn total_size(&self) -> usize {
        self.iter().map(|item| item.value.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_creation() {
        let key = Key::new("/users/alice/profile");
        assert_eq!(key.segments(), &["users", "alice", "profile"]);
        assert_eq!(key.as_str(), "/users/alice/profile");
        assert_eq!(key.depth(), 3);
    }

    #[test]
    fn test_key_operations() {
        let root = Key::root();
        assert!(root.is_empty());
        assert_eq!(root.as_str(), "/");

        let child = root.child("config").child("database");
        assert_eq!(child.as_str(), "/config/database");

        let parent = child.parent().unwrap();
        assert_eq!(parent.as_str(), "/config");

        assert_eq!(child.name().unwrap(), "database");
    }

    #[test]
    fn test_key_hierarchy() {
        let parent = Key::new("/users/alice");
        let child = Key::new("/users/alice/profile");
        let other = Key::new("/users/bob");

        assert!(child.is_descendant_of(&parent));
        assert!(!other.is_descendant_of(&parent));
        assert!(!parent.is_descendant_of(&child));
    }

    #[test]
    fn test_query_builder() {
        let query = Query::builder()
            .prefix("/users")
            .limit(10)
            .order(Order::Desc)
            .offset(5)
            .build();

        assert_eq!(query.prefix.as_ref().unwrap().as_str(), "/users");
        assert_eq!(query.limit, Some(10));
        assert_eq!(query.order, Order::Desc);
        assert_eq!(query.offset, Some(5));
    }

    #[test]
    fn test_result_item() {
        let key = Key::new("/test/key");
        let value = b"test value".to_vec();
        let item = ResultItem::new(key.clone(), value.clone());

        assert_eq!(item.key, key);
        assert_eq!(item.value, value);
        assert_eq!(item.size(), 10);
        assert!(!item.is_empty());
        assert_eq!(item.value_as_string().unwrap(), "test value");
    }

    #[test]
    fn test_results_ext() {
        let results = vec![
            ResultItem::new(Key::new("/small"), b"hi".to_vec()),
            ResultItem::new(Key::new("/large"), b"hello world".to_vec()),
            ResultItem::new(Key::new("/medium"), b"hello".to_vec()),
        ];

        let filtered = results.filter_by_min_size(5);
        assert_eq!(filtered.len(), 2);

        let keys = results.keys();
        assert_eq!(keys.len(), 3);

        let total = results.total_size();
        assert_eq!(total, 2 + 11 + 5); // "hi" + "hello world" + "hello"
    }
}
