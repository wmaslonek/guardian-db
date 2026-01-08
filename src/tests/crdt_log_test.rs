#[cfg(test)]
mod tests {
    use crate::log::Log;
    use crate::log::LogOptions;
    use crate::log::entry::Entry;
    use crate::log::entry::EntryOrHash;
    use crate::log::identity::DefaultIdentificator;
    use crate::log::identity::Identificator;
    use crate::log::identity::Identity;
    use crate::log::identity::Signatures;
    use crate::log::lamport_clock::LamportClock;
    use crate::p2p::network::client::IrohClient;
    use crate::p2p::network::config::ClientConfig;
    use iroh_blobs::Hash;
    use std::sync::Arc;

    async fn client() -> Arc<IrohClient> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let unique_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let counter = COUNTER.fetch_add(1, Ordering::SeqCst);
        let random = rand::random::<u64>();

        let client_config = ClientConfig {
            data_store_path: Some(std::path::PathBuf::from(format!(
                "./tmp/iroh_log_test_{}_{}_{}",
                unique_id, counter, random
            ))),
            ..ClientConfig::development()
        };
        Arc::new(
            IrohClient::new(client_config)
                .await
                .expect("Failed to create Iroh Client"),
        )
    }

    fn identity1() -> Identity {
        Identity::new(
            "userA",
            "public",
            Signatures::new("id_signature", "public_signature"),
        )
    }

    fn identity2() -> Identity {
        Identity::new(
            "userB",
            "public",
            Signatures::new("id_signature", "public_signature"),
        )
    }

    fn identity3() -> Identity {
        Identity::new(
            "userC",
            "public",
            Signatures::new("id_signature", "public_signature"),
        )
    }

    #[tokio::test]
    async fn set_id() {
        let log = Log::new(client().await, identity1(), LogOptions::new().id("ABC"));
        assert_eq!(log.id(), "ABC");
    }

    #[tokio::test]
    async fn set_clock_id() {
        let id = identity1();
        let log = Log::new(client().await, id.clone(), LogOptions::new().id("ABC"));
        assert_eq!(log.clock().id(), id.pub_key());
    }

    #[tokio::test]
    async fn set_items() {
        let client = client().await;
        let id = identity1();
        let e1 = Entry::create(
            &client,
            id.clone(),
            "A",
            b"entryA",
            &[],
            Some(LamportClock::new("A")),
        );
        let e2 = Entry::create(
            &client,
            id.clone(),
            "A",
            b"entryB",
            &[],
            Some(LamportClock::new("B")),
        );
        let e3 = Entry::create(
            &client,
            id.clone(),
            "A",
            b"entryC",
            &[],
            Some(LamportClock::new("C")),
        );
        let log = Log::new(client, id, LogOptions::new().id("A").entries(&[e1, e2, e3]));
        assert_eq!(log.len(), 3);
        assert_eq!(log.values()[0].payload(), b"entryA");
        assert_eq!(log.values()[1].payload(), b"entryB");
        assert_eq!(log.values()[2].payload(), b"entryC");
    }

    #[tokio::test]
    async fn set_heads() {
        let client = client().await;
        let id = identity1();
        let e1 = Entry::create(&client, id.clone(), "A", b"entryA", &[], None);
        let e2 = Entry::create(&client, id.clone(), "A", b"entryB", &[], None);
        let e3 = Entry::create(&client, id.clone(), "A", b"entryC", &[], None);
        let log = Log::new(
            client,
            id,
            LogOptions::new()
                .id("B")
                .entries(&[e1, e2, e3.clone()])
                .heads(std::slice::from_ref(&e3)),
        );
        assert_eq!(log.heads().len(), 1);
        assert_eq!(log.heads()[0].hash(), e3.hash());
    }

    #[tokio::test]
    async fn find_heads() {
        let client = client().await;
        let id = identity1();
        let e1 = Entry::create(&client, id.clone(), "A", b"entryA", &[], None);
        let e2 = Entry::create(&client, id.clone(), "A", b"entryB", &[], None);
        let e3 = Entry::create(&client, id.clone(), "A", b"entryC", &[], None);
        let log = Log::new(
            client,
            id,
            LogOptions::new()
                .id("A")
                .entries(&[e1.clone(), e2.clone(), e3.clone()]),
        );
        assert_eq!(log.heads().len(), 3);
        assert_eq!(log.heads()[2].hash(), e1.hash());
        assert_eq!(log.heads()[1].hash(), e2.hash());
        assert_eq!(log.heads()[0].hash(), e3.hash());
    }

    #[tokio::test]
    async fn to_string() {
        let expected = "five\n└─four\n  └─three\n    └─two\n      └─one\n";
        let mut log = Log::new(client().await, identity1(), LogOptions::new().id("A"));
        log.append("one", None);
        log.append("two", None);
        log.append("three", None);
        log.append("four", None);
        log.append("five", None);
        assert_eq!(log.to_string(), expected);
    }

    //fix comparison after implementing genuine hashing
    #[tokio::test]
    async fn get() {
        let mut log = Log::new(client().await, identity1(), LogOptions::new().id("AAA"));
        log.append("one", None);
        // Valida que podemos recuperar a entrada pelo hash
        let values = log.values();
        let hash = values[0].hash();
        assert!(log.get(hash).is_some());
        // Hash inexistente retorna None
        let fake_hash = Hash::from([0u8; 32]);
        assert_eq!(log.get(&fake_hash), None);
    }

    #[tokio::test]
    async fn set_identity() {
        let id1 = identity1();
        let mut log = Log::new(client().await, id1.clone(), LogOptions::new().id("AAA"));
        log.append("one", None);
        assert_eq!(log.values()[0].clock().id(), id1.pub_key());
        assert_eq!(log.values()[0].clock().time(), 1);
        let id2 = identity2();
        log.set_identity(id2.clone());
        log.append("two", None);
        assert_eq!(log.values()[1].clock().id(), id2.pub_key());
        assert_eq!(log.values()[1].clock().time(), 2);
        let id3 = identity3();
        log.append("three", None);
        assert_eq!(log.values()[2].clock().id(), id3.pub_key());
        assert_eq!(log.values()[2].clock().time(), 3);
    }

    //implement later
    #[test]
    fn has() {}

    //fix comparisons after implementing genuine hashing
    #[tokio::test]
    async fn serialize() {
        let client = client().await;
        let mut log = Log::new(client.clone(), identity1(), LogOptions::new().id("AAA"));
        log.append("one", None);
        log.append("two", None);
        log.append("three", None);

        // Verifica estrutura JSON com o hash atual gerado pelo sistema
        let json_output = log.json();
        let json_value: serde_json::Value = serde_json::from_str(&json_output).unwrap();

        assert_eq!(json_value["id"], "AAA");
        assert!(json_value["heads"].is_array());
        assert_eq!(json_value["heads"].as_array().unwrap().len(), 1);

        // Verifica que o hash é uma string válida (hex ou CID)
        let head_hash = json_value["heads"][0].as_str().unwrap();
        assert!(!head_hash.is_empty());

        // Verifica que json() e buffer() produzem o mesmo conteúdo
        assert_eq!(json_output, String::from_utf8(log.buffer()).unwrap());
    }

    #[tokio::test]
    async fn values() {
        let mut log = Log::new(client().await, identity1(), LogOptions::new());
        assert_eq!(log.len(), 0);
        log.append("hello1", None);
        log.append("hello2", None);
        log.append("hello3", None);
        assert_eq!(log.len(), 3);
        assert_eq!(log.values()[0].payload(), b"hello1");
        assert_eq!(log.values()[1].payload(), b"hello2");
        assert_eq!(log.values()[2].payload(), b"hello3");
    }

    #[test]
    fn test_clock() {
        let mut x = LamportClock::new("0000");
        let y = LamportClock::new("0001");
        let mut z = LamportClock::new("0002");
        assert!(x < y);
        assert!(y < z);
        z.tick();
        x.merge(&z);
        assert!(x > y);
        let w = LamportClock::new("0003").set_time(4);
        assert!(x < w);
        for _ in 0..3 {
            x.tick();
        }
        assert!(x < w);
        x.tick();
        assert!(x > w);
    }

    #[tokio::test]
    async fn log_join() {
        let id = Identity::new("0", "1", Signatures::new("2", "3"));
        let log_id = "xyz";
        let iroh_client = client().await;
        let mut x = Log::new(
            iroh_client.clone(),
            id.clone(),
            LogOptions::new().id(log_id),
        );
        x.append("to", None);
        x.append("set", None);
        x.append("your", None);
        x.append("global", None);

        let e2 = Entry::new(id.clone(), log_id, b"second", &[], None);
        let e3 = Entry::new(id.clone(), log_id, b"third", &[], None);
        let e1 = Entry::new(
            id.clone(),
            log_id,
            b"first",
            &[EntryOrHash::Entry(&e2), EntryOrHash::Entry(&e3)],
            None,
        );
        let es = &[Arc::new(e1), Arc::new(e2), Arc::new(e3)];
        let mut y = Log::new(
            iroh_client.clone(),
            id.clone(),
            LogOptions::new().id(log_id).entries(es),
        );
        y.append("fifth", None);
        y.append("seventh", None);

        let mut z = Log::new(
            iroh_client.clone(),
            id.clone(),
            LogOptions::new().id(log_id).entries(es),
        );
        z.append("fourth", None);
        z.append("sixth", None);
        z.append("eighth", None);

        println!("x:\t\t{}\ny:\t\t{}\nz:\t\t{}", x.all(), y.all(), z.all());

        println!(
            "diff z-y\t{:?}",
            z.diff(&y)
                .iter()
                .map(|x| x.1.hash().to_owned())
                .collect::<Vec<_>>()
        );
        y.join(&z, None);
        println!("\n<<join z+y = y>\n{}>\n", y);
        println!("----\t\ty\t\t----\n{}", y.entries());
        //println!("diff y-z\t{:?}",y.diff(&z).iter().map(|x| x.1.hash().to_owned()).collect::<Vec<_>>());
        //z.join(&y,None);
        //println!("\n<<join y+z = z>\n{}>",z);
        //println!("----\t\tz\t\t----\n{}",z.entries());

        println!(
            "diff z-y\t{:?}",
            z.diff(&y)
                .iter()
                .map(|x| x.1.hash().to_owned())
                .collect::<Vec<_>>()
        );
        y.join(&z, None);
        println!("\n<<join z+y = y>\n{}>\n", y);
        println!("----\t\ty\t\t----\n{}", y.entries());

        println!("y (json)\t{}\n", y.json());
        println!("y (snapshot)\t{}\n", y.snapshot());
        println!("y (buffer)\t{:?}\n", y.buffer());
        assert_eq!(y.json(), String::from_utf8(y.buffer()).unwrap());

        println!(
            "diff y-x\t{:?}",
            y.diff(&x)
                .iter()
                .map(|x| x.1.hash().to_owned())
                .collect::<Vec<_>>()
        );
        x.join(&y, Some(10));
        println!("\n<<join y+x = x>\n{}>\n", x);
        println!("----\t\tx\t\t----\n{}", x.entries());
    }

    #[test]
    fn identities() {
        let mut idpr = DefaultIdentificator::new();
        let id = idpr.create("local_id");

        let key = idpr.get("local_id").unwrap();
        let ext_id = key.pub_key();
        let signer = idpr.get(ext_id).unwrap();
        let pub_key = signer.pub_key();
        assert_eq!(id.pub_key(), signer.pub_key());

        // Verifica que a assinatura do ID é válida
        // A assinatura do ID assina o identity hash (ext_id)
        let id_sign = id.signatures().id();
        assert!(idpr.verify(ext_id, id_sign, pub_key));

        // Verifica que a assinatura da chave pública é válida
        // A assinatura da publicKey assina: identity_hash + identity_type
        let identity_type = "GuardianDB";
        let signed_data = format!("{}{}", ext_id, identity_type);
        let pub_key_sign = id.signatures().pub_key();
        assert!(idpr.verify(&signed_data, pub_key_sign, pub_key));
    }
}
