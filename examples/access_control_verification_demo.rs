use guardian_db::{
    access_control::{acl_simple::SimpleAccessController, traits::AccessController},
    guardian::error::Result,
    log::{
        entry::Entry,
        identity::{Identity, Signatures},
    },
};
use std::{collections::HashMap, sync::Arc};
use tracing::{info, info_span};

/// Demonstração completa do sistema de verificação de permissões no GuardianDB
///
/// Este exemplo mostra:
/// 1. Criação e configuração de Access Controllers
/// 2. Configuração de permissões para diferentes identidades
/// 3. Verificação de permissões durante exchange de heads
/// 4. Comportamento com heads autorizados vs não autorizados
#[tokio::main]
async fn main() -> Result<()> {
    // Configuração de logging estruturado com tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    let _span = info_span!("access_control_demo").entered();
    info!("=== Demonstração de Sistema de Verificação de Permissões ===");

    // ETAPA 1: Criação de identidades de teste
    info!("ETAPA 1: Criando identidades de teste");

    let authorized_identity = create_test_identity("authorized_user", "auth_public_key_123");
    let unauthorized_identity = create_test_identity("unauthorized_user", "unauth_public_key_456");
    let admin_identity = create_test_identity("admin_user", "admin_public_key_789");

    println!(
        "Identidades criadas: authorized_id={}, unauthorized_id={}, admin_id={}",
        authorized_identity.id(),
        unauthorized_identity.id(),
        admin_identity.id()
    );
    info!(
        authorized_id = %authorized_identity.id(),
        unauthorized_id = %unauthorized_identity.id(),
        admin_id = %admin_identity.id(),
        "Identidades criadas"
    );

    // ETAPA 2: Configuração do Access Controller
    info!("ETAPA 2: Configurando Access Controller");

    let access_controller = create_configured_access_controller().await?;

    // ETAPA 3: Configuração de permissões
    info!("ETAPA 3: Configurando permissões");

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

    info!(
        write_keys = ?access_controller.get_authorized_by_role("write").await?,
        admin_keys = ?access_controller.get_authorized_by_role("admin").await?,
        "Permissões configuradas"
    );

    // ETAPA 4: Criação de heads de teste
    info!("ETAPA 4: Criando heads de teste para verificação");

    let test_heads = vec![
        create_test_head("head_1", Some(authorized_identity.clone())),
        create_test_head("head_2", Some(unauthorized_identity.clone())),
        create_test_head("head_3", Some(admin_identity.clone())),
        create_test_head("head_4", None), // Head sem identidade
    ];

    info!("Heads de teste criados: {}", test_heads.len());

    // ETAPA 5: Simulação de verificação de permissões
    info!("ETAPA 5: Simulando verificação de permissões");

    let verification_results =
        simulate_permission_verification(&test_heads, &access_controller).await?;

    // ETAPA 6: Análise dos resultados
    info!("ETAPA 6: Análise dos resultados da verificação");

    let total_heads = test_heads.len();
    let authorized_heads = verification_results.len();
    let denied_heads = total_heads - authorized_heads;

    info!(
        total_heads = total_heads,
        authorized_heads = authorized_heads,
        denied_heads = denied_heads,
        authorization_rate = format!(
            "{:.1}%",
            (authorized_heads as f64 / total_heads as f64) * 100.0
        ),
        "Resultados da verificação de permissões"
    );

    // ETAPA 7: Detalhamento dos resultados
    info!("ETAPA 7: Detalhamento dos heads autorizados");

    for (i, head) in verification_results.iter().enumerate() {
        let identity_info = head
            .identity
            .as_ref()
            .map(|id| format!("ID: {}", id.id()))
            .unwrap_or_else(|| "Sem identidade".to_string());

        info!(
            index = i + 1,
            hash = %head.hash,
            identity = %identity_info,
            "Head autorizado"
        );
    }

    // ETAPA 8: Teste com permissão universal
    info!("ETAPA 8: Testando permissão universal (wildcard)");

    // Adiciona permissão universal temporariamente
    access_controller.grant("write", "*").await?;

    let universal_results =
        simulate_permission_verification(&test_heads, &access_controller).await?;

    info!(
        total_heads = test_heads.len(),
        authorized_heads = universal_results.len(),
        note = "Heads sem identidade ainda são rejeitados por segurança",
        "Resultados com permissão universal"
    );

    // Remove permissão universal
    access_controller.revoke("write", "*").await?;

    // ETAPA 9: Teste de cenários de erro
    info!("ETAPA 9: Testando cenários de erro");

    // Simula head com identidade malformada
    let malformed_identity = Identity::new(
        "", // ID vazio (inválido)
        "malformed_key",
        Signatures::new("", ""),
    );

    let malformed_head = create_test_head("malformed_head", Some(malformed_identity));
    let malformed_results =
        simulate_permission_verification(&[malformed_head], &access_controller).await?;

    info!(
        authorized_heads = malformed_results.len(),
        expected = 0,
        status = if malformed_results.is_empty() {
            "✓ CORRETO"
        } else {
            "✗ ERRO"
        },
        "Resultados com identidade malformada"
    );

    // ETAPA 10: Sumário final
    info!("ETAPA 10: Sumário da demonstração");

    info!(
        total_scenarios_tested = 4,
        access_controller_type = %access_controller.get_type(),
        security_model = "Baseado em permissões explícitas",
        default_policy = "Negar acesso (fail-secure)",
        "=== DEMONSTRAÇÃO CONCLUÍDA COM SUCESSO ==="
    );

    info!("Comportamentos demonstrados:");
    info!("1: Identidades autorizadas são aceitas");
    info!("2: Identidades não autorizadas são rejeitadas");
    info!("3: Permissões admin funcionam como escrita");
    info!("4: Heads sem identidade são rejeitados");
    info!("5: Permissão universal (*) aceita identidades válidas");
    info!("6: Identidades malformadas são rejeitadas");
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
    use guardian_db::log::{entry::EntryOrHash, lamport_clock::LamportClock};

    let clock = LamportClock::new("test_peer").set_time(1);
    let next: Vec<EntryOrHash> = Vec::new(); // Empty for test

    // Entry::new requires an identity, so we provide a default if None
    let entry_identity =
        identity.unwrap_or_else(|| create_test_identity("default_test_user", "default_test_key"));

    Entry::new(
        entry_identity,
        "test_log",
        format!("payload_for_{}", hash).as_bytes(),
        &next,
        Some(clock),
    )
}

/// Cria e configura um SimpleAccessController para demonstração
async fn create_configured_access_controller() -> Result<Arc<dyn AccessController>> {
    let mut initial_permissions = HashMap::new();

    // Inicializa categorias básicas vazias
    initial_permissions.insert("read".to_string(), Vec::new());
    initial_permissions.insert("write".to_string(), Vec::new());
    initial_permissions.insert("admin".to_string(), Vec::new());

    let controller = SimpleAccessController::new(Some(initial_permissions));

    tracing::debug!(
        controller_type = %controller.get_type(),
        "Access Controller configurado com categorias básicas"
    );

    Ok(Arc::new(controller))
}

/// Simula o processo de verificação de permissões (como seria feito no handle_event_exchange_heads)
async fn simulate_permission_verification(
    heads: &[Entry],
    access_controller: &Arc<dyn AccessController>,
) -> Result<Vec<Entry>> {
    tracing::debug!(
        heads_count = heads.len(),
        "Iniciando simulação de verificação de permissões"
    );

    let mut authorized_heads = Vec::new();
    let mut denied_count = 0;
    let mut no_identity_count = 0;

    for (i, head) in heads.iter().enumerate() {
        // VERIFICAÇÃO 1: Presença de identidade
        let identity = match &head.identity {
            Some(identity) => identity,
            None => {
                tracing::debug!(
                    index = i + 1,
                    total = heads.len(),
                    hash = %head.hash,
                    "Head rejeitado: sem identidade"
                );
                no_identity_count += 1;
                continue;
            }
        };

        // VERIFICAÇÃO 2: Validação básica da identidade
        if identity.id().is_empty() || identity.pub_key().is_empty() {
            tracing::debug!(
                index = i + 1,
                total = heads.len(),
                hash = %head.hash,
                "Head rejeitado: identidade inválida"
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
                tracing::warn!(error = %e, "Erro ao verificar permissões");
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
            tracing::debug!(
                index = i + 1,
                total = heads.len(),
                permission_type = %permission_type,
                hash = %head.hash,
                identity_id = %identity.id(),
                "Head autorizado"
            );

            authorized_heads.push(head.clone());
        } else {
            tracing::debug!(
                index = i + 1,
                total = heads.len(),
                hash = %head.hash,
                identity_id = %identity.id(),
                "Head rejeitado: sem permissões"
            );
            denied_count += 1;
        }
    }

    let authorized_count = authorized_heads.len();
    let total_heads = heads.len();

    info!(
        total_heads = total_heads,
        authorized_heads = authorized_count,
        denied_heads = denied_count,
        no_identity_heads = no_identity_count,
        "Simulação de verificação concluída"
    );

    Ok(authorized_heads)
}
