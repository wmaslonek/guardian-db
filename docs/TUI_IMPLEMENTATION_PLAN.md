# Guardian-DB TUI — Plano de Implementação

> Interface de Terminal (TUI) para inspeção, gerenciamento e monitoramento do Guardian-DB.

## Visão Geral

O Guardian-DB é uma biblioteca — toda interação hoje é feita via código Rust. Uma TUI dedicada transforma o banco em algo que **operadores e desenvolvedores podem inspecionar visualmente**, sem precisar escrever código para cada consulta ou diagnóstico.

### Stack Tecnológica

| Componente | Tecnologia | Justificativa |
|---|---|---|
| Renderização | `ratatui 0.30` | Já é dependência do projeto |
| Runtime async | `tokio` (full features) | Já é dependência do projeto |
| Eventos reativos | `EventBus` do guardian-db | Atualização em tempo real |
| Serialização | `serde_json` / `serde_cbor` | Já são dependências do projeto |
| Persistência | `redb` / KV stores | Já são dependências do projeto |

### Arquitetura Geral

```
┌─────────────────────────────────────────────────┐
│                    TUI App                       │
│  ┌───────────┐ ┌───────────┐ ┌───────────────┐  │
│  │ Dashboard │ │ Inspetores│ │   Monitores   │  │
│  │  (home)   │ │ (stores)  │ │ (rede/sync)   │  │
│  └─────┬─────┘ └─────┬─────┘ └──────┬────────┘  │
│        └──────────────┼──────────────┘           │
│                       │                          │
│              ┌────────▼────────┐                 │
│              │  State Machine  │                 │
│              │  (enum Screen)  │                 │
│              └────────┬────────┘                 │
│                       │                          │
│         ┌─────────────┼─────────────┐            │
│         │             │             │            │
│    ┌────▼────┐  ┌─────▼─────┐ ┌────▼─────┐     │
│    │ Terminal │  │  EventBus │ │  Tokio   │     │
│    │  Input  │  │ Listener  │ │  Tasks   │     │
│    └─────────┘  └───────────┘ └──────────┘     │
└─────────────────────────────────────────────────┘
                       │
              ┌────────▼────────┐
              │   GuardianDB    │
              │  (IrohClient)   │
              └─────────────────┘
```

### Padrão de Navegação

```
[F1] Dashboard  →  [F2] Stores  →  [F3] Rede  →  [F4] Acesso  →  [F5] Keystore  →  [F6] Blobs
         │               │              │              │               │               │
    visão geral     lista stores    peers map     ACL manager     key list       blob browser
         │               │              │              │               │               │
    Enter →          Enter →        Enter →        Enter →         Enter →         Enter →
    detalhes        entries/kv     peer detail    role detail     key detail     blob detail
```

---

## Recursos e Fases de Implementação

---

### Recurso 1: Dashboard de Stores

**Objetivo:** Visão geral de todos os stores (EventLog, KeyValue, Document) com métricas básicas.

#### Fase 1.1 — Scaffold da Aplicação TUI
- [X] Criar `examples/guardian_tui.rs` (ou `src/bin/guardian_administration_panel.rs`)
- [X] Configurar loop principal com `tokio::select!` multiplexando:
  - Input do terminal (crossterm events)
  - Eventos do `EventBus`
  - Timer de refresh (1s)
- [X] Implementar enum `Screen` como state machine (Dashboard, StoreDetail, etc.)
- [X] Implementar layout base com:
  - Header: nome do DB, NodeID, uptime
  - Body: conteúdo dinâmico por tela
  - Footer: atalhos de teclado ativos
- [X] Redirecionar logs do `tracing` para barra de status (não para stderr)

#### Fase 1.2 — Listagem de Stores
- [X] Enumerar todos os stores abertos via `GuardianDB`
- [X] Exibir tabela com colunas: Nome, Tipo (EventLog/KV/Document), Qtd Entries, Peers Conectados
- [X] Implementar navegação com setas ↑↓ e seleção com Enter
- [X] Adicionar filtro por tipo de store (Tab para alternar)
- [X] Indicador visual de status: 🟢 sincronizado, 🟡 sincronizando, 🔴 erro

#### Fase 1.3 — Detalhes do Store Selecionado
- [ ] Tela de detalhe com: nome, tipo, endereço, data de criação, tamanho estimado
- [ ] Lista de peers conectados ao store
- [ ] Últimas N entries (preview truncado)
- [ ] Ação: conectar novo peer ao store (input de NodeID)
- [ ] Navegação: Esc para voltar à lista

---

### Recurso 2: Inspetor de EventLog

**Objetivo:** Navegar, buscar e inspecionar entries dos CRDTs com visualização de histórico de merge.

#### Fase 2.1 — Listagem de Entries
- [ ] Ao selecionar um EventLog no Dashboard, listar todas entries ordenadas por timestamp
- [ ] Colunas: #, Timestamp, Autor (NodeID abreviado), Preview do Payload
- [ ] Scroll com PageUp/PageDown, setas ↑↓
- [ ] Lazy loading: carregar em blocos de 100 entries para stores grandes

#### Fase 2.2 — Detalhe de Entry
- [ ] Exibir entry completa em painel lateral (split pane) ou tela dedicada
- [ ] Campos: hash, clock, identity, next (ponteiros CRDT), payload formatado
- [ ] Payload exibido como JSON indentado (serde_json::to_string_pretty)
- [ ] Copiar hash/payload para clipboard com atalho (c)

#### Fase 2.3 — Busca e Filtros
- [ ] Campo de busca (/) que filtra entries por:
  - Conteúdo do payload (substring match)
  - Autor (NodeID)
  - Range de timestamps (de/até)
- [ ] Highlight dos termos encontrados no resultado
- [ ] Contador de resultados: "12 de 847 entries"

#### Fase 2.4 — Visualização de Heads CRDT
- [ ] Painel mostrando heads atuais do log (quantas, de quais peers)
- [ ] Indicador se há divergência (múltiplas heads = merge pendente)
- [ ] Diff simplificado entre versões de heads de peers diferentes
- [ ] Histórico de merges (timeline visual com caracteres box-drawing)

---

### Recurso 3: Inspetor de KeyValue

**Objetivo:** Browse interativo de pares chave-valor com edição inline e status de replicação.

#### Fase 3.1 — Listagem de Chaves
- [ ] Listar todas as chaves do KV store selecionado via `.all()`
- [ ] Colunas: Chave, Valor (preview truncado), Tamanho
- [ ] Ordenação: alfabética (padrão), por tamanho, por data de modificação
- [ ] Scroll e seleção como nos outros inspetores

#### Fase 3.2 — Detalhe e Edição de Valor
- [ ] Exibir valor completo (JSON formatado se aplicável, raw caso contrário)
- [ ] Modo edição (e): abrir editor inline com validação JSON
- [ ] Confirmar alteração (Enter) → executa `.put()` com replicação automática
- [ ] Cancelar (Esc) → descarta alterações
- [ ] Indicador de que o valor será replicado para N peers

#### Fase 3.3 — Operações CRUD
- [ ] Criar nova chave (n): input de nome + valor
- [ ] Deletar chave (d): confirmação antes de executar `.delete()`
- [ ] Busca por chave (/): filtro por substring no nome da chave
- [ ] Exportar todas chaves como JSON (x)

---

### Recurso 4: Gerenciador de Access Control

**Objetivo:** Interface visual para criar, editar e auditar regras de controle de acesso.

#### Fase 4.1 — Listagem de Controllers
- [ ] Listar controllers existentes: tipo (Simple/Guardian/Iroh), endereço do manifest
- [ ] Indicador de tipo com cor: 🔵 Simple, 🟢 Guardian, 🟣 Iroh
- [ ] Exibir quantidade de regras e keys autorizadas por controller

#### Fase 4.2 — Detalhe de Controller
- [ ] Exibir todas as permissões agrupadas por papel (admin, write, read)
- [ ] Para cada papel, listar os key IDs autorizados
- [ ] Painel lateral com resumo: total de keys, permissões por papel

#### Fase 4.3 — Operações de Grant/Revoke
- [ ] Conceder permissão (g): selecionar papel → input de key ID → confirmar
- [ ] Revogar permissão (r): selecionar key → confirmar revogação
- [ ] Feedback visual imediato (notificação temporária de sucesso/erro)
- [ ] Log de auditoria: últimas N operações de grant/revoke com timestamp

#### Fase 4.4 — Criação de Novo Controller
- [ ] Wizard interativo:
  1. Selecionar tipo (Simple / Guardian / Iroh)
  2. Definir nome
  3. Configurar permissões iniciais (papéis + keys)
  4. Confirmar e criar
- [ ] Exibir hash do manifest criado para compartilhamento
- [ ] Opção de copiar endereço para clipboard

---

### Recurso 5: Monitor de Replicação P2P

**Objetivo:** Visualização em tempo real do estado de sincronização entre peers.

#### Fase 5.1 — Lista de Peers
- [ ] Listar todos os peers conhecidos com status: Online/Offline/Sincronizando
- [ ] Colunas: NodeID (abreviado), Status, Último Sync, Latência, Stores Compartilhados
- [ ] Atualização reativa via `EventBus` (evento `EventExchangeHeads`)
- [ ] Auto-refresh a cada 5s para peers sem eventos recentes

#### Fase 5.2 — Detalhe de Peer
- [ ] Informações do peer: NodeID completo, endereços conhecidos, tipo de conexão
- [ ] Lista de stores compartilhados com este peer
- [ ] Histórico de syncs (últimos N exchange-heads)
- [ ] Ações: desconectar, forçar sync

#### Fase 5.3 — Dashboard de Sync em Tempo Real
- [ ] Painel com métricas agregadas:
  - Total de peers online / offline
  - Syncs por minuto
  - Bytes transferidos (se disponível)
  - Erros de sync recentes
- [ ] Barra de progresso para syncs em andamento
- [ ] Sparkline chart de atividade de sync (últimos 60s)

#### Fase 5.4 — Alertas e Diagnóstico
- [ ] Highlight de peers com problemas (sem sync há > 5min)
- [ ] Detecção de partição de rede (peers que não se veem)
- [ ] Sugestões de diagnóstico: "Peer X não sincroniza há 10min — verificar conectividade"
- [ ] Log filtrado de erros de rede/sync

---

### Recurso 6: Visualizador de Topologia de Rede

**Objetivo:** Mapa visual dos peers conectados e qualidade dos links.

#### Fase 6.1 — Grafo de Peers (ASCII)
- [ ] Representação em texto do grafo de conexões:
  ```
  [NodeA] ───── [NodeB]
     │              │
     └──── [NodeC] ─┘
  ```
- [ ] Tipo de link: mDNS local (linha sólida), relay n0 (linha tracejada)
- [ ] NodeID abreviado (primeiros 8 chars) como label

#### Fase 6.2 — Métricas de Link
- [ ] Exibir latência estimada em cada aresta do grafo
- [ ] Cor do link baseada em qualidade: verde (<50ms), amarelo (<200ms), vermelho (>200ms)
- [ ] Tooltip no peer selecionado com métricas detalhadas

#### Fase 6.3 — Descoberta e Relay
- [ ] Indicador de método de descoberta por peer (mDNS / relay / manual)
- [ ] Status do relay n0: conectado/desconectado, latência ao relay
- [ ] Lista de peers descobertos mas não conectados

---

### Recurso 7: Gerenciador de Keystore

**Objetivo:** Gestão visual das chaves criptográficas armazenadas.

#### Fase 7.1 — Listagem de Chaves
- [ ] Listar chaves do `RedbKeystore`: ID, tipo, data de criação
- [ ] Indicador de chave ativa vs. rotacionada
- [ ] Filtro por tipo de chave

#### Fase 7.2 — Detalhes de Chave
- [ ] Exibir metadata da chave (sem exibir material privado)
- [ ] Chave pública exportável (copiar para clipboard)
- [ ] Stores/controllers que usam esta chave

#### Fase 7.3 — Operações de Chave
- [ ] Gerar nova chave (n): selecionar tipo → confirmar
- [ ] Exportar chave pública (x)
- [ ] Rotacionar chave (r): gerar nova, marcar antiga como rotacionada
- [ ] **Nunca** exibir chave privada na TUI — apenas operações seguras

---

### Recurso 8: Explorador de EventBus

**Objetivo:** Monitor em tempo real dos eventos internos do Guardian-DB.

#### Fase 8.1 — Stream de Eventos
- [ ] Subscrever ao `EventBus` e exibir eventos em lista scrollável
- [ ] Cada evento: timestamp, tipo, resumo (1 linha)
- [ ] Scroll automático para o final (toggle com 'f' para follow mode)
- [ ] Pausar stream com espaço (buffer em background)

#### Fase 8.2 — Filtros de Evento
- [ ] Filtrar por tipo de evento: Sync, ExchangeHeads, AccessControl, Store
- [ ] Filtrar por peer de origem
- [ ] Campo de busca por conteúdo do payload
- [ ] Combinar filtros (AND lógico)

#### Fase 8.3 — Estatísticas
- [ ] Contador por tipo de evento (tabela de frequência)
- [ ] Eventos por segundo (sparkline)
- [ ] Top peers por volume de eventos

---

### Recurso 9: Browser de BlobStore

**Objetivo:** Navegar e gerenciar blobs armazenados (arquivos transferidos via P2P).

#### Fase 9.1 — Listagem de Blobs
- [ ] Listar blobs: Hash BLAKE3, tamanho, data de adição
- [ ] Ordenação por tamanho ou data
- [ ] Indicador de blobs com download completo vs. parcial

#### Fase 9.2 — Detalhes de Blob
- [ ] Hash completo, tamanho, peers que possuem o blob
- [ ] Preview do conteúdo (primeiros N bytes, se texto)
- [ ] Metadados associados (nome original do arquivo, mime type se disponível)

#### Fase 9.3 — Operações
- [ ] Adicionar blob a partir de arquivo local (a): input de caminho
- [ ] Exportar blob para arquivo (x): salvar em disco
- [ ] Deletar blob local (d): confirmação + aviso sobre replicação
- [ ] Calcular uso total de disco por blobs

---

## Cronograma de Fases Recomendado

### Fase A — Fundação (Recursos 1.1)
**Pré-requisito para todos os outros recursos.**

Entregas:
- Scaffold da TUI com loop async
- State machine de navegação entre telas
- Layout base (header/body/footer)
- Redirecionamento de logs

Dependências: nenhuma.

### Fase B — Inspetores de Dados (Recursos 1.2, 1.3, 2, 3)
**Funcionalidade core — ver o que está dentro do banco.**

Entregas:
- Dashboard com lista de stores
- Inspetor de EventLog completo
- Inspetor de KeyValue completo

Dependências: Fase A concluída.

### Fase C — Segurança e Chaves (Recursos 4, 7)
**Gestão de quem pode acessar o quê.**

Entregas:
- Gerenciador de Access Control
- Gerenciador de Keystore

Dependências: Fase A concluída. Pode ser feita em paralelo com Fase B.

### Fase D — Monitoramento de Rede (Recursos 5, 6, 8)
**Entender o que está acontecendo na rede P2P.**

Entregas:
- Monitor de replicação
- Visualizador de topologia
- Explorador de EventBus

Dependências: Fase A concluída. Pode ser feita em paralelo com B e C.

### Fase E — Armazenamento (Recurso 9)
**Gestão de blobs e arquivos.**

Entregas:
- Browser de BlobStore

Dependências: Fase A concluída.

---

## Convenções Técnicas

### Atalhos de Teclado Globais

| Tecla | Ação |
|---|---|
| F1–F6 | Navegar entre telas principais |
| Esc | Voltar à tela anterior |
| q | Sair da aplicação |
| / | Abrir campo de busca |
| ? | Exibir ajuda contextual |
| Tab | Alternar entre painéis/filtros |
| Enter | Abrir detalhe do item selecionado |
| r | Refresh manual dos dados |

### Padrão de Código

```rust
// Estado global da aplicação
enum Screen {
    Dashboard,
    StoreDetail { store_name: String },
    EventLogInspector { log_name: String },
    KeyValueInspector { kv_name: String },
    AccessControlManager,
    AccessControlDetail { controller_id: String },
    ReplicationMonitor,
    PeerDetail { node_id: String },
    NetworkTopology,
    EventBusExplorer,
    KeystoreManager,
    KeyDetail { key_id: String },
    BlobBrowser,
    BlobDetail { hash: String },
}

// Loop principal
async fn run_tui(db: GuardianDB) -> Result<()> {
    let mut terminal = ratatui::init();
    let event_bus = db.event_bus();

    loop {
        terminal.draw(|frame| render(&app_state, frame))?;

        tokio::select! {
            // Input do terminal
            event = crossterm_event_stream.next() => {
                handle_input(&mut app_state, event);
            }
            // Eventos do GuardianDB
            db_event = event_bus.recv() => {
                handle_db_event(&mut app_state, db_event);
            }
            // Timer de refresh
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                refresh_metrics(&mut app_state).await;
            }
        }

        if app_state.should_quit {
            break;
        }
    }

    ratatui::restore();
    Ok(())
}
```

### Segurança

- **Nunca** exibir material de chave privada na interface
- Confirmar operações destrutivas (delete) com diálogo "Tem certeza? [s/N]"
- Sanitizar inputs de NodeID e endereços antes de usar
- Não logar payloads sensíveis na barra de status

### Testabilidade

- Separar lógica de estado (`AppState`) da renderização (`render()`)
- Testes unitários para transições de estado (state machine)
- Testes de integração com `GuardianDB` em modo in-memory
- Mock do `EventBus` para testar atualizações reativas
