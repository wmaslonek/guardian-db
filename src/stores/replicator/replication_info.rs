use crate::stores::replicator::traits::ReplicationInfo as ReplicationInfoTrait;
use tokio::sync::RwLock;

// Struct interna para conter os dados protegidos pelo Lock.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
struct InfoState {
    progress: usize,
    max: usize,
}

// equivalente a replicationInfo struct em go
/// Implementação thread-safe da interface `ReplicationInfo`.
/// Utiliza um `RwLock` para garantir acesso seguro de múltiplas threads.
///
/// Esta implementação fornece:
/// - Acesso thread-safe através de `RwLock`
/// - Operações atômicas para get/set de progresso e máximo
/// - Métodos de conveniência para operações comuns
#[derive(Default, Debug)]
pub struct ReplicationInfo {
    state: RwLock<InfoState>,
}

// Note: Clone implementation creates a new instance with default values
// For preserving current values, use clone_with_values() or get_progress_and_max() + with_values()
impl Clone for ReplicationInfo {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl ReplicationInfo {
    /// Cria um clone que preserva os valores atuais.
    /// Esta é uma operação assíncrona pois precisa ler o estado atual.
    pub async fn clone_with_values(&self) -> Self {
        let state = self.state.read().await;
        Self::with_values(state.progress, state.max)
    }
}

// equivalente a NewReplicationInfo em go
impl ReplicationInfo {
    /// Cria uma nova instância de ReplicationInfo.
    pub fn new() -> Self {
        Self::default()
    }

    /// Cria uma nova instância com valores iniciais.
    pub fn with_values(progress: usize, max: usize) -> Self {
        Self {
            state: RwLock::new(InfoState { progress, max }),
        }
    }

    /// Incrementa o progresso em 1.
    /// Conveniência para operações de incremento simples.
    pub async fn increment_progress(&self) {
        let mut state = self.state.write().await;
        state.progress = state.progress.saturating_add(1);
    }

    /// Incrementa o progresso por um valor específico.
    /// Usa saturating_add para evitar overflow.
    pub async fn increment_progress_by(&self, amount: usize) {
        let mut state = self.state.write().await;
        state.progress = state.progress.saturating_add(amount);
    }

    /// Retorna se a replicação está completa (progress >= max).
    pub async fn is_complete(&self) -> bool {
        let state = self.state.read().await;
        state.max > 0 && state.progress >= state.max
    }

    /// Retorna o percentual de progresso (0.0 a 1.0).
    /// Retorna 0.0 se max for 0.
    pub async fn progress_ratio(&self) -> f64 {
        let state = self.state.read().await;
        if state.max == 0 {
            0.0
        } else {
            state.progress as f64 / state.max as f64
        }
    }

    /// Retorna o percentual de progresso (0 a 100).
    /// Retorna 0 se max for 0.
    pub async fn progress_percentage(&self) -> u8 {
        let ratio = self.progress_ratio().await;
        (ratio * 100.0).min(100.0) as u8
    }

    /// Obtém tanto o progresso quanto o máximo em uma única operação.
    /// Mais eficiente que chamadas separadas quando ambos os valores são necessários.
    pub async fn get_progress_and_max(&self) -> (usize, usize) {
        let state = self.state.read().await;
        (state.progress, state.max)
    }

    /// Define tanto o progresso quanto o máximo em uma única operação atômica.
    /// Mais eficiente que chamadas separadas.
    pub async fn set_progress_and_max(&self, progress: usize, max: usize) {
        let mut state = self.state.write().await;
        state.progress = progress;
        state.max = max;
    }

    /// Aplica um incremento ao máximo, usado quando novos itens são descobertos.
    pub async fn increment_max(&self, amount: usize) {
        let mut state = self.state.write().await;
        state.max = state.max.saturating_add(amount);
    }

    /// Retorna uma representação em string do progresso atual.
    pub async fn to_string(&self) -> String {
        let state = self.state.read().await;
        format!(
            "{}/{} ({}%)",
            state.progress,
            state.max,
            if state.max > 0 {
                (state.progress * 100 / state.max).min(100)
            } else {
                0
            }
        )
    }

    /// Verifica se os valores são válidos (progress <= max).
    pub async fn is_valid(&self) -> bool {
        let state = self.state.read().await;
        state.progress <= state.max
    }
}

// Implementação do trait `ReplicationInfo` para a nossa struct.
// Isso garante que nossa struct está em conformidade com a interface que definimos.
impl ReplicationInfoTrait for ReplicationInfo {
    // equivalente a GetProgress em go
    async fn get_progress(&self) -> usize {
        let state = self.state.read().await;
        state.progress
    }

    // equivalente a GetMax em go
    async fn get_max(&self) -> usize {
        let state = self.state.read().await;
        state.max
    }

    // equivalente a SetProgress em go
    async fn set_progress(&self, i: usize) {
        let mut state = self.state.write().await;
        state.progress = i;
    }

    // equivalente a SetMax em go
    async fn set_max(&self, i: usize) {
        let mut state = self.state.write().await;
        state.max = i;
    }

    // equivalente a Reset em go
    async fn reset(&self) {
        let mut state = self.state.write().await;
        state.progress = 0;
        state.max = 0;
    }
}
