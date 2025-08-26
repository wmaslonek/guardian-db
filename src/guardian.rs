// Usando ipfs_api_backend_hyper por compatibilidade com base_guardian
use ipfs_api_backend_hyper::IpfsClient;
use crate::iface::{
    EventLogStore, KeyValueStore, DocumentStore,
    CreateDBOptions
};
use crate::base_guardian::{GuardianDB as BaseGuardianDB, NewGuardianDBOptions};
use crate::error::{GuardianError, Result};

// Em Go, `GuardianDB` embute `BaseGuardianDB`. Em Rust, usamos composição,
// ou seja, a struct `GuardianDB` contém um campo do tipo `BaseGuardianDB`.
pub struct GuardianDB {
    base: BaseGuardianDB,
}

impl GuardianDB {
    // equivalente a NewGuardianDB em go
    pub async fn new(
        ipfs: IpfsClient,
        options: Option<NewGuardianDBOptions>,
    ) -> Result<Self> {
        let odb = BaseGuardianDB::new(ipfs, options).await?;
        Ok(GuardianDB { base: odb })
    }

    // equivalente a Log em go
    pub async fn log(
        &self,
        address: &str,
        options: Option<CreateDBOptions>,
    ) -> Result<Box<dyn EventLogStore<Error = GuardianError>>> {
        // Se `options` for `None`, criamos um valor padrão.
        let mut opts = options.unwrap_or_default();

        // Em Rust, campos opcionais são `Option<T>`, então usamos `Some()`.
        opts.create = Some(true);
        opts.store_type = Some("eventlog".to_string());

        let store = self.base.open(address, opts).await?;

        // Assumimos que o store retornado é o tipo correto
        // Em uma implementação real, seria necessário um downcast mais seguro
        todo!("Implementar conversão segura de Store para EventLogStore")
    }

    // equivalente a KeyValue em go
    pub async fn key_value(
        &self,
        address: &str,
        options: Option<CreateDBOptions>,
    ) -> Result<Box<dyn KeyValueStore<Error = GuardianError>>> {
        let mut opts = options.unwrap_or_default();

        opts.create = Some(true);
        opts.store_type = Some("keyvalue".to_string());

        let store = self.base.open(address, opts).await?;
        
        // Assumimos que o store retornado é o tipo correto
        todo!("Implementar conversão segura de Store para KeyValueStore")
    }

    // equivalente a Docs em go
    pub async fn docs(
        &self,
        address: &str,
        options: Option<CreateDBOptions>,
    ) -> Result<Box<dyn DocumentStore<Error = GuardianError>>> {
        let mut opts = options.unwrap_or_default();

        opts.create = Some(true);
        opts.store_type = Some("docstore".to_string());

        let store = self.base.open(address, opts).await?;

        // Assumimos que o store retornado é o tipo correto
        todo!("Implementar conversão segura de Store para DocumentStore")
    }
}