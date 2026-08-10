//! Runtime-independent Controller synchronization state.
#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::time::{Duration, Instant};

pub type SessionId = u64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileRef {
    profile_id: String,
    schema_version: u32,
    descriptor_digest: String,
}

impl ProfileRef {
    pub fn new(
        profile_id: String,
        schema_version: u32,
        descriptor_digest: String,
    ) -> Result<Self, ProfileError> {
        if profile_id.is_empty() || schema_version == 0 || descriptor_digest.is_empty() {
            return Err(ProfileError::InvalidReference);
        }
        Ok(Self {
            profile_id,
            schema_version,
            descriptor_digest,
        })
    }

    pub fn profile_id(&self) -> &str {
        &self.profile_id
    }

    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    pub fn descriptor_digest(&self) -> &str {
        &self.descriptor_digest
    }
}

pub fn negotiate_profile(
    requested: &ProfileRef,
    supported: &ProfileRef,
) -> Result<ProfileRef, ProfileError> {
    if requested.profile_id != supported.profile_id {
        return Err(ProfileError::UnknownProfile);
    }
    if requested.schema_version != supported.schema_version {
        return Err(ProfileError::UnknownSchemaVersion);
    }
    if requested.descriptor_digest != supported.descriptor_digest {
        return Err(ProfileError::DescriptorMismatch);
    }
    Ok(supported.clone())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProfileError {
    #[error("profile_id, schema_version and descriptor_digest must be present")]
    InvalidReference,
    #[error("the requested Controller profile is not compiled into this server")]
    UnknownProfile,
    #[error("the requested Controller profile schema version is not supported")]
    UnknownSchemaVersion,
    #[error("the requested Controller profile descriptor digest does not match")]
    DescriptorMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCredentials {
    session_token: Vec<u8>,
    resume_token: Vec<u8>,
}

impl SessionCredentials {
    pub fn new(session_token: Vec<u8>, resume_token: Vec<u8>) -> Result<Self, SessionError> {
        if session_token.is_empty() || resume_token.is_empty() {
            return Err(SessionError::EmptyCredential);
        }
        Ok(Self {
            session_token,
            resume_token,
        })
    }
}

pub struct OpenSession<'a> {
    pub controller_id: &'a str,
    pub controller_instance_id: &'a [u8],
    pub resume_token: &'a [u8],
    pub profile: ProfileRef,
    pub credentials: SessionCredentials,
    pub now: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionReady {
    pub session_id: SessionId,
    pub session_token: Vec<u8>,
    pub resume_token: Vec<u8>,
    pub profile: ProfileRef,
    pub resumed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenewOutcome {
    Current,
    ResyncRequired,
}

struct Session {
    controller_id: String,
    controller_instance_id: Vec<u8>,
    session_token: Vec<u8>,
    resume_token: Vec<u8>,
    profile: ProfileRef,
    expires_at: Instant,
    desired_revision: u64,
    needs_resync: bool,
}

pub struct SessionRegistry {
    maximum_sessions: usize,
    lease_duration: Duration,
    sessions: HashMap<SessionId, Session>,
    resume_tokens: HashMap<Vec<u8>, SessionId>,
    next_session_id: SessionId,
}

impl SessionRegistry {
    pub fn new(maximum_sessions: usize, lease_duration: Duration) -> Self {
        Self {
            maximum_sessions,
            lease_duration,
            sessions: HashMap::new(),
            resume_tokens: HashMap::new(),
            next_session_id: 1,
        }
    }

    pub fn open(&mut self, request: OpenSession<'_>) -> Result<SessionReady, SessionError> {
        if request.controller_id.is_empty() || request.controller_instance_id.is_empty() {
            return Err(SessionError::InvalidIdentity);
        }

        let resumed = self
            .resume_tokens
            .get(request.resume_token)
            .copied()
            .filter(|session_id| {
                self.sessions.get(session_id).is_some_and(|session| {
                    session.controller_id == request.controller_id
                        && session.controller_instance_id == request.controller_instance_id
                        && session.profile == request.profile
                        && session.expires_at > request.now
                })
            });

        if let Some(session_id) = resumed {
            if self.session_token_exists(&request.credentials.session_token) {
                return Err(SessionError::CredentialCollision);
            }
            let Some(session) = self.sessions.get_mut(&session_id) else {
                return Err(SessionError::MissingSession(session_id));
            };
            session.session_token = request.credentials.session_token.clone();
            session.expires_at = request.now + self.lease_duration;
            return Ok(SessionReady {
                session_id,
                session_token: session.session_token.clone(),
                resume_token: session.resume_token.clone(),
                profile: session.profile.clone(),
                resumed: true,
            });
        }

        if self.sessions.len() >= self.maximum_sessions {
            return Err(SessionError::ResourceExhausted);
        }
        if self
            .resume_tokens
            .contains_key(&request.credentials.resume_token)
            || self.session_token_exists(&request.credentials.session_token)
        {
            return Err(SessionError::CredentialCollision);
        }
        let session_id = self.next_session_id;
        self.next_session_id = self
            .next_session_id
            .checked_add(1)
            .ok_or(SessionError::SessionIdExhausted)?;
        let session = Session {
            controller_id: request.controller_id.to_owned(),
            controller_instance_id: request.controller_instance_id.to_vec(),
            session_token: request.credentials.session_token.clone(),
            resume_token: request.credentials.resume_token.clone(),
            profile: request.profile.clone(),
            expires_at: request.now + self.lease_duration,
            desired_revision: 0,
            needs_resync: false,
        };
        self.resume_tokens
            .insert(session.resume_token.clone(), session_id);
        self.sessions.insert(session_id, session);
        Ok(SessionReady {
            session_id,
            session_token: request.credentials.session_token,
            resume_token: request.credentials.resume_token,
            profile: request.profile,
            resumed: false,
        })
    }

    pub fn authorize(&self, session_id: SessionId, session_token: &[u8]) -> bool {
        self.sessions
            .get(&session_id)
            .is_some_and(|session| session.session_token == session_token)
    }

    pub fn renew(
        &mut self,
        session_id: SessionId,
        session_token: &[u8],
        desired_revision: u64,
        now: Instant,
    ) -> Result<RenewOutcome, SessionError> {
        if !self.authorize(session_id, session_token) {
            return Err(SessionError::InvalidSessionToken);
        }
        let Some(session) = self.sessions.get_mut(&session_id) else {
            return Err(SessionError::MissingSession(session_id));
        };
        session.expires_at = now + self.lease_duration;
        let requires_resync = session.needs_resync || session.desired_revision != desired_revision;
        session.needs_resync = false;
        Ok(if requires_resync {
            RenewOutcome::ResyncRequired
        } else {
            RenewOutcome::Current
        })
    }

    pub fn advance_desired_revision(&mut self, session_id: SessionId) {
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.desired_revision = session.desired_revision.saturating_add(1);
        }
    }

    pub fn set_desired_revision(&mut self, session_id: SessionId, desired_revision: u64) {
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.desired_revision = desired_revision;
        }
    }

    pub fn mark_resync_required(&mut self, session_id: SessionId) {
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.needs_resync = true;
        }
    }

    pub fn refresh_lease(&mut self, session_id: SessionId, now: Instant) {
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.expires_at = now + self.lease_duration;
        }
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.sessions
            .values()
            .map(|session| session.expires_at)
            .min()
    }

    pub fn expired_sessions(&self, now: Instant) -> Vec<SessionId> {
        self.sessions
            .iter()
            .filter(|(_, session)| session.expires_at <= now)
            .map(|(session_id, _)| *session_id)
            .collect()
    }

    pub fn remove(&mut self, session_id: SessionId) -> bool {
        let Some(session) = self.sessions.remove(&session_id) else {
            return false;
        };
        self.resume_tokens.remove(&session.resume_token);
        true
    }

    pub fn contains(&self, session_id: SessionId) -> bool {
        self.sessions.contains_key(&session_id)
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    fn session_token_exists(&self, token: &[u8]) -> bool {
        self.sessions
            .values()
            .any(|session| session.session_token == token)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    #[error("controller_id and controller_instance_id must not be empty")]
    InvalidIdentity,
    #[error("session and resume credentials must not be empty")]
    EmptyCredential,
    #[error("the Controller session limit is reached")]
    ResourceExhausted,
    #[error("a generated Controller credential collides with live state")]
    CredentialCollision,
    #[error("the Controller session identifier space is exhausted")]
    SessionIdExhausted,
    #[error("Controller session {0} is missing")]
    MissingSession(SessionId),
    #[error("session token is not current")]
    InvalidSessionToken,
}

#[cfg(test)]
// Test fixtures use explicit failure messages when their own setup is invalid.
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn profile(identifier: &str) -> ProfileRef {
        ProfileRef::new(identifier.to_owned(), 1, "digest".to_owned()).expect("valid test profile")
    }

    fn credentials(seed: u8) -> SessionCredentials {
        SessionCredentials::new(vec![seed; 32], vec![seed.saturating_add(1); 32])
            .expect("valid test credentials")
    }

    fn open<'a>(
        resume_token: &'a [u8],
        selected_profile: ProfileRef,
        credentials: SessionCredentials,
        now: Instant,
    ) -> OpenSession<'a> {
        OpenSession {
            controller_id: "controller",
            controller_instance_id: b"instance",
            resume_token,
            profile: selected_profile,
            credentials,
            now,
        }
    }

    #[test]
    fn exact_profile_negotiation_fails_closed() {
        let supported = profile("spaces");
        assert_eq!(
            negotiate_profile(&supported, &supported),
            Ok(supported.clone())
        );
        assert_eq!(
            negotiate_profile(&profile("unknown"), &supported),
            Err(ProfileError::UnknownProfile)
        );

        let wrong_version = ProfileRef::new("spaces".to_owned(), 2, "digest".to_owned())
            .expect("valid alternate version");
        assert_eq!(
            negotiate_profile(&wrong_version, &supported),
            Err(ProfileError::UnknownSchemaVersion)
        );
    }

    #[test]
    fn resume_rotates_only_the_session_capability() {
        let now = Instant::now();
        let mut sessions = SessionRegistry::new(2, Duration::from_secs(30));
        let first = sessions
            .open(open(&[], profile("spaces"), credentials(1), now))
            .expect("create session");
        let resumed = sessions
            .open(open(
                &first.resume_token,
                profile("spaces"),
                credentials(3),
                now + Duration::from_secs(1),
            ))
            .expect("resume session");

        assert!(resumed.resumed);
        assert_eq!(resumed.session_id, first.session_id);
        assert_eq!(resumed.resume_token, first.resume_token);
        assert_ne!(resumed.session_token, first.session_token);
    }

    #[test]
    fn renewals_are_bounded_by_token_revision_and_resync_state() {
        let now = Instant::now();
        let mut sessions = SessionRegistry::new(1, Duration::from_secs(30));
        let ready = sessions
            .open(open(&[], profile("spaces"), credentials(1), now))
            .expect("create session");
        sessions.set_desired_revision(ready.session_id, 4);
        sessions.mark_resync_required(ready.session_id);

        assert_eq!(
            sessions.renew(ready.session_id, &ready.session_token, 4, now),
            Ok(RenewOutcome::ResyncRequired)
        );
        assert_eq!(
            sessions.renew(ready.session_id, &ready.session_token, 4, now),
            Ok(RenewOutcome::Current)
        );
        assert_eq!(
            sessions.renew(ready.session_id, b"stale", 4, now),
            Err(SessionError::InvalidSessionToken)
        );
    }

    #[test]
    fn limits_and_expiry_are_decided_without_a_runtime_clock() {
        let now = Instant::now();
        let mut sessions = SessionRegistry::new(1, Duration::from_secs(30));
        let ready = sessions
            .open(open(&[], profile("spaces"), credentials(1), now))
            .expect("create session");
        assert_eq!(
            sessions.open(open(&[], profile("spaces"), credentials(3), now)),
            Err(SessionError::ResourceExhausted)
        );
        assert_eq!(
            sessions.expired_sessions(now + Duration::from_secs(30)),
            vec![ready.session_id]
        );
    }
}
