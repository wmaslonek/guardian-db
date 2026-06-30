use crate::odm::error::{OdmError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Runtime field types understood by the ODM validator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    Any,
    String,
    Number,
    Boolean,
    Object,
    Array,
    Timestamp,
}

impl FieldType {
    /// Returns whether a JSON value satisfies this field type (`Any` matches all).
    pub fn matches(self, value: &Value) -> bool {
        match self {
            Self::Any => true,
            Self::String | Self::Timestamp => value.is_string(),
            Self::Number => value.is_number(),
            Self::Boolean => value.is_boolean(),
            Self::Object => value.is_object(),
            Self::Array => value.is_array(),
        }
    }
}

/// Declarative constraints for one document field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDefinition {
    pub name: String,
    pub field_type: FieldType,
    pub required: bool,
    pub nullable: bool,
    pub primary_key: bool,
    pub unique: bool,
    pub indexed: bool,
}

impl FieldDefinition {
    /// Creates an unconstrained field definition of the given type.
    pub fn new(name: impl Into<String>, field_type: FieldType) -> Self {
        Self {
            name: name.into(),
            field_type,
            required: false,
            nullable: false,
            primary_key: false,
            unique: false,
            indexed: false,
        }
    }

    /// Marks the field as required (must be present).
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    /// Allows the field to hold an explicit `null` value.
    pub fn nullable(mut self) -> Self {
        self.nullable = true;
        self
    }

    /// Marks the field as the primary key (implies unique, indexed and required).
    pub fn primary_key(mut self) -> Self {
        self.primary_key = true;
        self.unique = true;
        self.indexed = true;
        self.required = true;
        self
    }

    /// Marks the field as unique (implies indexed).
    pub fn unique(mut self) -> Self {
        self.unique = true;
        self.indexed = true;
        self
    }

    /// Marks the field as indexed for faster queries.
    pub fn indexed(mut self) -> Self {
        self.indexed = true;
        self
    }
}

/// Names of the fields used to store creation and update timestamps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimestampDefinition {
    pub created_at: String,
    pub updated_at: String,
}

/// Runtime representation of a typed or schemaless collection model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSchema {
    model_name: String,
    collection: String,
    fields: BTreeMap<String, FieldDefinition>,
    primary_key: String,
    strict: bool,
    auto_generate_primary_key: bool,
    timestamps: Option<TimestampDefinition>,
    version: u32,
}

impl ModelSchema {
    /// Creates a strict schema with the default `_id` primary key.
    pub fn new(model_name: impl Into<String>, collection: impl Into<String>) -> Self {
        Self {
            model_name: model_name.into(),
            collection: collection.into(),
            fields: BTreeMap::new(),
            primary_key: "_id".to_string(),
            strict: true,
            auto_generate_primary_key: true,
            timestamps: None,
            version: 1,
        }
    }

    /// A permissive schema with an automatically generated `_id` primary key.
    pub fn schemaless(collection: impl Into<String>) -> Self {
        let collection = collection.into();
        let mut schema = Self::new("Document", collection);
        schema.strict = false;
        schema.add_field(
            FieldDefinition::new("_id", FieldType::String)
                .primary_key()
                .required(),
        );
        schema
    }

    /// Adds a field definition (builder style) and returns the schema.
    pub fn field(mut self, field: FieldDefinition) -> Self {
        self.add_field(field);
        self
    }

    /// Adds or replaces a field definition, updating the primary key if the
    /// field is marked as one.
    pub fn add_field(&mut self, field: FieldDefinition) {
        if field.primary_key {
            self.primary_key = field.name.clone();
        }
        self.fields.insert(field.name.clone(), field);
    }

    /// Sets the collection name this schema maps to.
    pub fn set_collection(&mut self, collection: impl Into<String>) {
        self.collection = collection.into();
    }

    /// Enables or disables strict mode (rejecting undeclared fields).
    pub fn set_strict(&mut self, strict: bool) {
        self.strict = strict;
    }

    /// Builder-style variant of [`set_strict`](Self::set_strict).
    pub fn strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Enables or disables automatic primary-key generation.
    pub fn set_auto_generate_primary_key(&mut self, enabled: bool) {
        self.auto_generate_primary_key = enabled;
    }

    /// Builder-style variant of [`set_auto_generate_primary_key`](Self::set_auto_generate_primary_key).
    pub fn auto_generate_primary_key(mut self, enabled: bool) -> Self {
        self.auto_generate_primary_key = enabled;
        self
    }

    /// Sets the schema version.
    pub fn set_version(&mut self, version: u32) {
        self.version = version;
    }

    /// Enables timestamp fields, declaring them if they are not already present.
    pub fn enable_timestamps(
        &mut self,
        created_at: impl Into<String>,
        updated_at: impl Into<String>,
    ) {
        let created_at = created_at.into();
        let updated_at = updated_at.into();

        self.fields
            .entry(created_at.clone())
            .or_insert_with(|| FieldDefinition::new(created_at.clone(), FieldType::Timestamp));
        self.fields
            .entry(updated_at.clone())
            .or_insert_with(|| FieldDefinition::new(updated_at.clone(), FieldType::Timestamp));
        self.timestamps = Some(TimestampDefinition {
            created_at,
            updated_at,
        });
    }

    /// Builder-style variant of [`enable_timestamps`](Self::enable_timestamps).
    pub fn timestamps(
        mut self,
        created_at: impl Into<String>,
        updated_at: impl Into<String>,
    ) -> Self {
        self.enable_timestamps(created_at, updated_at);
        self
    }

    /// Returns the model name.
    pub fn model_name(&self) -> &str {
        &self.model_name
    }

    /// Returns the collection name this schema maps to.
    pub fn collection(&self) -> &str {
        &self.collection
    }

    /// Returns the declared field definitions keyed by name.
    pub fn fields(&self) -> &BTreeMap<String, FieldDefinition> {
        &self.fields
    }

    /// Returns the primary-key field name.
    pub fn primary_key(&self) -> &str {
        &self.primary_key
    }

    /// Returns whether strict mode is enabled.
    pub fn strict_mode(&self) -> bool {
        self.strict
    }

    /// Returns whether the primary key is auto-generated when missing.
    pub fn auto_generates_primary_key(&self) -> bool {
        self.auto_generate_primary_key
    }

    /// Returns the timestamp field definition, if timestamps are enabled.
    pub fn timestamp_definition(&self) -> Option<&TimestampDefinition> {
        self.timestamps.as_ref()
    }

    /// Returns the schema version.
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Returns the set of field names that are indexed (including unique and
    /// primary-key fields).
    pub fn indexed_fields(&self) -> BTreeSet<String> {
        self.fields
            .values()
            .filter(|field| field.indexed || field.unique || field.primary_key)
            .map(|field| field.name.clone())
            .collect()
    }

    /// Returns the set of field names that must be unique (including the primary
    /// key).
    pub fn unique_fields(&self) -> BTreeSet<String> {
        self.fields
            .values()
            .filter(|field| field.unique || field.primary_key)
            .map(|field| field.name.clone())
            .collect()
    }

    /// Validates the schema itself: non-empty collection name, at most one
    /// primary key, and consistent primary-key metadata.
    pub fn validate_definition(&self) -> Result<()> {
        if self.collection.trim().is_empty() {
            return Err(OdmError::InvalidSchema(
                "collection name cannot be empty".to_string(),
            ));
        }

        let primary_fields: Vec<&FieldDefinition> = self
            .fields
            .values()
            .filter(|field| field.primary_key)
            .collect();
        if primary_fields.len() > 1 {
            return Err(OdmError::InvalidSchema(
                "only one primary key is supported".to_string(),
            ));
        }

        if let Some(primary) = primary_fields.first()
            && primary.name != self.primary_key
        {
            return Err(OdmError::InvalidSchema(format!(
                "primary key metadata is inconsistent for `{}`",
                primary.name
            )));
        }

        if self.strict && !self.fields.contains_key(&self.primary_key) {
            return Err(OdmError::InvalidSchema(format!(
                "primary key `{}` is not declared as a field",
                self.primary_key
            )));
        }

        Ok(())
    }

    /// Validates a document against the schema: required/nullable rules, field
    /// types, and (in strict mode) rejection of undeclared fields.
    pub fn validate_document(&self, document: &Value) -> Result<()> {
        let object = document.as_object().ok_or_else(|| OdmError::Validation {
            field: "$document".to_string(),
            message: "document must be a JSON object".to_string(),
        })?;

        for field in self.fields.values() {
            match object.get(&field.name) {
                None if field.required => {
                    return Err(OdmError::Validation {
                        field: field.name.clone(),
                        message: "required field is missing".to_string(),
                    });
                }
                Some(Value::Null) if field.required && !field.nullable => {
                    return Err(OdmError::Validation {
                        field: field.name.clone(),
                        message: "required field cannot be null".to_string(),
                    });
                }
                Some(Value::Null) => {}
                Some(value) if !field.field_type.matches(value) => {
                    return Err(OdmError::Validation {
                        field: field.name.clone(),
                        message: format!("expected {:?}", field.field_type),
                    });
                }
                _ => {}
            }
        }

        if self.strict {
            for key in object.keys() {
                // `_id` is an internal storage key when a model uses a custom primary key.
                if key != "_id" && !self.fields.contains_key(key) {
                    return Err(OdmError::Validation {
                        field: key.clone(),
                        message: "field is not declared in the strict schema".to_string(),
                    });
                }
            }
        }

        Ok(())
    }

    /// Returns the document as a mutable JSON object, erroring if it is not one.
    pub(crate) fn object_mut<'a>(
        &self,
        document: &'a mut Value,
    ) -> Result<&'a mut Map<String, Value>> {
        document
            .as_object_mut()
            .ok_or_else(|| OdmError::Validation {
                field: "$document".to_string(),
                message: "document must be a JSON object".to_string(),
            })
    }
}
