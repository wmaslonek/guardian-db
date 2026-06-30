use crate::odm::error::{OdmError, Result};
use crate::odm::index::{IndexCatalog, IndexMetadata};
use crate::odm::model::Model;
use crate::odm::query::matches_query;
use crate::odm::schema::ModelSchema;
use crate::odm::storage::CollectionStorage;
use crate::odm::transaction::{ConsistencyLevel, TransactionContext, WriteOptions};
use chrono::{SecondsFormat, Utc};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::marker::PhantomData;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

/// A primary-key value used to look documents up by id. Convertible from common
/// scalar types (`String`, `&str`, integers, `bool`) and from `serde_json::Value`.
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentId(Value);

impl DocumentId {
    /// Consumes the id and returns the underlying JSON value.
    ///
    pub fn into_value(self) -> Value {
        self.0
    }
}

impl From<Value> for DocumentId {
    fn from(value: Value) -> Self {
        Self(value)
    }
}

impl From<String> for DocumentId {
    fn from(value: String) -> Self {
        Self(Value::String(value))
    }
}

impl From<&str> for DocumentId {
    fn from(value: &str) -> Self {
        Self(Value::String(value.to_string()))
    }
}

impl From<i64> for DocumentId {
    fn from(value: i64) -> Self {
        Self(Value::from(value))
    }
}

impl From<i32> for DocumentId {
    fn from(value: i32) -> Self {
        Self(Value::from(value))
    }
}

impl From<u64> for DocumentId {
    fn from(value: u64) -> Self {
        Self(Value::from(value))
    }
}

impl From<u32> for DocumentId {
    fn from(value: u32) -> Self {
        Self(Value::from(value))
    }
}

impl From<bool> for DocumentId {
    fn from(value: bool) -> Self {
        Self(Value::from(value))
    }
}

#[derive(Debug, Default)]
struct CollectionState {
    documents: BTreeMap<String, Value>,
    indexes: IndexCatalog,
}

/// Mongoose-style dynamic collection over a GuardianDB document store.
///
/// Each mutation refreshes the locally replicated view and holds a collection
/// mutex across validation, unique-index maintenance, and persistence. This
/// gives atomic validation/write behavior within one process. Distributed
/// peers continue to converge according to GuardianDB's underlying CRDT rules.
#[derive(Clone)]
pub struct Collection {
    name: Arc<str>,
    schema: Arc<ModelSchema>,
    storage: Arc<dyn CollectionStorage>,
    state: Arc<Mutex<CollectionState>>,
}

impl Collection {
    /// Creates a collection bound to `schema` and `storage`, validating the
    /// schema and loading the initial replicated view.
    pub async fn new(
        name: impl Into<String>,
        mut schema: ModelSchema,
        storage: Arc<dyn CollectionStorage>,
    ) -> Result<Self> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(OdmError::InvalidSchema(
                "collection name cannot be empty".to_string(),
            ));
        }
        schema.set_collection(name.clone());
        schema.validate_definition()?;

        let collection = Self {
            name: Arc::from(name),
            schema: Arc::new(schema),
            storage,
            state: Arc::new(Mutex::new(CollectionState::default())),
        };
        {
            let mut state = collection.state.lock().await;
            collection.refresh_locked(&mut state).await?;
        }
        Ok(collection)
    }

    /// Creates a permissive (schemaless) collection with an auto-generated
    /// `_id` primary key.
    pub async fn schemaless(
        name: impl Into<String>,
        storage: Arc<dyn CollectionStorage>,
    ) -> Result<Self> {
        let name = name.into();
        Self::new(name.clone(), ModelSchema::schemaless(name), storage).await
    }

    /// Returns the collection name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the collection's schema.
    pub fn schema(&self) -> &ModelSchema {
        &self.schema
    }

    /// Returns metadata describing the collection's indexes.
    pub fn indexes(&self) -> Vec<IndexMetadata> {
        IndexCatalog::metadata(&self.schema)
    }

    /// Starts a new local-atomic transaction context.
    pub fn begin_transaction(&self) -> TransactionContext {
        TransactionContext::local()
    }

    /// Inserts a single document with default write options.
    pub async fn insert_one(&self, document: Value) -> Result<Value> {
        self.insert_one_with_options(document, WriteOptions::default())
            .await
    }

    /// Inserts a single document, validating it and maintaining indexes. Fails
    /// if the primary key already exists.
    pub async fn insert_one_with_options(
        &self,
        document: Value,
        options: WriteOptions,
    ) -> Result<Value> {
        validate_write_options(&options)?;
        let mut state = self.state.lock().await;
        self.refresh_locked(&mut state).await?;

        let (id, prepared) = self.prepare_insert(document)?;
        if state.documents.contains_key(&id) {
            return Err(OdmError::DuplicateKey {
                field: self.schema.primary_key().to_string(),
                value: id,
            });
        }

        let mut candidate = state.documents.clone();
        candidate.insert(id.clone(), prepared.clone());
        let indexes = IndexCatalog::rebuild(&self.schema, &candidate)?;

        self.storage.write_one(&id, &prepared).await?;
        state.documents = candidate;
        state.indexes = indexes;
        Ok(self.external_document(prepared))
    }

    /// Validates an entire batch before any write is attempted.
    pub async fn insert(&self, documents: Vec<Value>) -> Result<Vec<Value>> {
        self.insert_with_options(documents, WriteOptions::default())
            .await
    }

    /// Inserts a batch of documents with explicit options. The whole batch is
    /// validated and index-checked before any write is committed.
    pub async fn insert_with_options(
        &self,
        documents: Vec<Value>,
        options: WriteOptions,
    ) -> Result<Vec<Value>> {
        validate_write_options(&options)?;
        if documents.is_empty() {
            return Ok(Vec::new());
        }

        let mut state = self.state.lock().await;
        self.refresh_locked(&mut state).await?;

        let mut prepared = Vec::with_capacity(documents.len());
        let mut candidate = state.documents.clone();
        for document in documents {
            let (id, document) = self.prepare_insert(document)?;
            if candidate.insert(id.clone(), document.clone()).is_some() {
                return Err(OdmError::DuplicateKey {
                    field: self.schema.primary_key().to_string(),
                    value: id,
                });
            }
            prepared.push((id, document));
        }
        let indexes = IndexCatalog::rebuild(&self.schema, &candidate)?;

        self.storage.write_many(&prepared).await?;
        state.documents = candidate;
        state.indexes = indexes;
        Ok(prepared
            .into_iter()
            .map(|(_, document)| self.external_document(document))
            .collect())
    }

    /// Returns the first document matching `query`, if any.
    pub async fn find_one(&self, query: Value) -> Result<Option<Value>> {
        Ok(self.find(query).await?.into_iter().next())
    }

    /// Returns all documents matching `query`, using indexes to narrow the scan
    /// when possible.
    pub async fn find(&self, query: Value) -> Result<Vec<Value>> {
        let mut state = self.state.lock().await;
        self.refresh_locked(&mut state).await?;

        let candidate_ids = state.indexes.candidates(&query)?;
        let iter: Box<dyn Iterator<Item = (&String, &Value)> + '_> = match &candidate_ids {
            Some(ids) => Box::new(
                ids.iter()
                    .filter_map(|id| state.documents.get_key_value(id)),
            ),
            None => Box::new(state.documents.iter()),
        };

        let mut matches = Vec::new();
        for (_, document) in iter {
            if matches_query(document, &query)? {
                matches.push(self.external_document(document.clone()));
            }
        }
        Ok(matches)
    }

    /// Returns the document with the given primary-key id, if it exists.
    pub async fn find_by_id(&self, id: impl Into<DocumentId>) -> Result<Option<Value>> {
        let id = canonical_value(&id.into().into_value())?;
        let mut state = self.state.lock().await;
        self.refresh_locked(&mut state).await?;
        Ok(state
            .documents
            .get(&id)
            .cloned()
            .map(|document| self.external_document(document)))
    }

    /// Updates the first document matching `query` and returns its new value.
    pub async fn update(&self, query: Value, operations: Value) -> Result<Option<Value>> {
        self.update_with_options(query, operations, WriteOptions::default())
            .await
    }

    /// Updates the first document matching `query` using the given operators and
    /// write options, re-validating it and refreshing indexes.
    pub async fn update_with_options(
        &self,
        query: Value,
        operations: Value,
        options: WriteOptions,
    ) -> Result<Option<Value>> {
        validate_write_options(&options)?;
        let mut state = self.state.lock().await;
        self.refresh_locked(&mut state).await?;

        let candidate_ids = state.indexes.candidates(&query)?;
        let matched_id = match candidate_ids {
            Some(ids) => {
                let mut found = None;
                for id in ids {
                    let Some(document) = state.documents.get(&id) else {
                        continue;
                    };
                    if matches_query(document, &query)? {
                        found = Some(id);
                        break;
                    }
                }
                found
            }
            None => {
                let mut found = None;
                for (id, document) in &state.documents {
                    if matches_query(document, &query)? {
                        found = Some(id.clone());
                        break;
                    }
                }
                found
            }
        };

        let Some(id) = matched_id else {
            return Ok(None);
        };
        let mut document = state
            .documents
            .get(&id)
            .cloned()
            .expect("matched document must exist");

        let immutable = BTreeSet::from([self.schema.primary_key().to_string(), "_id".to_string()]);
        let changed = crate::odm::update::apply_update(&mut document, &operations, &immutable)?;
        if !changed {
            return Ok(Some(self.external_document(document)));
        }

        self.apply_update_timestamp(&mut document)?;
        self.schema.validate_document(&document)?;
        let updated_id = self.document_id(&document)?;
        if updated_id != id {
            return Err(OdmError::ImmutableField(
                self.schema.primary_key().to_string(),
            ));
        }

        let mut candidate = state.documents.clone();
        candidate.insert(id.clone(), document.clone());
        let indexes = IndexCatalog::rebuild(&self.schema, &candidate)?;

        self.storage.write_one(&id, &document).await?;
        state.documents = candidate;
        state.indexes = indexes;
        Ok(Some(self.external_document(document)))
    }

    /// Reloads every document from storage into the local view, validating each
    /// one and rebuilding the index catalog. Requires the state lock to be held.
    async fn refresh_locked(&self, state: &mut CollectionState) -> Result<()> {
        let mut documents = BTreeMap::new();
        for mut document in self.storage.load_all().await? {
            self.schema.validate_document(&document)?;
            let id = self.document_id(&document)?;
            if self.schema.primary_key() != "_id" {
                let object = self.schema.object_mut(&mut document)?;
                object
                    .entry("_id".to_string())
                    .or_insert_with(|| Value::String(id.clone()));
            }
            if documents.insert(id.clone(), document).is_some() {
                return Err(OdmError::DuplicateKey {
                    field: self.schema.primary_key().to_string(),
                    value: id,
                });
            }
        }
        state.indexes = IndexCatalog::rebuild(&self.schema, &documents)?;
        state.documents = documents;
        Ok(())
    }

    /// Prepares a document for insertion: fills in an auto-generated primary key
    /// and timestamps when applicable, mirrors the id into `_id`, validates it,
    /// and returns the canonical id alongside the prepared document.
    fn prepare_insert(&self, mut document: Value) -> Result<(String, Value)> {
        let now = now_timestamp();
        let primary_key = self.schema.primary_key().to_string();

        {
            let object = self.schema.object_mut(&mut document)?;
            let missing_primary = object.get(&primary_key).is_none_or(|value| value.is_null());
            if missing_primary {
                if self.schema.auto_generates_primary_key() {
                    object.insert(
                        primary_key.clone(),
                        Value::String(Uuid::new_v4().to_string()),
                    );
                } else {
                    return Err(OdmError::Validation {
                        field: primary_key.clone(),
                        message: "primary key is required".to_string(),
                    });
                }
            }

            if let Some(timestamps) = self.schema.timestamp_definition() {
                if object
                    .get(&timestamps.created_at)
                    .is_none_or(Value::is_null)
                {
                    object.insert(timestamps.created_at.clone(), Value::String(now.clone()));
                }
                object.insert(timestamps.updated_at.clone(), Value::String(now));
            }
        }

        let id = self.document_id(&document)?;
        if self.schema.primary_key() != "_id" {
            self.schema
                .object_mut(&mut document)?
                .insert("_id".to_string(), Value::String(id.clone()));
        }
        self.schema.validate_document(&document)?;
        Ok((id, document))
    }

    /// Sets the `updated_at` timestamp field if the schema enables timestamps.
    fn apply_update_timestamp(&self, document: &mut Value) -> Result<()> {
        let Some(timestamps) = self.schema.timestamp_definition() else {
            return Ok(());
        };
        self.schema.object_mut(document)?.insert(
            timestamps.updated_at.clone(),
            Value::String(now_timestamp()),
        );
        Ok(())
    }

    /// Extracts the canonical string id from a document's primary-key field.
    fn document_id(&self, document: &Value) -> Result<String> {
        let value = document
            .as_object()
            .and_then(|object| object.get(self.schema.primary_key()))
            .ok_or_else(|| OdmError::Validation {
                field: self.schema.primary_key().to_string(),
                message: "primary key is missing".to_string(),
            })?;
        canonical_value(value)
    }

    /// Returns the externally visible form of a document, stripping the internal
    /// `_id` mirror when the schema uses a custom primary key.
    fn external_document(&self, mut document: Value) -> Value {
        if self.schema.primary_key() != "_id"
            && let Some(object) = document.as_object_mut()
        {
            object.remove("_id");
        }
        document
    }
}

/// Converts a scalar JSON value (string, number, or bool) into its canonical
/// string id form, rejecting other types.
fn canonical_value(value: &Value) -> Result<String> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Number(value) => Ok(value.to_string()),
        Value::Bool(value) => Ok(value.to_string()),
        _ => Err(OdmError::Validation {
            field: "$id".to_string(),
            message: "primary keys must be strings, numbers, or booleans".to_string(),
        }),
    }
}

/// Returns the current UTC time as an RFC 3339 string with millisecond precision.
fn now_timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Rejects write options that request a not-yet-supported consistency level
/// (currently only `LocalAtomic` is available).
fn validate_write_options(options: &WriteOptions) -> Result<()> {
    if options
        .transaction
        .as_ref()
        .is_some_and(|transaction| transaction.consistency == ConsistencyLevel::Replicated)
    {
        return Err(OdmError::UnsupportedConsistency(
            "replicated transactions require a distributed coordinator; local atomic writes are available today"
                .to_string(),
        ));
    }
    Ok(())
}

/// Strongly typed wrapper around [`Collection`].
#[derive(Clone)]
pub struct TypedCollection<M: Model> {
    inner: Collection,
    marker: PhantomData<fn() -> M>,
}

impl<M: Model> TypedCollection<M> {
    /// Creates a typed collection from the model's derived schema.
    pub async fn new(storage: Arc<dyn CollectionStorage>) -> Result<Self> {
        let schema = M::schema();
        let name = schema.collection().to_string();
        Ok(Self {
            inner: Collection::new(name, schema, storage).await?,
            marker: PhantomData,
        })
    }

    /// Returns the underlying untyped collection.
    pub fn collection(&self) -> &Collection {
        &self.inner
    }

    /// Returns the model's schema.
    pub fn schema(&self) -> &ModelSchema {
        self.inner.schema()
    }

    /// Starts a new local-atomic transaction context.
    pub fn begin_transaction(&self) -> TransactionContext {
        self.inner.begin_transaction()
    }

    /// Inserts a single typed model with default write options.
    pub async fn insert_one(&self, model: M) -> Result<M> {
        self.insert_one_with_options(model, WriteOptions::default())
            .await
    }

    /// Inserts a single typed model with explicit write options, returning the
    /// stored model (with any generated id/timestamps populated).
    pub async fn insert_one_with_options(&self, model: M, options: WriteOptions) -> Result<M> {
        let value = serde_json::to_value(model)?;
        let inserted = self.inner.insert_one_with_options(value, options).await?;
        Ok(serde_json::from_value(inserted)?)
    }

    /// Inserts a batch of typed models with default write options.
    pub async fn insert(&self, models: Vec<M>) -> Result<Vec<M>> {
        self.insert_with_options(models, WriteOptions::default())
            .await
    }

    /// Inserts a batch of typed models with explicit write options.
    pub async fn insert_with_options(
        &self,
        models: Vec<M>,
        options: WriteOptions,
    ) -> Result<Vec<M>> {
        let values = models
            .into_iter()
            .map(serde_json::to_value)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        self.inner
            .insert_with_options(values, options)
            .await?
            .into_iter()
            .map(serde_json::from_value)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Returns the first model matching `query`, deserialized into `M`.
    pub async fn find_one<Q: Serialize>(&self, query: Q) -> Result<Option<M>> {
        self.inner
            .find_one(serde_json::to_value(query)?)
            .await?
            .map(serde_json::from_value)
            .transpose()
            .map_err(Into::into)
    }

    /// Returns all models matching `query`, deserialized into `M`.
    pub async fn find<Q: Serialize>(&self, query: Q) -> Result<Vec<M>> {
        self.inner
            .find(serde_json::to_value(query)?)
            .await?
            .into_iter()
            .map(serde_json::from_value)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Returns the model with the given primary-key id, deserialized into `M`.
    pub async fn find_by_id(&self, id: impl Into<DocumentId>) -> Result<Option<M>> {
        self.inner
            .find_by_id(id)
            .await?
            .map(serde_json::from_value)
            .transpose()
            .map_err(Into::into)
    }

    /// Updates the first model matching `query` with default write options.
    pub async fn update<Q: Serialize, U: Serialize>(
        &self,
        query: Q,
        operations: U,
    ) -> Result<Option<M>> {
        self.update_with_options(query, operations, WriteOptions::default())
            .await
    }

    /// Updates the first model matching `query` with explicit write options,
    /// returning the updated model.
    pub async fn update_with_options<Q: Serialize, U: Serialize>(
        &self,
        query: Q,
        operations: U,
        options: WriteOptions,
    ) -> Result<Option<M>> {
        self.inner
            .update_with_options(
                serde_json::to_value(query)?,
                serde_json::to_value(operations)?,
                options,
            )
            .await?
            .map(serde_json::from_value)
            .transpose()
            .map_err(Into::into)
    }
}
