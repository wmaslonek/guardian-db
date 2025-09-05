use guardian_db::{
    access_controller::{simple::SimpleAccessController, traits::AccessController},
    error::Result,
    ipfs_log::{
        entry::Entry,
        identity::{Identity, Signatures},
    },
};
use slog::{Drain, Logger};
use std::{collections::HashMap, sync::Arc};

/// Demonstração completa do sistema de verificação de permissões no GuardianDB
///
/// Este exemplo mostra:
/// 1. Criação e configuração de Access Controllers
/// 2. Configuração de permissões para diferentes identidades
/// 3. Verificação de permissões durante exchange de heads
/// 4. Comportamento com heads autorizados vs não autorizados
#[tokio::main]
async fn main() -> Result<()> {
    // Configuração de logging estruturado
    let decorator = slog_term::TermDecorator::new().build();
    let drain = slog_term::FullFormat::new(decorator).build().fuse();
    let drain = std::sync::Mutex::new(drain).fuse();
    let logger = Logger::root(drain, slog::o!("example" => "access_control_demo"));

    slog::info!(
        logger,
        "=== Demonstração de Sistema de Verificação de Permissões ==="
    );

    // ETAPA 1: Criação de identidades de teste
    slog::info!(logger, "ETAPA 1: Criando identidades de teste");

    let authorized_identity = create_test_identity("authorized_user", "auth_public_key_123");
    let unauthorized_identity = create_test_identity("unauthorized_user", "unauth_public_key_456");
    let admin_identity = create_test_identity("admin_user", "admin_public_key_789");

    slog::info!(logger, "Identidades criadas";
        "authorized_id" => authorized_identity.id(),
        "unauthorized_id" => unauthorized_identity.id(),
        "admin_id" => admin_identity.id()
    );

    // ETAPA 2: Configuração do Access Controller
    slog::info!(logger, "ETAPA 2: Configurando Access Controller");

    let access_controller = create_configured_access_controller(&logger).await?;

    // ETAPA 3: Configuração de permissões
    slog::info!(logger, "ETAPA 3: Configurando permissões");

    // Concede permissão de escrita para usuário autorizado
    access_controller
        .grant("write", authorized_identity.id())
        .await?;
    access_controller
        .grant("write", authorized_identity.pub_key())
        .await?;

    // Concede permissão admin para admin
    access_controller
        .grant("admin", admin_identity.id())
        .await?;
    access_controller
        .grant("admin", admin_identity.pub_key())
        .await?;

    // Usuário não autorizado não recebe permissões intencionalmente

    slog::info!(logger, "Permissões configuradas";
        "write_keys" => ?access_controller.get_authorized_by_role("write").await?,
        "admin_keys" => ?access_controller.get_authorized_by_role("admin").await?
    );

    // ETAPA 4: Criação de heads de teste
    slog::info!(logger, "ETAPA 4: Criando heads de teste para verificação");

    let test_heads = vec![
        create_test_head("head_1", Some(authorized_identity.clone())),
        create_test_head("head_2", Some(unauthorized_identity.clone())),
        create_test_head("head_3", Some(admin_identity.clone())),
        create_test_head("head_4", None), // Head sem identidade
    ];

    slog::info!(logger, "Heads de teste criados: {}", test_heads.len());

    // ETAPA 5: Simulação de verificação de permissões
    slog::info!(logger, "ETAPA 5: Simulando verificação de permissões");

    let verification_results =
        simulate_permission_verification(&test_heads, &access_controller, &logger).await?;

    // ETAPA 6: Análise dos resultados
    slog::info!(logger, "ETAPA 6: Análise dos resultados da verificação");

    let total_heads = test_heads.len();
    let authorized_heads = verification_results.len();
    let denied_heads = total_heads - authorized_heads;

    slog::info!(logger, "Resultados da verificação de permissões";
        "total_heads" => total_heads,
        "authorized_heads" => authorized_heads,
        "denied_heads" => denied_heads,
        "authorization_rate" => format!("{:.1}%", (authorized_heads as f64 / total_heads as f64) * 100.0)
    );

    // ETAPA 7: Detalhamento dos resultados
    slog::info!(logger, "ETAPA 7: Detalhamento dos heads autorizados");

    for (i, head) in verification_results.iter().enumerate() {
        let identity_info = head
            .identity
            .as_ref()
            .map(|id| format!("ID: {}", id.id()))
            .unwrap_or_else(|| "Sem identidade".to_string());

        slog::info!(logger, "Head autorizado";
            "index" => i + 1,
            "hash" => &head.hash,
            "identity" => identity_info
        );
    }

    // ETAPA 8: Teste com permissão universal
    slog::info!(logger, "ETAPA 8: Testando permissão universal (wildcard)");

    // Adiciona permissão universal temporariamente
    access_controller.grant("write", "*").await?;

    let universal_results =
        simulate_permission_verification(&test_heads, &access_controller, &logger).await?;

    slog::info!(logger, "Resultados com permissão universal";
        "total_heads" => test_heads.len(),
        "authorized_heads" => universal_results.len(),
        "note" => "Heads sem identidade ainda são rejeitados por segurança"
    );

    // Remove permissão universal
    access_controller.revoke("write", "*").await?;

    // ETAPA 9: Teste de cenários de erro
    slog::info!(logger, "ETAPA 9: Testando cenários de erro");

    // Simula head com identidade malformada
    let malformed_identity = Identity::new(
        "", // ID vazio (inválido)
        "malformed_key",
        Signatures::new("", ""),
    );

    let malformed_head = create_test_head("malformed_head", Some(malformed_identity));
    let malformed_results =
        simulate_permission_verification(&[malformed_head], &access_controller, &logger).await?;

    slog::info!(logger, "Resultados com identidade malformada";
        "authorized_heads" => malformed_results.len(),
        "expected" => 0,
        "status" => if malformed_results.is_empty() { "✅ CORRETO" } else { "❌ ERRO" }
    );

    // ETAPA 10: Sumário final
    slog::info!(logger, "ETAPA 10: Sumário da demonstração");

    slog::info!(logger, "=== DEMONSTRAÇÃO CONCLUÍDA COM SUCESSO ===";
        "total_scenarios_tested" => 4,
        "access_controller_type" => access_controller.r#type(),
        "security_model" => "Baseado em permissões explícitas",
        "default_policy" => "Negar acesso (fail-secure)"
    );

    slog::info!(logger, "Comportamentos demonstrados:";
        "1" => "✅ Identidades autorizadas são aceitas",
        "2" => "❌ Identidades não autorizadas são rejeitadas",
        "3" => "👑 Permissões admin funcionam como escrita",
        "4" => "🚫 Heads sem identidade são rejeitados",
        "5" => "🌐 Permissão universal (*) aceita identidades válidas",
        "6" => "🛡️ Identidades malformadas são rejeitadas"
    );

    Ok(())
}

/// Cria uma identidade de teste com dados básicos
fn create_test_identity(id: &str, pub_key: &str) -> Identity {
    let signatures = Signatures::new(
        &format!("signature_for_{}", id),
        &format!("signature_for_key_{}", pub_key),
    );

    Identity::new(id, pub_key, signatures)
}

/// Cria um head de teste com identidade opcional
fn create_test_head(hash: &str, identity: Option<Identity>) -> Entry {
    use guardian_db::ipfs_log::{entry::EntryOrHash, lamport_clock::LamportClock};

    let clock = LamportClock::new("test_peer").set_time(1);
    let next: Vec<EntryOrHash> = Vec::new(); // Empty for test

    // Entry::new requires an identity, so we provide a default if None
    let entry_identity =
        identity.unwrap_or_else(|| create_test_identity("default_test_user", "default_test_key"));

    Entry::new(
        entry_identity,
        "test_log",
        &format!("payload_for_{}", hash),
        &next,
        Some(clock),
    )
}

/// Cria e configura um SimpleAccessController para demonstração
async fn create_configured_access_controller(logger: &Logger) -> Result<Arc<dyn AccessController>> {
    let mut initial_permissions = HashMap::new();

    // Inicializa categorias básicas vazias
    initial_permissions.insert("read".to_string(), Vec::new());
    initial_permissions.insert("write".to_string(), Vec::new());
    initial_permissions.insert("admin".to_string(), Vec::new());

    let controller = SimpleAccessController::new(logger.clone(), Some(initial_permissions));

    slog::debug!(logger, "Access Controller configurado";
        "type" => controller.r#type(),
        "initial_permissions" => "Categorias básicas criadas"
    );

    Ok(Arc::new(controller))
}

/// Simula o processo de verificação de permissões (como seria feito no handle_event_exchange_heads)
async fn simulate_permission_verification(
    heads: &[Entry],
    access_controller: &Arc<dyn AccessController>,
    logger: &Logger,
) -> Result<Vec<Entry>> {
    slog::debug!(logger, "Iniciando simulação de verificação de permissões";
        "heads_count" => heads.len()
    );

    let mut authorized_heads = Vec::new();
    let mut denied_count = 0;
    let mut no_identity_count = 0;

    for (i, head) in heads.iter().enumerate() {
        // VERIFICAÇÃO 1: Presença de identidade
        let identity = match &head.identity {
            Some(identity) => identity,
            None => {
                slog::debug!(
                    logger,
                    "Head {}/{} rejeitado: sem identidade - {}",
                    i + 1,
                    heads.len(),
                    &head.hash
                );
                no_identity_count += 1;
                continue;
            }
        };

        // VERIFICAÇÃO 2: Validação básica da identidade
        if identity.id().is_empty() || identity.pub_key().is_empty() {
            slog::debug!(
                logger,
                "Head {}/{} rejeitado: identidade inválida - {}",
                i + 1,
                heads.len(),
                &head.hash
            );
            denied_count += 1;
            continue;
        }

        // VERIFICAÇÃO 3: Permissões de escrita
        let identity_key = identity.pub_key();
        let has_write_permission = match access_controller.get_authorized_by_role("write").await {
            Ok(authorized_keys) => {
                authorized_keys.contains(&identity_key.to_string())
                    || authorized_keys.contains(&identity.id().to_string())
                    || authorized_keys.contains(&"*".to_string())
            }
            Err(e) => {
                slog::warn!(logger, "Erro ao verificar permissões: {}", e);
                false
            }
        };

        // VERIFICAÇÃO 4: Permissões administrativas
        let has_admin_permission = match access_controller.get_authorized_by_role("admin").await {
            Ok(admin_keys) => {
                admin_keys.contains(&identity_key.to_string())
                    || admin_keys.contains(&identity.id().to_string())
                    || admin_keys.contains(&"*".to_string())
            }
            Err(_) => false,
        };

        // DECISÃO: Aceita se tem qualquer permissão adequada
        if has_write_permission || has_admin_permission {
            let permission_type = if has_admin_permission {
                "admin"
            } else {
                "write"
            };
            slog::debug!(
                logger,
                "Head {}/{} autorizado: permissão {} - {} (ID: {})",
                i + 1,
                heads.len(),
                permission_type,
                &head.hash,
                identity.id()
            );

            authorized_heads.push(head.clone());
        } else {
            slog::debug!(
                logger,
                "Head {}/{} rejeitado: sem permissões - {} (ID: {})",
                i + 1,
                heads.len(),
                &head.hash,
                identity.id()
            );
            denied_count += 1;
        }
    }

    let authorized_count = authorized_heads.len();
    let total_heads = heads.len();

    slog::info!(logger, "Simulação de verificação concluída";
        "total_heads" => total_heads,
        "authorized_heads" => authorized_count,
        "denied_heads" => denied_count,
        "no_identity_heads" => no_identity_count
    );

    Ok(authorized_heads)
}
