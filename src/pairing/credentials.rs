use super::*;
use sarmg_client_runtime::{CredentialMutation, CredentialSnapshot};
use std::sync::Arc;

/// This adapter borrows the already-held transaction, never reopening state or
/// acquiring a second lock. Host owns the journal/binding, Foundation the API.
pub(crate) struct HostCredentials<'a> {
    config: &'a ClientConfig,
    store: &'a StateTransaction,
}

impl<'a> HostCredentials<'a> {
    pub(crate) fn new(config: &'a ClientConfig, store: &'a StateTransaction) -> Self {
        Self { config, store }
    }

    fn binding(&self) -> anyhow::Result<Option<ActiveBinding>> {
        let Some(state) = load_state(self.store)? else {
            return Ok(None);
        };
        if matches!(state, StoredPairingState::Activating { .. }) {
            // Rotation owns the durable files until Active is committed last.
            return Ok(None);
        }
        let binding = load_active_binding(self.config, self.store)?;
        if let StoredPairingState::Active { .. } = state {
            let expected = binding_from_active_state(&state)?;
            if binding.as_ref() != Some(&expected) {
                bail!("active credential binding does not match the pairing journal");
            }
        }
        Ok(binding)
    }
}

impl CredentialStore for HostCredentials<'_> {
    type Revision = (Uuid, Uuid);
    type Replacement = StoredPairingState;
    type Error = anyhow::Error;

    fn load(&self) -> anyhow::Result<Option<CredentialSnapshot<Self::Revision>>> {
        if local_auth_state_unlocked(self.store)?
            .is_none_or(|state| state.status != CredentialAuthorization::Authorized)
        {
            return Ok(None);
        }
        let binding = self
            .binding()?
            .context("authorized credential binding is unavailable")?;
        let identity = crate::client_identity::from_state(self.store)?;
        identity.ensure_matches(&crate::client_identity::for_instance(
            &binding.instance_id.to_string(),
        )?)?;
        let secret = match crate::transport::read_secret(self.store, "host token") {
            Ok(secret) => secret,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        Ok(Some(CredentialSnapshot {
            identity,
            revision: (binding.generation, binding.request_id),
            secret: Arc::new(secret),
        }))
    }

    fn replace(&mut self, replacement: Self::Replacement) -> anyhow::Result<()> {
        let StoredPairingState::Activating {
            generation,
            request_id,
            ..
        } = replacement
        else {
            bail!("credential rotation requires an Activating journal");
        };
        match load_state(self.store)? {
            Some(
                current @ StoredPairingState::Activating {
                    generation: current_generation,
                    request_id: current_request,
                    ..
                },
            ) if generation == current_generation && request_id == current_request => {
                // Commit the reloaded durable journal, never a stale caller's token.
                commit_activating_unlocked(self.config, self.store, current).map(|_| ())
            }
            _ => Err(PairingSuperseded.into()),
        }
    }

    fn invalidate(
        &mut self,
        expected: &Self::Revision,
        reason: &str,
    ) -> anyhow::Result<CredentialMutation> {
        let Some(binding) = self.binding()? else {
            return Ok(CredentialMutation::Superseded);
        };
        if (binding.generation, binding.request_id) != *expected {
            return Ok(CredentialMutation::Superseded);
        }
        local_auth_state_unlocked(self.store)?
            .context("current credential authorization state is missing")?;
        persist_auth_state_unlocked(
            self.store,
            &LocalAuthState {
                version: PAIRING_STATE_VERSION,
                status: CredentialAuthorization::ReauthorizationRequired,
                reason: reason.into(),
                changed_at: Utc::now(),
            },
        )?;
        Ok(CredentialMutation::Applied)
    }
}
