//! sled-backed persistent store.

use std::path::Path;

use async_trait::async_trait;
use deapbox_core::traits::PersistentStore;
use deapbox_core::types::{AgentId, ChatId, CoreError};
use serde::{Deserialize, Serialize};

const BINDING_PREFIX: &str = "binding:";
const RESUME_PREFIX: &str = "resume:";
const VALUE_VERSION: u8 = 1;

/// Persistent store backed by sled.
///
/// The public API stays semantic: callers read and write chat bindings and
/// resume keys, while the concrete key schema remains private to this module.
pub struct SledStore {
    db: sled::Db,
}

#[derive(Debug, Serialize, Deserialize)]
struct SessionBindingValue {
    version: u8,
    agent_id: AgentId,
}

#[derive(Debug, Serialize, Deserialize)]
struct ResumeKeyValue {
    version: u8,
    key: String,
}

impl SledStore {
    /// Open a sled database at `path`.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::Store` when sled cannot open the database.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, CoreError> {
        let db = sled::open(path).map_err(store_error)?;
        Ok(Self { db })
    }

    /// Create a store from an already-open sled database.
    pub fn from_db(db: sled::Db) -> Self {
        Self { db }
    }

    fn binding_key(chat_id: &ChatId) -> Vec<u8> {
        prefixed_key(BINDING_PREFIX, &chat_id.0)
    }

    fn resume_key(chat_id: &ChatId) -> Vec<u8> {
        prefixed_key(RESUME_PREFIX, &chat_id.0)
    }

    fn flush(&self) -> Result<(), CoreError> {
        self.db.flush().map(|_| ()).map_err(store_error)
    }
}

#[async_trait]
impl PersistentStore for SledStore {
    async fn get_session_binding(&self, chat_id: &ChatId) -> Result<Option<AgentId>, CoreError> {
        let Some(bytes) = self
            .db
            .get(Self::binding_key(chat_id))
            .map_err(store_error)?
        else {
            return Ok(None);
        };

        let value: SessionBindingValue =
            serde_json::from_slice(&bytes).map_err(serialization_error)?;
        if value.version != VALUE_VERSION {
            return Err(CoreError::Store(format!(
                "unsupported session binding value version: {}",
                value.version
            )));
        }

        Ok(Some(value.agent_id))
    }

    async fn set_session_binding(
        &self,
        chat_id: &ChatId,
        agent_id: &AgentId,
    ) -> Result<(), CoreError> {
        let value = SessionBindingValue {
            version: VALUE_VERSION,
            agent_id: agent_id.clone(),
        };
        let bytes = serde_json::to_vec(&value).map_err(serialization_error)?;
        self.db
            .insert(Self::binding_key(chat_id), bytes)
            .map_err(store_error)?;
        self.flush()
    }

    async fn get_resume_key(&self, chat_id: &ChatId) -> Result<Option<String>, CoreError> {
        let Some(bytes) = self
            .db
            .get(Self::resume_key(chat_id))
            .map_err(store_error)?
        else {
            return Ok(None);
        };

        let value: ResumeKeyValue = serde_json::from_slice(&bytes).map_err(serialization_error)?;
        if value.version != VALUE_VERSION {
            return Err(CoreError::Store(format!(
                "unsupported resume key value version: {}",
                value.version
            )));
        }

        Ok(Some(value.key))
    }

    async fn set_resume_key(&self, chat_id: &ChatId, key: &str) -> Result<(), CoreError> {
        let value = ResumeKeyValue {
            version: VALUE_VERSION,
            key: key.to_owned(),
        };
        let bytes = serde_json::to_vec(&value).map_err(serialization_error)?;
        self.db
            .insert(Self::resume_key(chat_id), bytes)
            .map_err(store_error)?;
        self.flush()
    }
}

fn prefixed_key(prefix: &str, chat_id: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(prefix.len() + chat_id.len());
    key.extend_from_slice(prefix.as_bytes());
    key.extend_from_slice(chat_id.as_bytes());
    key
}

fn store_error(error: sled::Error) -> CoreError {
    CoreError::Store(error.to_string())
}

fn serialization_error(error: serde_json::Error) -> CoreError {
    CoreError::Store(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_temp_store() -> (tempfile::TempDir, SledStore) {
        let dir = tempfile::tempdir().expect("create temp dir");
        let store = SledStore::open(dir.path()).expect("open sled store");
        (dir, store)
    }

    #[tokio::test]
    async fn missing_keys_return_none() {
        let (_dir, store) = open_temp_store();
        let chat_id = ChatId("chat-1".to_owned());

        assert_eq!(store.get_session_binding(&chat_id).await.unwrap(), None);
        assert_eq!(store.get_resume_key(&chat_id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn session_binding_round_trips_and_overwrites() {
        let (_dir, store) = open_temp_store();
        let chat_id = ChatId("群聊-α".to_owned());
        let first_agent = AgentId("codex".to_owned());
        let second_agent = AgentId("opencode".to_owned());

        store
            .set_session_binding(&chat_id, &first_agent)
            .await
            .unwrap();
        assert_eq!(
            store.get_session_binding(&chat_id).await.unwrap(),
            Some(first_agent)
        );

        store
            .set_session_binding(&chat_id, &second_agent)
            .await
            .unwrap();
        assert_eq!(
            store.get_session_binding(&chat_id).await.unwrap(),
            Some(second_agent)
        );
    }

    #[tokio::test]
    async fn resume_key_round_trips_and_overwrites() {
        let (_dir, store) = open_temp_store();
        let chat_id = ChatId("oc_中文_chat".to_owned());

        store.set_resume_key(&chat_id, "resume-1").await.unwrap();
        assert_eq!(
            store.get_resume_key(&chat_id).await.unwrap(),
            Some("resume-1".to_owned())
        );

        store.set_resume_key(&chat_id, "resume-2").await.unwrap();
        assert_eq!(
            store.get_resume_key(&chat_id).await.unwrap(),
            Some("resume-2".to_owned())
        );
    }

    #[tokio::test]
    async fn values_survive_database_reopen() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let chat_id = ChatId("chat-reopen".to_owned());
        let agent_id = AgentId("codex".to_owned());

        {
            let store = SledStore::open(dir.path()).expect("open sled store");
            store
                .set_session_binding(&chat_id, &agent_id)
                .await
                .unwrap();
            store.set_resume_key(&chat_id, "resume-key").await.unwrap();
        }

        let reopened = SledStore::open(dir.path()).expect("reopen sled store");
        assert_eq!(
            reopened.get_session_binding(&chat_id).await.unwrap(),
            Some(agent_id)
        );
        assert_eq!(
            reopened.get_resume_key(&chat_id).await.unwrap(),
            Some("resume-key".to_owned())
        );
    }
}
