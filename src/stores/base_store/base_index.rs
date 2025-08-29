use crate::eqlabs_ipfs_log::{entry::Entry, log::Log};
use crate::error::GuardianError;
use crate::iface::StoreIndex;
use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

/// Equivalente à struct `baseIndex` do Go.
///
/// BaseIndex é a implementação base de um índice para IPFS log stores.
/// Mantém um mapeamento de chaves para valores, processando entradas do log
/// de operações para manter o estado atualizado.
pub struct BaseIndex {
    /// ID do índice, geralmente a chave pública da loja.
    id: Vec<u8>,

    /// Mapa interno para o índice baseado em hash.
    /// Armazena valores como bytes para flexibilidade, protegido por RwLock
    /// para permitir acesso concorrente seguro (múltiplos leitores ou um escritor).
    index: RwLock<HashMap<String, Vec<u8>>>,
}

/// Equivalente à função `NewBaseIndex` do Go.
///
/// Construtor para o `BaseIndex`. Cria uma nova instância com um HashMap vazio
/// para armazenar o índice de chave-valor.
pub fn new_base_index(
    public_key: Vec<u8>,
) -> Box<dyn StoreIndex<Error = GuardianError> + Send + Sync> {
    Box::new(BaseIndex {
        id: public_key,
        index: RwLock::new(HashMap::new()),
    })
}

/// Implementação do trait `StoreIndex` para `BaseIndex`.
impl StoreIndex for BaseIndex {
    /// Especifica que usaremos GuardianError como o tipo de erro associado.
    type Error = GuardianError;

    /// Equivalente à função `Get` em Go.
    ///
    /// Busca um valor no índice pela sua chave.
    /// Retorna `Some(valor)` se a chave existir, ou `None` caso contrário.
    ///
    /// NOTA: Limitação atual - a trait StoreIndex tem um design problemático
    /// que não permite retornar referências para dados protegidos por lock.
    /// Uma refatoração futura da trait seria necessária para resolver isso adequadamente.
    fn get(&self, _key: &str) -> Option<&(dyn Any + Send + Sync)> {
        // Por enquanto retorna None devido à limitação da trait.
        // Uma implementação adequada requereria mudanças na arquitetura da trait.
        None
    }

    /// Equivalente à função `UpdateIndex` em Go.
    ///
    /// Atualiza o índice processando as entradas do log de operações.
    /// Implementa a lógica CRDT processando operações PUT e DEL.
    fn update_index(&mut self, _log: &Log, entries: &[Entry]) -> Result<(), Self::Error> {
        // Conjunto para rastrear chaves já processadas, garantindo que
        // apenas a operação mais recente para cada chave seja aplicada.
        let mut handled = HashSet::new();

        // Adquire um bloqueio de escrita para modificar o índice de forma segura.
        let mut index = self.index.write().map_err(|e| {
            GuardianError::Store(format!("Falha ao adquirir lock de escrita: {}", e))
        })?;

        // Itera sobre as entradas fornecidas em ordem reversa (do mais novo para o mais antigo).
        // Isso garante que apenas a operação mais recente para cada chave seja aplicada.
        for entry in entries.iter().rev() {
            // Parseia a operação da entrada do log
            let operation =
                match crate::stores::operation::operation::parse_operation(entry.clone()) {
                    Ok(op) => op,
                    Err(e) => {
                        // Log o erro mas continua processando outras entradas
                        eprintln!("Aviso: Erro ao parsear operação: {}", e);
                        continue;
                    }
                };

            // Obtém a chave da operação
            let key = match operation.key() {
                Some(k) if !k.is_empty() => k,
                _ => continue, // Ignora entradas com chave nula ou vazia
            };

            // Evita processar a mesma chave múltiplas vezes
            if handled.contains(key) {
                continue;
            }
            handled.insert(key.clone());

            // Aplica a operação baseada no tipo
            match operation.op() {
                "PUT" => {
                    let value = operation.value();
                    if !value.is_empty() {
                        index.insert(key.clone(), value.to_vec());
                    }
                }
                "DEL" => {
                    index.remove(key);
                }
                _ => {
                    // Ignora operações desconhecidas
                    eprintln!("Aviso: Operação desconhecida ignorada: {}", operation.op());
                }
            }
        }

        Ok(())
    }
}

impl BaseIndex {
    /// Retorna o ID do índice (public key).
    pub fn id(&self) -> &[u8] {
        &self.id
    }

    /// Retorna uma cópia de todas as chaves presentes no índice.
    /// Útil para iteração e debug.
    pub fn keys(&self) -> Result<Vec<String>, GuardianError> {
        let index_lock = self.index.read().map_err(|e| {
            GuardianError::Store(format!("Falha ao adquirir lock de leitura: {}", e))
        })?;

        Ok(index_lock.keys().cloned().collect())
    }

    /// Retorna uma cópia do valor associado à chave, se existir.
    /// Esta é uma alternativa ao método `get()` da trait que tem limitações.
    pub fn get_value(&self, key: &str) -> Result<Option<Vec<u8>>, GuardianError> {
        let index_lock = self.index.read().map_err(|e| {
            GuardianError::Store(format!("Falha ao adquirir lock de leitura: {}", e))
        })?;

        Ok(index_lock.get(key).cloned())
    }

    /// Retorna o número de entradas no índice.
    pub fn len(&self) -> Result<usize, GuardianError> {
        let index_lock = self.index.read().map_err(|e| {
            GuardianError::Store(format!("Falha ao adquirir lock de leitura: {}", e))
        })?;

        Ok(index_lock.len())
    }

    /// Verifica se o índice está vazio.
    pub fn is_empty(&self) -> Result<bool, GuardianError> {
        let index_lock = self.index.read().map_err(|e| {
            GuardianError::Store(format!("Falha ao adquirir lock de leitura: {}", e))
        })?;

        Ok(index_lock.is_empty())
    }

    /// Limpa todo o conteúdo do índice.
    /// Útil para reset ou testes.
    pub fn clear(&mut self) -> Result<(), GuardianError> {
        let mut index_lock = self.index.write().map_err(|e| {
            GuardianError::Store(format!("Falha ao adquirir lock de escrita: {}", e))
        })?;

        index_lock.clear();
        Ok(())
    }
}

// MELHORIAS IMPLEMENTADAS:
//
// 1. Refatoração da estrutura de dados:
//    - Mudou de Vec<Entry> para HashMap<String, Vec<u8>> para indexação eficiente
//    - Permite busca O(1) por chave ao invés de iteração O(n)
//
// 2. Implementação correta do update_index:
//    - Processa operações PUT e DEL corretamente
//    - Implementa lógica CRDT (apenas a operação mais recente por chave)
//    - Tratamento de erros sem panic (elimina unwrap() perigoso)
//
// 3. Métodos auxiliares adicionados:
//    - get_value(): alternativa segura ao get() da trait
//    - keys(): listagem de todas as chaves
//    - len(), is_empty(): estatísticas do índice
//    - clear(): reset completo do índice
//    - id(): acesso ao identificador do índice
//
// 4. Tratamento de erros robusto:
//    - Uso de Result ao invés de unwrap()
//    - Mensagens de erro descritivas
//    - Logs de warning para operações problemáticas
//
// 5. Thread safety:
//    - Uso adequado de RwLock para acesso concorrente
//    - Tratamento de lock poisoning
//
// LIMITAÇÕES CONHECIDAS:
// - O método get() da trait StoreIndex tem limitações arquiteturais
//   que impedem retornar referências para dados protegidos por lock.
//   Use get_value() como alternativa segura.
