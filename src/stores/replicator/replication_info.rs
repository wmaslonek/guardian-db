use serde::{Deserialize, Serialize};

/// Representa o estado de replicação de uma store
/// Mantido para compatibilidade, mas a replicação é feita pelo Iroh
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicationInfo {
    /// Número de entradas já replicadas
    progress: usize,
    /// Número máximo de entradas a serem replicadas
    max: usize,
    /// Flag indicando se está em processo de sincronização
    buffered: usize,
    /// Flag indicando se está aguardando sincronização
    queued: usize,
}

impl Default for ReplicationInfo {
    fn default() -> Self {
        Self::new()
    }
}

impl ReplicationInfo {
    /// Cria uma nova instância com valores padrão
    pub fn new() -> Self {
        Self {
            progress: 0,
            max: 0,
            buffered: 0,
            queued: 0,
        }
    }

    /// Retorna o progresso atual (0-100)
    pub async fn get_progress(&self) -> usize {
        self.progress
    }

    /// Retorna o valor máximo
    pub async fn get_max(&self) -> usize {
        self.max
    }

    /// Retorna o progresso e o máximo simultaneamente
    pub async fn get_progress_and_max(&self) -> (usize, usize) {
        (self.progress, self.max)
    }

    /// Retorna o número de entradas em buffer
    pub async fn get_buffered(&self) -> usize {
        self.buffered
    }

    /// Retorna o número de entradas na fila
    pub async fn get_queued(&self) -> usize {
        self.queued
    }

    /// Atualiza o progresso
    pub async fn inc_progress(&mut self, amount: usize) {
        self.progress += amount;
    }

    /// Define o valor máximo
    pub async fn set_max(&mut self, value: usize) {
        self.max = value;
    }

    /// Incrementa o buffer
    pub async fn inc_buffered(&mut self) {
        self.buffered += 1;
    }

    /// Decrementa o buffer
    pub async fn dec_buffered(&mut self) {
        if self.buffered > 0 {
            self.buffered -= 1;
        }
    }

    /// Incrementa a fila
    pub async fn inc_queued(&mut self) {
        self.queued += 1;
    }

    /// Decrementa a fila
    pub async fn dec_queued(&mut self) {
        if self.queued > 0 {
            self.queued -= 1;
        }
    }

    /// Reseta todos os contadores
    pub async fn reset(&mut self) {
        self.progress = 0;
        self.max = 0;
        self.buffered = 0;
        self.queued = 0;
    }

    /// Define o progresso
    pub async fn set_progress(&mut self, value: usize) {
        self.progress = value;
    }

    /// Define progresso e máximo simultaneamente
    pub async fn set_progress_and_max(&mut self, progress: usize, max: usize) {
        self.progress = progress;
        self.max = max;
    }

    /// Calcula a porcentagem de progresso (0-100)
    pub async fn progress_percentage(&self) -> f64 {
        if self.max == 0 {
            0.0
        } else {
            (self.progress as f64 / self.max as f64) * 100.0
        }
    }
}
