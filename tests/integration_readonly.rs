/// Testes de integração: replicação somente-leitura criptograficamente garantida.
///
/// Validam, end-to-end com dois nós reais sobre QUIC, que:
/// - um nó leitor (papel "read") importa o namespace do escritor via DocTicket somente-leitura
///   e recebe as atualizações replicadas;
/// - esse nó leitor **não consegue originar escrita** (put/delete falham localmente), porque
///   não detém o segredo de escrita do namespace.
mod common;

use common::{TestNode, connect_nodes, init_test_logging, wait_for_propagation};
use guardian_db::access_control::manifest::CreateAccessControllerOptions;
use guardian_db::traits::CreateDBOptions;

#[tokio::test]
async fn test_read_only_replica_receives_but_cannot_write() {
    init_test_logging();

    let writer = TestNode::new("ro_writer")
        .await
        .expect("Failed to create writer node");
    let reader = TestNode::new("ro_reader")
        .await
        .expect("Failed to create reader node");

    let writer_peer = writer.iroh.node_id();
    let reader_peer = reader.iroh.node_id();

    // Conecta os nós (popula known_peers e estabelece gossip) ANTES de criar os stores,
    // para que o leitor consiga obter o DocTicket do escritor por troca automática.
    connect_nodes(&writer, &reader)
        .await
        .expect("Failed to connect nodes");

    // ── Escritor: ACL de replicação somente-leitura ──────────────────────────────
    // Apenas o escritor escreve; todos podem ler (read: "*"), o que autoriza o leitor a
    // receber um DocTicket somente-leitura.
    let writer_opts = CreateDBOptions {
        access_controller: Some(Box::new(
            CreateAccessControllerOptions::read_only_replication(vec![
                "writer-only-placeholder".to_string(),
            ]),
        )),
        ..Default::default()
    };
    let wkv = writer
        .db
        .key_value("ro-shared", Some(writer_opts))
        .await
        .expect("Failed to create writer KV store");

    // Dá tempo para o escritor registrar seu provedor de ticket antes do leitor resolver.
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // ── Leitor: aberto como read-only ────────────────────────────────────────────
    // Importa o namespace do escritor (DocTicket de leitura) e NÃO pode criar um próprio.
    let reader_opts = CreateDBOptions {
        read_only: Some(true),
        ..Default::default()
    };
    let rkv = reader
        .db
        .key_value("ro-shared", Some(reader_opts))
        .await
        .expect("Read-only reader should import the writer's namespace");

    // Escritor grava uma chave.
    wkv.put("k1", b"v1".to_vec())
        .await
        .expect("Writer should be able to write");

    // Dispara sincronização explícita em ambos os sentidos.
    writer
        .db
        .connect_to_peer(reader_peer)
        .await
        .expect("writer -> reader connect");
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    reader
        .db
        .connect_to_peer(writer_peer)
        .await
        .expect("reader -> writer connect");

    // O leitor deve convergir e enxergar a chave do escritor (com algumas tentativas).
    let mut got = None;
    for _ in 0..6 {
        wait_for_propagation().await;
        if let Ok(Some(v)) = rkv.get("k1").await {
            got = Some(v);
            break;
        }
    }
    assert_eq!(
        got.as_deref(),
        Some(b"v1".as_ref()),
        "Read-only replica should receive the writer's replicated value"
    );

    // O ponto central: o leitor NÃO pode originar escrita.
    let put_res = rkv.put("k2", b"v2".to_vec()).await;
    assert!(
        put_res.is_err(),
        "Read-only replica must reject local put (no namespace write secret)"
    );

    let del_res = rkv.delete("k1").await;
    assert!(
        del_res.is_err(),
        "Read-only replica must reject local delete"
    );

    // E o estado do escritor não foi afetado por nenhuma tentativa do leitor.
    let writer_state = wkv.all();
    assert_eq!(
        writer_state.get("k1").map(|v| v.as_slice()),
        Some(b"v1".as_ref()),
        "Writer state must be intact"
    );
    assert!(
        !writer_state.contains_key("k2"),
        "Reader must not have been able to inject k2 into the shared store"
    );
}
