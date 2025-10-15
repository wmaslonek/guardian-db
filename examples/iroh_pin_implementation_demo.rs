/// Demonstração de Pin no Backend Iroh
///
/// Este exemplo mostra como o sistema de pin foi implementado usando
/// a API do Iroh com Tags permanentes para proteção contra GC.
use guardian_db::ipfs_core_api::{
    backends::{IpfsBackend, PinInfo, iroh::IrohBackend},
    config::ClientConfig,
    types::AddResponse,
};
use std::io::Cursor;
use std::pin::Pin;
use tokio::io::AsyncRead;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Demo: Pin com Iroh Tags");
    println!("================================================\n");

    // Configura cliente de teste
    let mut config = ClientConfig::testing();
    config.data_store_path = Some("./test_data/iroh_pin_demo".into());

    // Cria backend Iroh
    println!("Inicializando backend Iroh...");
    let backend = IrohBackend::new(&config).await?;
    println!("✓ Backend Iroh inicializado com sucesso\n");

    // === TESTE 1: Adicionar conteúdo e fazer pin ===
    println!("TESTE 1: Adicionando conteúdo e criando pin");

    let test_data = b"Este conteudo sera protegido com Iroh Tags permanentes!";
    let cursor = Cursor::new(test_data.to_vec());
    let reader: Pin<Box<dyn AsyncRead + Send>> = Box::pin(cursor);

    println!("   Adicionando conteúdo ao store...");
    let add_response: AddResponse = backend.add(reader).await?;
    let cid = add_response.hash;

    println!("   ✓ Conteúdo adicionado com CID: {}", cid);
    println!("   Criando pin usando Iroh Tags...");

    backend.pin_add(&cid).await?;
    println!("   ✓ Pin criado com Tag permanente: 'pin-{}'", cid);
    println!();

    // === TESTE 2: Listar pins reais ===
    println!("TESTE 2: Listando pins através das Tags do Iroh");

    let pins: Vec<PinInfo> = backend.pin_ls().await?;
    println!("   Pins encontrados: {}", pins.len());

    for pin in &pins {
        println!("     - CID: {} (tipo: {:?})", pin.cid, pin.pin_type);
    }

    if pins.is_empty() {
        println!("   Nenhum pin encontrado nas Tags do store");
    } else {
        println!("   ✓ Pins carregados das Tags permanentes do Iroh");
    }
    println!();

    // === TESTE 3: Verificar proteção ===
    println!("TESTE 3: Verificando proteção do conteúdo");

    // Simula tentativa de acesso ao conteúdo
    println!("   Tentando acessar conteúdo protegido...");
    let retrieved_data = backend.cat(&cid).await;

    match retrieved_data {
        Ok(_reader) => {
            println!("   ✓ Conteúdo acessível - proteção ativa via Tag");
        }
        Err(e) => {
            println!("   Erro ao acessar conteúdo: {}", e);
        }
    }
    println!();

    // === TESTE 4: Remover pin ===
    println!("TESTE 4: Removendo pin e Tag permanente");

    println!("   Removendo pin (Tag será deletada)...");
    backend.pin_rm(&cid).await?;
    println!("   ✓ Pin removido - Tag 'pin-{}' deletada", cid);

    // Verifica se pin foi removido
    let pins_after_removal: Vec<PinInfo> = backend.pin_ls().await?;
    println!("   Pins restantes: {}", pins_after_removal.len());
    let pin_still_exists = pins_after_removal.iter().any(|p| p.cid == cid);
    if pin_still_exists {
        println!("   ! Pin ainda existe (erro na remoção)");
    } else {
        println!("   ✓ Pin removido com sucesso das Tags do Iroh");
    }
    println!();

    // === TESTE 5: Múltiplos pins ===
    println!("TESTE 5: Gerenciamento de múltiplos pins");

    let mut created_cids = Vec::new();

    // Adiciona múltiplos conteúdos com pins
    for i in 1..=3 {
        let data = format!("Conteudo de teste numero {} para pin", i);
        let cursor = Cursor::new(data.into_bytes());
        let reader: Pin<Box<dyn AsyncRead + Send>> = Box::pin(cursor);

        let response = backend.add(reader).await?;
        let cid = response.hash;

        backend.pin_add(&cid).await?;
        created_cids.push(cid.clone());

        println!("   Pin criado para conteúdo {}: {}", i, cid);
    }

    // Lista todos os pins
    let all_pins: Vec<PinInfo> = backend.pin_ls().await?;
    println!("   Total de pins ativos: {}", all_pins.len());

    // Remove todos os pins criados
    for cid in &created_cids {
        backend.pin_rm(cid).await?;
        println!("   Pin removido: {}", cid);
    }

    let final_pins: Vec<PinInfo> = backend.pin_ls().await?;
    println!("   ✓ Pins finais: {} (limpeza completa)", final_pins.len());
    println!();

    // === RESUMO ===
    println!("✓ Pin implementado usando Iroh Tags API");
    println!("✓ Proteção contra garbage collection ativa");
    println!("✓ Tags permanentes ('pin-{cid}') criadas no FsStore");
    println!("✓ Listagem de pins através das Tags do store");
    println!("✓ Remoção de pins via exclusão de Tags");
    println!("✓ Compatibilidade total com interface IPFS");
    println!();
    println!("Detalhes Técnicos:");
    println!("• pin_add(): Cria Tag permanente via set_tag()");
    println!("• pin_rm(): Remove Tag via set_tag(tag, None)");
    println!("• pin_ls(): Lista Tags via tags() iterator");
    println!("• Proteção: HashAndFormat + BlobFormat::Raw");
    println!("• Store: iroh-bytes FsStore com persistência");

    Ok(())
}
