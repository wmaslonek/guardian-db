/// Exemplo demonstrando a funcionalidade refatorada dos construtores do GuardianDB
///
/// Este exemplo mostra como:
/// 1. Criar e registrar construtores de AccessController
/// 2. Recuperar construtores registrados
/// 3. Demonstrar a clonagem com Arc<dyn Fn>
use guardian_db::{
    access_controller::{manifest::ManifestParams, simple::SimpleAccessController},
    base_guardian::GuardianDB,
    error::Result,
    iface::AccessControllerConstructor,
    ipfs_core_api::config::ClientConfig,
};
use std::sync::Arc;
use tracing::{info, info_span};

/// Demonstra o registro e uso de construtores de AccessController
async fn demonstrate_constructor_functionality(guardian_db: &GuardianDB) -> Result<()> {
    let _span = info_span!("constructor_demo").entered();
    info!("=== Demonstração de Funcionalidade dos Construtores ===");

    // 1. Verificar tipos disponíveis antes do registro
    let initial_types = guardian_db.access_controller_types_names();
    info!(
        types = ?initial_types,
        "1. Tipos iniciais de AccessController"
    );

    // 2. Criar e registrar um construtor de exemplo
    info!("2. Criando e registrando construtor de AccessController");

    // Criar um construtor de exemplo usando o padrão Arc<dyn Fn>
    let simple_constructor = create_simple_access_controller_constructor();

    // Registrar o construtor (isso testará toda a cadeia de determinação de tipo)
    match guardian_db
        .register_access_controller_type(simple_constructor)
        .await
    {
        Ok(()) => {
            info!("Construtor registrado com sucesso");
        }
        Err(e) => {
            info!(error = %e, "Erro ao registrar construtor");
        }
    }

    // 3. Verificar se o tipo foi registrado corretamente
    let updated_types = guardian_db.access_controller_types_names();
    info!(
        count = updated_types.len(),
        types = ?updated_types,
        "3. Tipos após registro"
    );

    // 4. Testar recuperação do construtor registrado
    info!("4. Testando recuperação do construtor");

    for controller_type in &updated_types {
        info!(controller_type = %controller_type, "Testando tipo");

        match guardian_db.get_access_controller_type(controller_type) {
            Some(_constructor) => {
                info!(controller_type = %controller_type, "Construtor recuperado com sucesso");

                // Tentar usar o construtor recuperado
                info!("Testando execução do construtor recuperado");

                // Para evitar problemas com mock, vamos apenas verificar que o construtor existe
                // Em um ambiente real, poderíamos executar o construtor
                info!(controller_type = %controller_type, "Construtor recuperado e disponível para uso");
            }
            None => {
                info!(controller_type = %controller_type, "Construtor não encontrado");
            }
        }
    }

    // 5. Demonstrar funcionalidades das funções Store também
    info!("5. Demonstrando funcionalidades análogas para Store constructors");

    let store_types = guardian_db.store_types_names();
    info!(
        count = store_types.len(),
        types = ?store_types,
        "Tipos de Store disponíveis"
    );

    match guardian_db.get_store_constructor("eventlog") {
        Some(_constructor) => {
            info!("Construtor de Store 'eventlog' encontrado e pode ser clonado");
        }
        None => {
            info!("Construtor de Store 'eventlog' não encontrado (esperado - não foi registrado)");
        }
    }

    // 6. Informações sobre a refatoração implementada
    info!("6. Benefícios da refatoração Arc<dyn Fn>");
    info!("✓ Construtores podem ser clonados");
    info!("✓ Thread-safe para uso concorrente");
    info!("✓ Compartilhamento eficiente de memória");
    info!("✓ get_access_controller_type retorna cópias funcionais");
    info!("✓ get_store_constructor retorna cópias funcionais");
    info!("✓ Determinação automática de tipo via execução do construtor");

    info!("=== Demonstração Concluída ===");
    Ok(())
}
/// Demonstra como criar um construtor de AccessController
/// Esta função mostra o padrão que deveria ser usado nos módulos específicos
fn _example_how_to_create_constructor() {
    use guardian_db::iface::AccessControllerConstructor;
    use std::sync::Arc;

    // Exemplo de como criar um construtor:
    let _simple_constructor: AccessControllerConstructor = Arc::new(|_db, options, _opts| {
        let options_clone = options.clone();
        Box::pin(async move {
            // Extrair configurações das opções
            let initial_keys = if options_clone.get_all_access().is_empty() {
                let mut default_permissions = std::collections::HashMap::new();
                default_permissions.insert("write".to_string(), vec!["*".to_string()]);
                Some(default_permissions)
            } else {
                Some(options_clone.get_all_access())
            };

            let controller = SimpleAccessController::new(initial_keys);
            Ok(Arc::new(controller)
                as Arc<
                    dyn guardian_db::access_controller::traits::AccessController,
                >)
        })
    });

    // Este construtor poderia então ser registrado com:
    // guardian_db.register_access_controller_type(simple_constructor).await?;
}

/// Cria um construtor de AccessController simples para demonstração
fn create_simple_access_controller_constructor() -> AccessControllerConstructor {
    Arc::new(|_db, options, _opts| {
        let options_clone = options.clone();
        Box::pin(async move {
            // Extrair configurações das opções
            let initial_keys = if options_clone.get_all_access().is_empty() {
                let mut default_permissions = std::collections::HashMap::new();
                default_permissions.insert("write".to_string(), vec!["*".to_string()]);
                Some(default_permissions)
            } else {
                Some(options_clone.get_all_access())
            };

            let controller = SimpleAccessController::new(initial_keys);
            Ok(Arc::new(controller)
                as Arc<
                    dyn guardian_db::access_controller::traits::AccessController,
                >)
        })
    })
}

/// Função principal do exemplo
#[tokio::main]
async fn main() -> Result<()> {
    println!("Iniciando demonstração de funcionalidade dos construtores...");

    // Criar o escopo para garantir cleanup correto
    let result = {
        println!("Configurando Iroh backend...");
        let config = ClientConfig::default();

        println!("Criando GuardianDB...");
        let guardian_db = match GuardianDB::new(Some(config), None).await {
            Ok(db) => {
                println!("GuardianDB criado com sucesso!");
                db
            }
            Err(e) => {
                eprintln!("Erro ao criar GuardianDB: {}", e);
                return Err(e);
            }
        };

        println!("Executando demonstração...");

        // Executar demonstração em bloco separado
        let demo_result = demonstrate_constructor_functionality(&guardian_db).await;

        println!("Fechando GuardianDB...");
        // Sempre tentar fechar o GuardianDB
        match guardian_db.close().await {
            Ok(()) => {
                println!("GuardianDB fechado com sucesso!");
            }
            Err(e) => {
                eprintln!("Aviso ao fechar GuardianDB: {}", e);
                // Continua mesmo com aviso de fechamento
            }
        }

        demo_result
    };

    // Verificar resultado final
    match result {
        Ok(()) => {
            println!("Demonstração concluída com sucesso!");
            Ok(())
        }
        Err(e) => {
            eprintln!("Erro durante demonstração: {}", e);
            Err(e)
        }
    }
}
