use guardian_db::ipfs_core_api::{
    backends::{IpfsBackend, IrohBackend},
    config::ClientConfig,
};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Teste Simples de Persistência FsStore");

    // Configura diretório temporário para teste
    let temp_dir = std::env::temp_dir().join("guardian_fsstore_simple_test");
    std::fs::create_dir_all(&temp_dir)?;

    println!("Diretório de teste: {}", temp_dir.display());

    // Configura cliente IPFS Iroh com FsStore
    let config = ClientConfig {
        data_store_path: Some(temp_dir.clone()),
        enable_pubsub: false,
        enable_swarm: false,
        ..Default::default()
    };

    // Cria backend Iroh
    let iroh_backend = Arc::new(IrohBackend::new(&config).await?);

    println!("✓ Backend Iroh criado com sucesso");

    // Teste básico: Adicionar e recuperar conteúdo
    let test_data = b"Hello, FsStore!";
    println!("Adicionando conteúdo...");

    let data_reader = Box::pin(std::io::Cursor::new(test_data.to_vec()));
    let add_response = iroh_backend.add(data_reader).await?;
    println!("✓ Conteúdo adicionado com CID: {}", add_response.hash);

    println!("Recuperando conteúdo...");
    let mut content_stream = iroh_backend.cat(&add_response.hash).await?;
    let mut recovered_data = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut content_stream, &mut recovered_data)
        .await
        .expect("Erro ao ler conteúdo recuperado");

    if recovered_data == test_data {
        println!("✓ Conteúdo recuperado com sucesso!");
    } else {
        println!("Conteúdo não corresponde");
    }

    // Verificar persistência no filesystem
    if temp_dir.exists() {
        println!("✓ Diretório de dados foi criado: {}", temp_dir.display());

        // Lista conteúdo do diretório
        if let Ok(entries) = std::fs::read_dir(&temp_dir) {
            let count = entries.count();
            println!("   - {} arquivos/diretórios criados", count);
        }
    } else {
        println!("Diretório de dados não foi encontrado");
    }

    println!("Teste concluído!");

    Ok(())
}
