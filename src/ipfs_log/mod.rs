#![allow(dead_code)]

pub mod entry;
pub mod identity;
pub mod lamport_clock;
pub mod log;

pub mod access_controller;
pub mod identity_provider;
pub mod iface;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ipfs_api_backend_hyper::IpfsClient;
    use serde_json::json;

    use super::entry::Entry;
    use super::entry::EntryOrHash;
    use super::identity::DefaultIdentificator;
    use super::identity::Identificator;
    use super::identity::Identity;
    use super::identity::Signatures;
    use super::lamport_clock::LamportClock;
    use super::log::Log;
    use super::log::LogOptions;

    fn ipfs() -> Arc<IpfsClient> {
        Arc::new(IpfsClient::default())
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

    #[test]
    fn set_id() {
        let log = Log::new(ipfs(), identity1(), LogOptions::new().id("ABC"));
        assert_eq!(log.id(), "ABC");
    }

    #[test]
    fn set_clock_id() {
        let id = identity1();
        let log = Log::new(ipfs(), id.clone(), LogOptions::new().id("ABC"));
        assert_eq!(log.clock().id(), id.pub_key());
    }

    #[test]
    #[ignore] // Requires running IPFS daemon
    fn set_items() {
        let ipfs = ipfs();
        let id = identity1();
        let e1 = Entry::create(
            &ipfs,
            id.clone(),
            "A",
            "entryA",
            &[],
            Some(LamportClock::new("A")),
        );
        let e2 = Entry::create(
            &ipfs,
            id.clone(),
            "A",
            "entryB",
            &[],
            Some(LamportClock::new("B")),
        );
        let e3 = Entry::create(
            &ipfs,
            id.clone(),
            "A",
            "entryC",
            &[],
            Some(LamportClock::new("C")),
        );
        let log = Log::new(ipfs, id, LogOptions::new().id("A").entries(&[e1, e2, e3]));
        assert_eq!(log.len(), 3);
        assert_eq!(log.values()[0].payload(), "entryA");
        assert_eq!(log.values()[1].payload(), "entryB");
        assert_eq!(log.values()[2].payload(), "entryC");
    }

    #[test]
    #[ignore] // Requires running IPFS daemon
    fn set_heads() {
        let ipfs = ipfs();
        let id = identity1();
        let e1 = Entry::create(&ipfs, id.clone(), "A", "entryA", &[], None);
        let e2 = Entry::create(&ipfs, id.clone(), "A", "entryB", &[], None);
        let e3 = Entry::create(&ipfs, id.clone(), "A", "entryC", &[], None);
        let log = Log::new(
            ipfs,
            id,
            LogOptions::new()
                .id("B")
                .entries(&[e1, e2, e3.clone()])
                .heads(std::slice::from_ref(&e3)),
        );
        assert_eq!(log.heads().len(), 1);
        assert_eq!(log.heads()[0].hash(), e3.hash());
    }

    #[test]
    #[ignore] // Requires running IPFS daemon
    fn find_heads() {
        let ipfs = ipfs();
        let id = identity1();
        let e1 = Entry::create(&ipfs, id.clone(), "A", "entryA", &[], None);
        let e2 = Entry::create(&ipfs, id.clone(), "A", "entryB", &[], None);
        let e3 = Entry::create(&ipfs, id.clone(), "A", "entryC", &[], None);
        let log = Log::new(
            ipfs,
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

    #[test]
    #[ignore] // Requires running IPFS daemon
    fn to_string() {
        let expected = "five\n└─four\n  └─three\n    └─two\n      └─one\n";
        let mut log = Log::new(ipfs(), identity1(), LogOptions::new().id("A"));
        log.append("one", None);
        log.append("two", None);
        log.append("three", None);
        log.append("four", None);
        log.append("five", None);
        assert_eq!(log.to_string(), expected);
    }

    //fix comparison after implementing genuine hashing
    #[test]
    #[ignore] // Requires running IPFS daemon
    fn get() {
        let mut log = Log::new(ipfs(), identity1(), LogOptions::new().id("AAA"));
        log.append("one", None);
        assert_eq!(
            log.get(log.values()[0].hash()).unwrap().hash(),
            "QmUMWpQmAqh4Uws3eSWkELeic1eHTnwzZq3p3VGt1D5Cy9"
        );
        assert_eq!(log.get("zero"), None);
    }

    #[test]
    #[ignore] // Requires running IPFS daemon
    fn set_identity() {
        let id1 = identity1();
        let mut log = Log::new(ipfs(), id1.clone(), LogOptions::new().id("AAA"));
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
    #[test]
    #[ignore] // Requires running IPFS daemon
    fn serialize() {
        let expected = json!({
            "id": "AAA",
            "heads": ["QmREuiyqTuJrcWr5BLrT9d9p8dcvdWvwc4JJMHpKcei4Em"],
        })
        .to_string();
        let ipfs = ipfs();
        let mut log = Log::new(ipfs.clone(), identity1(), LogOptions::new().id("AAA"));
        log.append("one", None);
        log.append("two", None);
        log.append("three", None);
        assert_eq!(log.json(), expected);
        //...

        //extra
        let log2 = Log::from_multihash(
            ipfs.clone(),
            identity1(),
            LogOptions::new().id("AAA"),
            "QmREuiyqTuJrcWr5BLrT9d9p8dcvdWvwc4JJMHpKcei4Em",
        );
        assert_eq!(log.snapshot(), log2.snapshot());
        let log3 = Log::from_multihash(
            ipfs,
            identity1(),
            LogOptions::new().id("AAA"),
            "QmQyM8vsbzs6ibi6DFRhXVFurR1AaFyJkPnnvQTeNEdbZu",
        );
        assert_ne!(log.snapshot(), log3.snapshot());
    }

    #[test]
    #[ignore] // Requires running IPFS daemon
    fn values() {
        let mut log = Log::new(ipfs(), identity1(), LogOptions::new());
        assert_eq!(log.len(), 0);
        log.append("hello1", None);
        log.append("hello2", None);
        log.append("hello3", None);
        assert_eq!(log.len(), 3);
        assert_eq!(log.values()[0].payload(), "hello1");
        assert_eq!(log.values()[1].payload(), "hello2");
        assert_eq!(log.values()[2].payload(), "hello3");
    }

    #[test]
    #[ignore]
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

    #[test]
    #[ignore]
    fn log_join() {
        let id = Identity::new("0", "1", Signatures::new("2", "3"));
        let log_id = "xyz";
        let mut x = Log::new(ipfs(), id.clone(), LogOptions::new().id(log_id));
        x.append("to", None);
        x.append("set", None);
        x.append("your", None);
        x.append("global", None);

        let e2 = Entry::new(id.clone(), log_id, "second", &[], None);
        let e3 = Entry::new(id.clone(), log_id, "third", &[], None);
        let e1 = Entry::new(
            id.clone(),
            log_id,
            "first",
            &[EntryOrHash::Entry(&e2), EntryOrHash::Entry(&e3)],
            None,
        );
        let es = &[Arc::new(e1), Arc::new(e2), Arc::new(e3)];
        let mut y = Log::new(ipfs(), id.clone(), LogOptions::new().id(log_id).entries(es));
        y.append("fifth", None);
        y.append("seventh", None);

        let mut z = Log::new(ipfs(), id.clone(), LogOptions::new().id(log_id).entries(es));
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

    /*
    #[test]
    fn ipfs () {
        let client = IpfsClient::default();

        /*
        let data = Cursor::new("tinamämmi");
        let request = client.add(data).map(|r| println!("put {}",r.hash)).map_err(|e| eprintln!("{}",e));
        run(request);

        let mut idpr = DefaultIdentificator::new();
        let id = idpr.create("local_id");
        let mut log = Log::new(id.clone(),LogOptions::new().id("log_id"));
        log.append("first",None);
        log.append("second",None);
        log.append("third",None);
        run(client.add(Cursor::new(log.snapshot())).map(|r| println!("put {}",r.hash)).map_err(|e| eprintln!("{}",e)));
        run(client.object_get("QmQJxSCHs1e3NRSXZeHg86yhHWCTHd26Lx1HFsmqQHkF4R").map(|r| println!("get {}:\n{}","QmQJxSCHs1e3NRSXZeHg86yhHWCTHd26Lx1HFsmqQHkF4R",r.data)).map_err(|e| eprintln!("{}",e)));
        run(client.object_get("QmekwsuyWM853FXJ5SzUW6eQG2LXjp6L8a7xSJf9ZWZW4U").map(|r| println!("get {}:\n{}","QmekwsuyWM853FXJ5SzUW6eQG2LXjp6L8a7xSJf9ZWZW4U",r.data)).map_err(|e| eprintln!("{}",e)));*/

        /*
        let request = client.object_new(Some(ObjectTemplate::UnixFsDir)).map(|r| println!("object: {}",r.hash)).map_err(|e| eprintln!("error: {}",e));
        run(request);*/
    }*/

    #[test]
    fn identities() {
        let mut idpr = DefaultIdentificator::new();
        let id = idpr.create("local_id");

        let key = idpr.get("local_id").unwrap();
        let ext_id = key.pub_key();
        let signer = idpr.get(ext_id).unwrap();
        let pub_key = signer.pub_key();
        assert_eq!(id.pub_key(), signer.pub_key());

        let id_sign = idpr.sign(ext_id, signer);
        assert!(idpr.verify(ext_id, &id_sign, pub_key));
        assert_eq!(id.signatures().id(), id_sign);

        let mut pub_key_id_sign = id.pub_key().to_owned();
        pub_key_id_sign.push_str(&id_sign);
        let pub_key_id_sign_sign = idpr.sign(&pub_key_id_sign, key);
        assert_eq!(id.signatures().pub_key(), pub_key_id_sign_sign);
    }
}
