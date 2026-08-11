use std::collections::HashMap;

use crate::SessionId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimMode {
    Snapshot,
    Explicit,
}

pub struct ClaimRequest<'a> {
    pub entity_id: &'a str,
    pub owner: SessionId,
    pub registration_id: &'a [u8],
    pub presented_fencing_token: &'a [u8],
    pub client_revision: u64,
    pub new_fencing_token: Vec<u8>,
    pub mode: ClaimMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    Resumed {
        fencing_token: Vec<u8>,
        revision_advanced: bool,
    },
    Acquired {
        fencing_token: Vec<u8>,
        previous: Option<OwnershipState>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnershipState {
    pub entity_id: String,
    pub owner: SessionId,
    pub registration_id: Vec<u8>,
    pub fencing_token: Vec<u8>,
    pub client_revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionOutcome {
    Unchanged,
    Advanced,
}

pub struct OwnershipRegistry {
    maximum_entities: usize,
    maximum_entities_per_owner: usize,
    entries: HashMap<String, OwnershipState>,
}

impl OwnershipRegistry {
    pub fn new(maximum_entities: usize, maximum_entities_per_owner: usize) -> Self {
        Self {
            maximum_entities,
            maximum_entities_per_owner,
            entries: HashMap::new(),
        }
    }

    pub fn claim(&mut self, request: ClaimRequest<'_>) -> Result<ClaimOutcome, OwnershipError> {
        if request.entity_id.is_empty() || request.registration_id.is_empty() {
            return Err(OwnershipError::InvalidClaim);
        }

        if let Some(current) = self.entries.get_mut(request.entity_id) {
            let resumable = current.owner == request.owner
                && current.registration_id == request.registration_id
                && (request.mode == ClaimMode::Explicit
                    || current.fencing_token == request.presented_fencing_token);
            if resumable {
                let revision_advanced = request.client_revision > current.client_revision;
                if revision_advanced {
                    current.client_revision = request.client_revision;
                }
                return Ok(ClaimOutcome::Resumed {
                    fencing_token: current.fencing_token.clone(),
                    revision_advanced,
                });
            }
            if request.mode == ClaimMode::Snapshot {
                return Err(OwnershipError::SnapshotConflict);
            }
        }

        let previous = self.entries.get(request.entity_id).cloned();
        if previous.is_none() && self.entries.len() >= self.maximum_entities {
            return Err(OwnershipError::GlobalLimit);
        }
        let already_owned = self
            .entries
            .values()
            .filter(|state| state.owner == request.owner)
            .count();
        if previous
            .as_ref()
            .is_none_or(|state| state.owner != request.owner)
            && already_owned >= self.maximum_entities_per_owner
        {
            return Err(OwnershipError::OwnerLimit);
        }
        if request.new_fencing_token.is_empty() {
            return Err(OwnershipError::InvalidClaim);
        }
        if self
            .entries
            .values()
            .any(|state| state.fencing_token == request.new_fencing_token)
        {
            return Err(OwnershipError::FencingTokenCollision);
        }

        let state = OwnershipState {
            entity_id: request.entity_id.to_owned(),
            owner: request.owner,
            registration_id: request.registration_id.to_vec(),
            fencing_token: request.new_fencing_token.clone(),
            client_revision: request.client_revision,
        };
        self.entries.insert(request.entity_id.to_owned(), state);
        Ok(ClaimOutcome::Acquired {
            fencing_token: request.new_fencing_token,
            previous,
        })
    }

    pub fn accept_revision(
        &mut self,
        entity_id: &str,
        owner: SessionId,
        fencing_token: &[u8],
        client_revision: u64,
        payload_unchanged: bool,
    ) -> Result<RevisionOutcome, OwnershipError> {
        let Some(current) = self.entries.get_mut(entity_id) else {
            return Err(OwnershipError::NotFound);
        };
        if current.owner != owner || current.fencing_token != fencing_token {
            return Err(OwnershipError::OwnershipLost);
        }
        if client_revision < current.client_revision
            || (client_revision == current.client_revision && !payload_unchanged)
        {
            return Err(OwnershipError::StaleRevision);
        }
        if client_revision == current.client_revision {
            return Ok(RevisionOutcome::Unchanged);
        }
        current.client_revision = client_revision;
        Ok(RevisionOutcome::Advanced)
    }

    pub fn release(
        &mut self,
        entity_id: &str,
        owner: SessionId,
        registration_id: &[u8],
        fencing_token: &[u8],
    ) -> Result<OwnershipState, OwnershipError> {
        let Some(current) = self.entries.get(entity_id) else {
            return Err(OwnershipError::OwnershipLost);
        };
        if current.owner != owner
            || current.registration_id != registration_id
            || current.fencing_token != fencing_token
        {
            return Err(OwnershipError::OwnershipLost);
        }
        self.entries
            .remove(entity_id)
            .ok_or(OwnershipError::OwnershipLost)
    }

    pub fn get(&self, entity_id: &str) -> Option<&OwnershipState> {
        self.entries.get(entity_id)
    }

    pub fn owned_entities(&self, owner: SessionId) -> Vec<String> {
        self.entries
            .values()
            .filter(|state| state.owner == owner)
            .map(|state| state.entity_id.clone())
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OwnershipError {
    #[error("entity_id and registration_id must be present, with a fencing token for acquisition")]
    InvalidClaim,
    #[error("a snapshot cannot replace current ownership")]
    SnapshotConflict,
    #[error("the global ownership limit is reached")]
    GlobalLimit,
    #[error("the ownership limit for this session is reached")]
    OwnerLimit,
    #[error("a generated fencing token collides with live state")]
    FencingTokenCollision,
    #[error("the entity is not registered")]
    NotFound,
    #[error("the ownership capability is stale")]
    OwnershipLost,
    #[error("the entity revision moved backwards or changed in place")]
    StaleRevision,
}

#[cfg(test)]
// Test fixtures use explicit failure messages when their own setup is invalid.
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn claim<'a>(
        entity_id: &'a str,
        owner: SessionId,
        registration_id: &'a [u8],
        presented_fencing_token: &'a [u8],
        new_token: u8,
        mode: ClaimMode,
    ) -> ClaimRequest<'a> {
        ClaimRequest {
            entity_id,
            owner,
            registration_id,
            presented_fencing_token,
            client_revision: 1,
            new_fencing_token: vec![new_token; 32],
            mode,
        }
    }

    #[test]
    fn explicit_takeover_rotates_fencing_and_snapshot_takeover_is_refused() {
        let mut ownership = OwnershipRegistry::new(2, 2);
        let first = ownership
            .claim(claim("entity", 1, b"first", &[], 1, ClaimMode::Explicit))
            .expect("first claim");
        let ClaimOutcome::Acquired {
            fencing_token: first_token,
            ..
        } = first
        else {
            panic!("first claim acquires ownership");
        };

        assert_eq!(
            ownership.claim(claim("entity", 2, b"second", &[], 2, ClaimMode::Snapshot)),
            Err(OwnershipError::SnapshotConflict)
        );
        let second = ownership
            .claim(claim("entity", 2, b"second", &[], 2, ClaimMode::Explicit))
            .expect("explicit takeover");
        let ClaimOutcome::Acquired {
            fencing_token: second_token,
            previous: Some(previous),
        } = second
        else {
            panic!("takeover reports previous ownership");
        };
        assert_eq!(previous.owner, 1);
        assert_ne!(first_token, second_token);
    }

    #[test]
    fn stale_release_and_revision_are_rejected() {
        let mut ownership = OwnershipRegistry::new(1, 1);
        let acquired = ownership
            .claim(claim(
                "entity",
                1,
                b"registration",
                &[],
                1,
                ClaimMode::Explicit,
            ))
            .expect("claim ownership");
        let ClaimOutcome::Acquired { fencing_token, .. } = acquired else {
            panic!("claim acquires ownership");
        };

        assert_eq!(
            ownership.accept_revision("entity", 1, &fencing_token, 2, false),
            Ok(RevisionOutcome::Advanced)
        );
        assert_eq!(
            ownership.accept_revision("entity", 1, &fencing_token, 1, true),
            Err(OwnershipError::StaleRevision)
        );
        assert_eq!(
            ownership.release("entity", 1, b"registration", b"stale"),
            Err(OwnershipError::OwnershipLost)
        );
        assert!(ownership.get("entity").is_some());
    }

    #[test]
    fn limits_are_counted_globally_and_per_owner() {
        let mut ownership = OwnershipRegistry::new(2, 1);
        ownership
            .claim(claim("one", 1, b"one", &[], 1, ClaimMode::Explicit))
            .expect("first claim");
        assert_eq!(
            ownership.claim(claim("two", 1, b"two", &[], 2, ClaimMode::Explicit)),
            Err(OwnershipError::OwnerLimit)
        );
        ownership
            .claim(claim("two", 2, b"two", &[], 2, ClaimMode::Explicit))
            .expect("second owner claim");
        assert_eq!(
            ownership.claim(claim("three", 3, b"three", &[], 3, ClaimMode::Explicit,)),
            Err(OwnershipError::GlobalLimit)
        );
    }
}
