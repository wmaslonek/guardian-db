// ╔════════════════════════════════════════════════════════════════════════════════╗
// ║                          ⚠ MODULE IN DEVELOPMENT                              ║
// ╚════════════════════════════════════════════════════════════════════════════════╝
// ═══════════════════════════════════════════════════════════════════════════════
// Guardian-DB Administration Panel (TUI)
// ═══════════════════════════════════════════════════════════════════════════════
//
// Painel visual para inspeção, gerenciamento e monitoramento do Guardian-DB.
// Fase 1.1: Scaffold da aplicação com state machine, layout base e log capture.
//
// Uso:
//   cargo run --bin guardian-admin
//   ou com diretório customizado:
//   cargo run --bin guardian-admin -- --data-dir ./meu_db
// ═══════════════════════════════════════════════════════════════════════════════

use guardian_db::{
    guardian::{
        GuardianDB,
        core::{
            EventDatabaseCreated, EventExchangeHeads, EventPeerConnected, EventPeerDisconnected,
            EventStoreUpdated, EventSyncCompleted, EventSyncError, NewGuardianDBOptions,
        },
    },
    p2p::network::{client::IrohClient, config::ClientConfig},
};
// Trait necessária para acessar store_type(), db_name(), index() nos stores
#[allow(unused_imports)]
use guardian_db::traits::Store;
use ratatui::{
    Frame,
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind},
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};
use tracing_subscriber::fmt::MakeWriter;

// ═══════════════════════════════════════════════════════════
// Log Capture — redireciona tracing para a barra de status
// ═══════════════════════════════════════════════════════════

#[derive(Clone)]
struct LogBuffer {
    last_line: Arc<StdMutex<String>>,
}

impl LogBuffer {
    fn new() -> Self {
        Self {
            last_line: Arc::new(StdMutex::new(String::new())),
        }
    }

    fn get_last(&self) -> String {
        self.last_line.lock().map(|l| l.clone()).unwrap_or_default()
    }
}

struct LogWriter {
    buf: Vec<u8>,
    last_line: Arc<StdMutex<String>>,
}

impl std::io::Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for LogWriter {
    fn drop(&mut self) {
        if let Ok(s) = std::str::from_utf8(&self.buf) {
            let trimmed = s.trim();
            if !trimmed.is_empty()
                && let Ok(mut last) = self.last_line.lock()
            {
                *last = trimmed.to_string();
            }
        }
    }
}

impl<'a> MakeWriter<'a> for LogBuffer {
    type Writer = LogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        LogWriter {
            buf: Vec::new(),
            last_line: Arc::clone(&self.last_line),
        }
    }
}

// ═══════════════════════════════════════════════════════════
// State Machine — telas do painel
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
enum Screen {
    /// Tela inicial de carregamento / conexão ao DB
    Connecting,
    /// Visão geral: lista de stores, métricas, node info
    Dashboard,
    /// Detalhes de um store selecionado
    StoreDetail { store_address: String },
    /// Inspetor de EventLog
    EventLogInspector { log_name: String },
    /// Inspetor de KeyValue
    KeyValueInspector { kv_name: String },
    /// Gerenciador de Access Control
    AccessControlManager,
    /// Detalhes de um controller de acesso
    AccessControlDetail { controller_id: String },
    /// Monitor de replicação P2P
    ReplicationMonitor,
    /// Detalhes de um peer
    PeerDetail { node_id: String },
    /// Visualizador de topologia de rede
    NetworkTopology,
    /// Explorador de EventBus
    EventBusExplorer,
    /// Gerenciador de Keystore
    KeystoreManager,
    /// Detalhes de uma chave
    KeyDetail { key_id: String },
    /// Browser de BlobStore
    BlobBrowser,
    /// Detalhes de um blob
    BlobDetail { hash: String },
}

// ═══════════════════════════════════════════════════════════
// Store Info — metadados coletados dos stores abertos
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy, PartialEq)]
enum SyncStatus {
    Synced,
    Syncing,
    Error,
}

impl SyncStatus {
    #[allow(dead_code)]
    fn label(&self) -> &str {
        match self {
            SyncStatus::Synced => "synced",
            SyncStatus::Syncing => "syncing",
            SyncStatus::Error => "error",
        }
    }

    fn color(&self) -> Color {
        match self {
            SyncStatus::Synced => Color::Green,
            SyncStatus::Syncing => Color::Yellow,
            SyncStatus::Error => Color::Red,
        }
    }

    fn icon(&self) -> &str {
        match self {
            SyncStatus::Synced => "●",
            SyncStatus::Syncing => "◐",
            SyncStatus::Error => "✗",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum StoreFilter {
    All,
    EventLog,
    KeyValue,
    Document,
}

impl StoreFilter {
    fn next(&self) -> Self {
        match self {
            StoreFilter::All => StoreFilter::EventLog,
            StoreFilter::EventLog => StoreFilter::KeyValue,
            StoreFilter::KeyValue => StoreFilter::Document,
            StoreFilter::Document => StoreFilter::All,
        }
    }

    fn label(&self) -> &str {
        match self {
            StoreFilter::All => "Todos",
            StoreFilter::EventLog => "EventLog",
            StoreFilter::KeyValue => "KeyValue",
            StoreFilter::Document => "Document",
        }
    }

    fn matches(&self, store_type: &str) -> bool {
        match self {
            StoreFilter::All => true,
            StoreFilter::EventLog => store_type == "eventlog",
            StoreFilter::KeyValue => store_type == "keyvalue",
            StoreFilter::Document => store_type == "document",
        }
    }
}

#[derive(Debug, Clone)]
struct StoreInfo {
    address: String,
    store_type: String,
    entry_count: usize,
    db_name: String,
    sync_status: SyncStatus,
    replication_progress: usize,
    replication_max: usize,
    #[allow(dead_code)]
    buffered: usize,
}

// ═══════════════════════════════════════════════════════════
// Notification — feedback temporário ao usuário
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Clone)]
struct Notification {
    message: String,
    is_error: bool,
    created_at: Instant,
}

impl Notification {
    fn success(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            is_error: false,
            created_at: Instant::now(),
        }
    }

    #[allow(dead_code)]
    fn error(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            is_error: true,
            created_at: Instant::now(),
        }
    }

    fn is_expired(&self) -> bool {
        self.created_at.elapsed().as_secs() >= 5
    }
}

// ═══════════════════════════════════════════════════════════
// App State — estado central da aplicação
// ═══════════════════════════════════════════════════════════

struct App {
    screen: Screen,
    screen_history: Vec<Screen>,
    should_quit: bool,
    log_buffer: LogBuffer,
    notification: Option<Notification>,
    started_at: Instant,

    // Dados do DB
    node_id: String,
    data_dir: PathBuf,
    stores: Vec<StoreInfo>,
    filtered_indices: Vec<usize>,
    store_list_state: ListState,
    store_filter: StoreFilter,

    // Contadores de rede
    peers_online: usize,
    syncs_total: u64,
    sync_errors: u64,
    has_updates: Arc<AtomicBool>,
}

impl App {
    fn new(log_buffer: LogBuffer, data_dir: PathBuf) -> Self {
        Self {
            screen: Screen::Connecting,
            screen_history: Vec::new(),
            should_quit: false,
            log_buffer,
            notification: None,
            started_at: Instant::now(),

            node_id: String::new(),
            data_dir,
            stores: Vec::new(),
            filtered_indices: Vec::new(),
            store_list_state: ListState::default(),
            store_filter: StoreFilter::All,

            peers_online: 0,
            syncs_total: 0,
            sync_errors: 0,
            has_updates: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Navega para uma nova tela, empilhando a anterior
    fn navigate_to(&mut self, screen: Screen) {
        let current = self.screen.clone();
        self.screen_history.push(current);
        self.screen = screen;
    }

    /// Volta para a tela anterior
    fn go_back(&mut self) {
        if let Some(prev) = self.screen_history.pop() {
            self.screen = prev;
        }
    }

    /// Define uma notificação de sucesso
    fn notify_success(&mut self, msg: impl Into<String>) {
        self.notification = Some(Notification::success(msg));
    }

    /// Define uma notificação de erro
    #[allow(dead_code)]
    fn notify_error(&mut self, msg: impl Into<String>) {
        self.notification = Some(Notification::error(msg));
    }

    /// Limpa notificações expiradas
    fn tick_notifications(&mut self) {
        if let Some(ref n) = self.notification
            && n.is_expired()
        {
            self.notification = None;
        }
    }

    /// Retorna o uptime formatado
    fn uptime(&self) -> String {
        let secs = self.started_at.elapsed().as_secs();
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        let s = secs % 60;
        format!("{h:02}:{m:02}:{s:02}")
    }

    /// Atualiza a lista de stores a partir do GuardianDB
    async fn refresh_stores(&mut self, db: &GuardianDB) {
        let all_stores = db.list_stores();
        let mut infos = Vec::with_capacity(all_stores.len());

        for (addr, store) in &all_stores {
            let index = store.index();
            let entry_count = index.len().unwrap_or(0);

            // Replication is handled natively by Iroh; the store no longer exposes
            // progress counters, so stores are reported as synced.
            infos.push(StoreInfo {
                address: addr.clone(),
                store_type: store.store_type().to_string(),
                entry_count,
                db_name: store.db_name().to_string(),
                sync_status: SyncStatus::Synced,
                replication_progress: 0,
                replication_max: 0,
                buffered: 0,
            });
        }

        // Ordena: eventlog primeiro, depois keyvalue, depois document
        infos.sort_by(|a, b| {
            let type_order = |t: &str| match t {
                "eventlog" => 0,
                "keyvalue" => 1,
                "document" => 2,
                _ => 3,
            };
            type_order(&a.store_type)
                .cmp(&type_order(&b.store_type))
                .then_with(|| a.db_name.cmp(&b.db_name))
        });

        self.stores = infos;
        self.apply_filter();
    }

    /// Aplica o filtro atual e recalcula os índices visíveis
    fn apply_filter(&mut self) {
        self.filtered_indices = self
            .stores
            .iter()
            .enumerate()
            .filter(|(_, s)| self.store_filter.matches(&s.store_type))
            .map(|(i, _)| i)
            .collect();

        // Ajusta seleção se ficou fora dos limites
        if self.filtered_indices.is_empty() {
            self.store_list_state.select(None);
        } else if let Some(sel) = self.store_list_state.selected()
            && sel >= self.filtered_indices.len()
        {
            self.store_list_state
                .select(Some(self.filtered_indices.len() - 1));
        }
    }

    /// Retorna os stores filtrados pela seleção atual
    fn filtered_stores(&self) -> Vec<&StoreInfo> {
        self.filtered_indices
            .iter()
            .filter_map(|&i| self.stores.get(i))
            .collect()
    }

    /// Retorna o store selecionado atualmente (considerando filtro)
    fn selected_store(&self) -> Option<&StoreInfo> {
        self.store_list_state
            .selected()
            .and_then(|sel| self.filtered_indices.get(sel))
            .and_then(|&i| self.stores.get(i))
    }
}

// ═══════════════════════════════════════════════════════════
// Rendering — layout e desenho de cada tela
// ═══════════════════════════════════════════════════════════

fn ui(frame: &mut Frame, app: &App) {
    let area = frame.area();

    // Layout: Header (3) | Body (flex) | Footer (3)
    let main_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Min(5),    // body
            Constraint::Length(3), // footer
        ])
        .split(area);

    render_header(frame, main_layout[0], app);
    render_body(frame, main_layout[1], app);
    render_footer(frame, main_layout[2], app);
}

fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let node_display = if app.node_id.is_empty() {
        "connecting...".to_string()
    } else if app.node_id.len() > 16 {
        format!("{}…", &app.node_id[..16])
    } else {
        app.node_id.clone()
    };

    let header_text = Line::from(vec![
        Span::styled(
            " ◆ Guardian Administration Panel ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("│ ", Style::default().fg(Color::DarkGray)),
        Span::styled("Node: ", Style::default().fg(Color::Gray)),
        Span::styled(node_display, Style::default().fg(Color::Yellow)),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled("Up: ", Style::default().fg(Color::Gray)),
        Span::styled(app.uptime(), Style::default().fg(Color::Green)),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled("Peers: ", Style::default().fg(Color::Gray)),
        Span::styled(
            app.peers_online.to_string(),
            Style::default().fg(if app.peers_online > 0 {
                Color::Green
            } else {
                Color::DarkGray
            }),
        ),
    ]);

    let header = Paragraph::new(header_text).block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::DarkGray)),
    );

    frame.render_widget(header, area);
}

fn render_body(frame: &mut Frame, area: Rect, app: &App) {
    match &app.screen {
        Screen::Connecting => render_connecting(frame, area),
        Screen::Dashboard => render_dashboard(frame, area, app),
        // Telas futuras — placeholder
        _ => render_placeholder(frame, area, &app.screen),
    }
}

fn render_connecting(frame: &mut Frame, area: Rect) {
    let text = vec![
        Line::from(""),
        Line::from(""),
        Line::from(vec![Span::styled(
            "◆ Guardian Administration Panel",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "Inicializando IrohClient e GuardianDB...",
            Style::default().fg(Color::Yellow),
        )]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "Aguarde enquanto o nó P2P é configurado.",
            Style::default().fg(Color::Gray),
        )]),
    ];

    let paragraph = Paragraph::new(text)
        .alignment(Alignment::Center)
        .block(Block::default());

    frame.render_widget(paragraph, area);
}

fn render_dashboard(frame: &mut Frame, area: Rect, app: &App) {
    // Layout: painel de métricas (5) | lista de stores (flex)
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5), // métricas
            Constraint::Min(3),    // stores
        ])
        .split(area);

    // Contadores por tipo
    let eventlog_count = app
        .stores
        .iter()
        .filter(|s| s.store_type == "eventlog")
        .count();
    let kv_count = app
        .stores
        .iter()
        .filter(|s| s.store_type == "keyvalue")
        .count();
    let doc_count = app
        .stores
        .iter()
        .filter(|s| s.store_type == "document")
        .count();
    let total_entries: usize = app.stores.iter().map(|s| s.entry_count).sum();
    let syncing_count = app
        .stores
        .iter()
        .filter(|s| s.sync_status == SyncStatus::Syncing)
        .count();
    let error_count_stores = app
        .stores
        .iter()
        .filter(|s| s.sync_status == SyncStatus::Error)
        .count();

    // Painel de métricas
    let metrics_text = vec![
        Line::from(vec![
            Span::styled(" Stores: ", Style::default().fg(Color::Gray)),
            Span::styled(
                app.stores.len().to_string(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" (", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{eventlog_count} log"),
                Style::default().fg(Color::Blue),
            ),
            Span::styled(", ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{kv_count} kv"), Style::default().fg(Color::Green)),
            Span::styled(", ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{doc_count} doc"),
                Style::default().fg(Color::Magenta),
            ),
            Span::styled(")   │   ", Style::default().fg(Color::DarkGray)),
            Span::styled("Entries: ", Style::default().fg(Color::Gray)),
            Span::styled(total_entries.to_string(), Style::default().fg(Color::White)),
            Span::styled("   │   ", Style::default().fg(Color::DarkGray)),
            Span::styled("Syncs: ", Style::default().fg(Color::Gray)),
            Span::styled(
                app.syncs_total.to_string(),
                Style::default().fg(Color::Green),
            ),
            Span::styled("   │   ", Style::default().fg(Color::DarkGray)),
            Span::styled("Erros: ", Style::default().fg(Color::Gray)),
            Span::styled(
                app.sync_errors.to_string(),
                Style::default().fg(if app.sync_errors > 0 {
                    Color::Red
                } else {
                    Color::DarkGray
                }),
            ),
        ]),
        Line::from(vec![
            Span::styled(" Syncing: ", Style::default().fg(Color::Gray)),
            Span::styled(
                syncing_count.to_string(),
                Style::default().fg(if syncing_count > 0 {
                    Color::Yellow
                } else {
                    Color::DarkGray
                }),
            ),
            Span::styled("   │   ", Style::default().fg(Color::DarkGray)),
            Span::styled("Erros em stores: ", Style::default().fg(Color::Gray)),
            Span::styled(
                error_count_stores.to_string(),
                Style::default().fg(if error_count_stores > 0 {
                    Color::Red
                } else {
                    Color::DarkGray
                }),
            ),
            Span::styled("   │   ", Style::default().fg(Color::DarkGray)),
            Span::styled("Dir: ", Style::default().fg(Color::Gray)),
            Span::styled(
                app.data_dir.display().to_string(),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
    ];

    let metrics = Paragraph::new(metrics_text).block(
        Block::default()
            .title(" Métricas ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(metrics, layout[0]);

    // Título da lista com filtro ativo
    let filter_label = app.store_filter.label();
    let filtered = app.filtered_stores();
    let store_title = if app.store_filter == StoreFilter::All {
        format!(" Stores ({}) ", app.stores.len())
    } else {
        format!(
            " Stores — {} ({}/{}) ",
            filter_label,
            filtered.len(),
            app.stores.len()
        )
    };

    // Lista de stores
    if filtered.is_empty() {
        let msg = if app.stores.is_empty() {
            vec![
                Line::from(""),
                Line::from(vec![Span::styled(
                    " Nenhuma store aberta.",
                    Style::default().fg(Color::DarkGray),
                )]),
                Line::from(""),
                Line::from(vec![Span::styled(
                    " Use db.log(), db.key_value() ou db.docs() via código para criar stores.",
                    Style::default().fg(Color::DarkGray),
                )]),
                Line::from(vec![Span::styled(
                    " As stores aparecerão aqui automaticamente.",
                    Style::default().fg(Color::DarkGray),
                )]),
            ]
        } else {
            vec![
                Line::from(""),
                Line::from(vec![Span::styled(
                    format!(
                        " Nenhuma store do tipo '{}' encontrada. Pressione Tab para alterar filtro.",
                        filter_label
                    ),
                    Style::default().fg(Color::DarkGray),
                )]),
            ]
        };

        let empty = Paragraph::new(msg).block(
            Block::default()
                .title(store_title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        frame.render_widget(empty, layout[1]);
    } else {
        let items: Vec<ListItem> = filtered
            .iter()
            .map(|s| {
                let type_color = match s.store_type.as_str() {
                    "eventlog" => Color::Blue,
                    "keyvalue" => Color::Green,
                    "document" => Color::Magenta,
                    _ => Color::Gray,
                };

                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!(" {} ", s.sync_status.icon()),
                        Style::default().fg(s.sync_status.color()),
                    ),
                    Span::styled(
                        format!("{:>10} ", s.store_type),
                        Style::default().fg(type_color),
                    ),
                    Span::styled("│ ", Style::default().fg(Color::DarkGray)),
                    Span::styled(&s.db_name, Style::default().fg(Color::White)),
                    Span::styled(
                        format!("  ({} entries)", s.entry_count),
                        Style::default().fg(Color::DarkGray),
                    ),
                    if s.sync_status == SyncStatus::Syncing {
                        Span::styled(
                            format!("  [{}/{}]", s.replication_progress, s.replication_max),
                            Style::default().fg(Color::Yellow),
                        )
                    } else {
                        Span::styled("", Style::default())
                    },
                ]))
            })
            .collect();

        let store_list = List::new(items)
            .block(
                Block::default()
                    .title(store_title)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray)),
            )
            .highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▶ ");

        frame.render_stateful_widget(store_list, layout[1], &mut app.store_list_state.clone());
    }
}

fn render_placeholder(frame: &mut Frame, area: Rect, screen: &Screen) {
    let screen_name = match screen {
        Screen::StoreDetail { store_address } => format!("Store: {store_address}"),
        Screen::EventLogInspector { log_name } => format!("EventLog: {log_name}"),
        Screen::KeyValueInspector { kv_name } => format!("KeyValue: {kv_name}"),
        Screen::AccessControlManager => "Access Control Manager".into(),
        Screen::AccessControlDetail { controller_id } => format!("ACL: {controller_id}"),
        Screen::ReplicationMonitor => "Monitor de Replicação".into(),
        Screen::PeerDetail { node_id } => format!("Peer: {node_id}"),
        Screen::NetworkTopology => "Topologia de Rede".into(),
        Screen::EventBusExplorer => "EventBus Explorer".into(),
        Screen::KeystoreManager => "Keystore Manager".into(),
        Screen::KeyDetail { key_id } => format!("Key: {key_id}"),
        Screen::BlobBrowser => "Blob Browser".into(),
        Screen::BlobDetail { hash } => format!("Blob: {hash}"),
        _ => "Unknown".into(),
    };

    let text = vec![
        Line::from(""),
        Line::from(vec![Span::styled(
            format!("🚧 {screen_name}"),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "Esta tela será implementada em fases futuras.",
            Style::default().fg(Color::DarkGray),
        )]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "Pressione Esc para voltar.",
            Style::default().fg(Color::Gray),
        )]),
    ];

    let paragraph = Paragraph::new(text).alignment(Alignment::Center).block(
        Block::default()
            .title(format!(" {screen_name} "))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );

    frame.render_widget(paragraph, area);
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    // Se há notificação ativa, mostrar ela
    if let Some(ref notif) = app.notification {
        let color = if notif.is_error {
            Color::Red
        } else {
            Color::Green
        };
        let notif_line = Line::from(vec![
            Span::styled(
                if notif.is_error { " ✗ " } else { " ✓ " },
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(&notif.message, Style::default().fg(color)),
        ]);
        let p = Paragraph::new(notif_line).block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        frame.render_widget(p, area);
        return;
    }

    // Atalhos de teclado contextuais
    let shortcuts = match app.screen {
        Screen::Connecting => vec![("q", "Sair")],
        Screen::Dashboard => vec![
            ("F1", "Dashboard"),
            ("F3", "Rede"),
            ("F4", "Acesso"),
            ("F5", "Keystore"),
            ("F6", "Blobs"),
            ("\u{2191}\u{2193}", "Navegar"),
            ("Enter", "Abrir"),
            ("Tab", "Filtrar"),
            ("r", "Refresh"),
            ("q", "Sair"),
        ],
        _ => vec![("Esc", "Voltar"), ("q", "Sair")],
    };

    let shortcut_spans: Vec<Span> = shortcuts
        .iter()
        .flat_map(|(key, desc)| {
            vec![
                Span::styled(
                    format!(" {key} "),
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" {desc}  "), Style::default().fg(Color::Gray)),
            ]
        })
        .collect();

    // Última linha de log no canto direito
    let log_line = app.log_buffer.get_last();
    let log_display = if log_line.len() > 60 {
        format!("…{}", &log_line[log_line.len() - 59..])
    } else {
        log_line
    };

    // Layout: atalhos (esquerda) | log (direita)
    let footer_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(30), Constraint::Length(62)])
        .split(area);

    let shortcuts_widget = Paragraph::new(Line::from(shortcut_spans)).block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(shortcuts_widget, footer_layout[0]);

    let log_widget = Paragraph::new(Line::from(vec![Span::styled(
        log_display,
        Style::default().fg(Color::DarkGray),
    )]))
    .alignment(Alignment::Right)
    .block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(log_widget, footer_layout[1]);
}

// ═══════════════════════════════════════════════════════════
// Input Handling
// ═══════════════════════════════════════════════════════════

fn handle_key(app: &mut App, key: KeyEvent) {
    // Ignora releases
    if key.kind != KeyEventKind::Press {
        return;
    }

    match key.code {
        // Quit global
        KeyCode::Char('q') => {
            app.should_quit = true;
        }

        // Navegação global por teclas de função
        KeyCode::F(1) => {
            app.screen = Screen::Dashboard;
            app.screen_history.clear();
        }
        KeyCode::F(3) => {
            app.screen = Screen::ReplicationMonitor;
            app.screen_history.clear();
            app.screen_history.push(Screen::Dashboard);
        }
        KeyCode::F(4) => {
            app.screen = Screen::AccessControlManager;
            app.screen_history.clear();
            app.screen_history.push(Screen::Dashboard);
        }
        KeyCode::F(5) => {
            app.screen = Screen::KeystoreManager;
            app.screen_history.clear();
            app.screen_history.push(Screen::Dashboard);
        }
        KeyCode::F(6) => {
            app.screen = Screen::BlobBrowser;
            app.screen_history.clear();
            app.screen_history.push(Screen::Dashboard);
        }

        // Voltar
        KeyCode::Esc => {
            app.go_back();
        }

        // Ações contextuais
        _ => handle_screen_key(app, key),
    }
}

fn handle_screen_key(app: &mut App, key: KeyEvent) {
    // Outras telas serão implementadas nas próximas fases
    if app.screen == Screen::Dashboard {
        handle_dashboard_key(app, key);
    }
}

fn handle_dashboard_key(app: &mut App, key: KeyEvent) {
    let filtered_len = app.filtered_indices.len();

    match key.code {
        KeyCode::Up | KeyCode::Char('k') if filtered_len > 0 => {
            let i = app.store_list_state.selected().unwrap_or(0);
            let new_i = if i == 0 { filtered_len - 1 } else { i - 1 };
            app.store_list_state.select(Some(new_i));
        }
        KeyCode::Down | KeyCode::Char('j') if filtered_len > 0 => {
            let i = app.store_list_state.selected().unwrap_or(0);
            let new_i = if i >= filtered_len - 1 { 0 } else { i + 1 };
            app.store_list_state.select(Some(new_i));
        }
        KeyCode::Enter => {
            if let Some(store) = app.selected_store() {
                let screen = match store.store_type.as_str() {
                    "eventlog" => Screen::EventLogInspector {
                        log_name: store.address.clone(),
                    },
                    "keyvalue" => Screen::KeyValueInspector {
                        kv_name: store.address.clone(),
                    },
                    _ => Screen::StoreDetail {
                        store_address: store.address.clone(),
                    },
                };
                app.navigate_to(screen);
            }
        }
        KeyCode::Tab => {
            app.store_filter = app.store_filter.next();
            app.apply_filter();
            // Seleciona o primeiro item ao trocar filtro
            if !app.filtered_indices.is_empty() {
                app.store_list_state.select(Some(0));
            }
        }
        KeyCode::Char('r') => {
            app.has_updates.store(true, Ordering::Relaxed);
        }
        _ => {}
    }
}

// ═══════════════════════════════════════════════════════════
// Background Tasks — escuta eventos do GuardianDB
// ═══════════════════════════════════════════════════════════

fn spawn_event_listeners(db: &GuardianDB, app_updates: Arc<AtomicBool>) {
    let event_bus = db.base().event_bus();

    // Listener: ExchangeHeads (sync com peer)
    {
        let updates = app_updates.clone();
        let bus = event_bus.clone();
        tokio::spawn(async move {
            if let Ok(mut rx) = bus.subscribe::<EventExchangeHeads>().await {
                while let Ok(_event) = rx.recv().await {
                    updates.store(true, Ordering::Relaxed);
                }
            }
        });
    }

    // Listener: PeerConnected
    {
        let updates = app_updates.clone();
        let bus = event_bus.clone();
        tokio::spawn(async move {
            if let Ok(mut rx) = bus.subscribe::<EventPeerConnected>().await {
                while let Ok(_event) = rx.recv().await {
                    updates.store(true, Ordering::Relaxed);
                }
            }
        });
    }

    // Listener: PeerDisconnected
    {
        let updates = app_updates.clone();
        let bus = event_bus.clone();
        tokio::spawn(async move {
            if let Ok(mut rx) = bus.subscribe::<EventPeerDisconnected>().await {
                while let Ok(_event) = rx.recv().await {
                    updates.store(true, Ordering::Relaxed);
                }
            }
        });
    }

    // Listener: SyncCompleted
    {
        let updates = app_updates.clone();
        let bus = event_bus.clone();
        tokio::spawn(async move {
            if let Ok(mut rx) = bus.subscribe::<EventSyncCompleted>().await {
                while let Ok(_event) = rx.recv().await {
                    updates.store(true, Ordering::Relaxed);
                }
            }
        });
    }

    // Listener: SyncError
    {
        let updates = app_updates.clone();
        let bus = event_bus.clone();
        tokio::spawn(async move {
            if let Ok(mut rx) = bus.subscribe::<EventSyncError>().await {
                while let Ok(_event) = rx.recv().await {
                    updates.store(true, Ordering::Relaxed);
                }
            }
        });
    }

    // Listener: StoreUpdated
    {
        let updates = app_updates.clone();
        let bus = event_bus.clone();
        tokio::spawn(async move {
            if let Ok(mut rx) = bus.subscribe::<EventStoreUpdated>().await {
                while let Ok(_event) = rx.recv().await {
                    updates.store(true, Ordering::Relaxed);
                }
            }
        });
    }

    // Listener: DatabaseCreated
    {
        let updates = app_updates.clone();
        let bus = event_bus.clone();
        tokio::spawn(async move {
            if let Ok(mut rx) = bus.subscribe::<EventDatabaseCreated>().await {
                while let Ok(_event) = rx.recv().await {
                    updates.store(true, Ordering::Relaxed);
                }
            }
        });
    }
}

// ═══════════════════════════════════════════════════════════
// Main
// ═══════════════════════════════════════════════════════════

fn parse_args() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    let mut data_dir = PathBuf::from("./guardian_admin_data");

    let mut i = 1;
    while i < args.len() {
        if args[i] == "--data-dir"
            && let Some(dir) = args.get(i + 1)
        {
            data_dir = PathBuf::from(dir);
            i += 2;
        } else {
            i += 1;
        }
    }

    data_dir
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let data_dir = parse_args();
    let log_buffer = LogBuffer::new();

    // Configura tracing para capturar logs na TUI
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "warn,guardian_db=info,iroh=warn".to_string()),
        )
        .with_target(false)
        .with_thread_ids(false)
        .with_thread_names(false)
        .with_writer(log_buffer.clone())
        .with_ansi(false)
        .compact()
        .init();

    let mut terminal = ratatui::init();
    let result = run_app(&mut terminal, log_buffer, data_dir).await;
    ratatui::restore();

    if let Err(ref e) = result {
        eprintln!("Erro: {e}");
    }

    result
}

async fn run_app(
    terminal: &mut ratatui::DefaultTerminal,
    log_buffer: LogBuffer,
    data_dir: PathBuf,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut app = App::new(log_buffer, data_dir.clone());

    // Renderiza tela de conexão
    terminal.draw(|f| ui(f, &app))?;

    // Inicializa IrohClient + GuardianDB
    let config = ClientConfig {
        enable_pubsub: true,
        enable_discovery_mdns: true,
        enable_discovery_n0: true,
        data_store_path: Some(data_dir.join("iroh")),
        ..Default::default()
    };

    let client = IrohClient::new(config).await?;
    app.node_id = client.node_id().to_string();

    let db_options = NewGuardianDBOptions {
        directory: Some(data_dir.join("db")),
        backend: Some(client.backend().clone()),
        ..Default::default()
    };

    let db = GuardianDB::new(client.clone(), Some(db_options)).await?;

    // Registra listeners de eventos
    let updates_flag = app.has_updates.clone();
    spawn_event_listeners(&db, updates_flag);

    // Transição para Dashboard
    app.screen = Screen::Dashboard;
    app.notify_success(format!(
        "Conectado! Node: {}…",
        &app.node_id[..app.node_id.len().min(12)]
    ));

    // Refresh inicial
    app.refresh_stores(&db).await;

    // ─── Event loop principal ─────────────────────────────
    loop {
        terminal.draw(|f| ui(f, &app))?;

        // Poll com timeout de 100ms
        if event::poll(std::time::Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            handle_key(&mut app, key);
        }

        // Tick: limpa notificações expiradas, verifica updates
        app.tick_notifications();

        if app.has_updates.swap(false, Ordering::Relaxed) {
            app.refresh_stores(&db).await;
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}
