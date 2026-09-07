//! Explicit plaintext serialization only for protected product persistence DTOs.
//! Shared secret types deliberately have no general Serialize implementation.

use std::sync::Arc;

use sarmg_client_secret::SecretString;
use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

pub(crate) fn serialize<S>(secret: &Arc<SecretString>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(secret.expose())
}

pub(crate) fn deserialize<'de, D>(deserializer: D) -> Result<Arc<SecretString>, D::Error>
where
    D: Deserializer<'de>,
{
    // Never propagate serde's unexpected-value diagnostic, which could quote
    // a malformed secret. The state reader also redacts structural JSON errors.
    String::deserialize(deserializer)
        .map(|value| Arc::new(SecretString::new(value)))
        .map_err(|_| D::Error::custom("invalid secret"))
}

/// Normalize the optional environment token without leaving the original
/// environment copy as an ordinary, non-zeroizing String during trimming.
pub(crate) fn trimmed(value: String) -> Option<Arc<SecretString>> {
    let value = SecretString::new(value);
    let trimmed = value.expose().trim();
    if trimmed.is_empty() {
        None
    } else if trimmed.len() == value.expose().len() {
        Some(Arc::new(value))
    } else {
        Some(Arc::new(SecretString::new(trimmed.to_owned())))
    }
}

pub(crate) mod optional {
    use super::*;

    pub(crate) fn serialize<S>(
        secret: &Option<Arc<SecretString>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match secret {
            Some(value) => serializer.serialize_some(value.expose()),
            None => serializer.serialize_none(),
        }
    }

    pub(crate) fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<Option<Arc<SecretString>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<String>::deserialize(deserializer)
            .map(|value| value.map(|value| Arc::new(SecretString::new(value))))
            .map_err(|_| D::Error::custom("invalid optional secret"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_token_normalization_preserves_the_current_optional_semantics() {
        assert!(trimmed(" \t\n".into()).is_none());
        assert_eq!(trimmed(" token \n".into()).unwrap().expose(), "token");
        assert_eq!(trimmed("令牌🔑".into()).unwrap().expose(), "令牌🔑");
    }
}
