use std::collections::{HashMap, VecDeque};

use crate::SessionId;

pub struct ReliableCommands<Output> {
    maximum_completed_per_session: usize,
    sessions: HashMap<SessionId, SessionLog<Output>>,
}

struct SessionLog<Output> {
    completed: HashMap<Vec<u8>, Output>,
    order: VecDeque<Vec<u8>>,
}

impl<Output: Clone> ReliableCommands<Output> {
    pub fn new(maximum_completed_per_session: usize) -> Self {
        Self {
            maximum_completed_per_session,
            sessions: HashMap::new(),
        }
    }

    pub fn attach_session(&mut self, session_id: SessionId) {
        self.sessions
            .entry(session_id)
            .or_insert_with(|| SessionLog {
                completed: HashMap::new(),
                order: VecDeque::new(),
            });
    }

    pub fn replay(&self, session_id: SessionId, request_id: &[u8]) -> Option<Output> {
        self.sessions
            .get(&session_id)
            .and_then(|session| session.completed.get(request_id))
            .cloned()
    }

    pub fn complete(
        &mut self,
        session_id: SessionId,
        request_id: Vec<u8>,
        result: Output,
    ) -> Result<(), ReliableError> {
        let Some(session) = self.sessions.get_mut(&session_id) else {
            return Err(ReliableError::MissingSession(session_id));
        };
        if request_id.is_empty() {
            return Err(ReliableError::EmptyRequestId);
        }
        if !session.completed.contains_key(&request_id) {
            session.order.push_back(request_id.clone());
        }
        session.completed.insert(request_id, result);
        while session.order.len() > self.maximum_completed_per_session {
            if let Some(expired) = session.order.pop_front() {
                session.completed.remove(&expired);
            }
        }
        Ok(())
    }

    pub fn remove_session(&mut self, session_id: SessionId) {
        self.sessions.remove(&session_id);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReliableError {
    #[error("Controller session {0} has no reliable-command log")]
    MissingSession(SessionId),
    #[error("reliable command request_id must not be empty")]
    EmptyRequestId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_requests_replay_the_last_completed_result() {
        let mut commands = ReliableCommands::new(2);
        commands.attach_session(1);
        assert_eq!(commands.replay(1, b"request"), None);
        assert_eq!(commands.complete(1, b"request".to_vec(), 10), Ok(()));
        assert_eq!(commands.replay(1, b"request"), Some(10));
        assert_eq!(commands.complete(1, b"request".to_vec(), 11), Ok(()));
        assert_eq!(commands.replay(1, b"request"), Some(11));
    }

    #[test]
    fn completion_history_is_bounded_in_insertion_order() {
        let mut commands = ReliableCommands::new(2);
        commands.attach_session(1);
        assert_eq!(commands.complete(1, b"one".to_vec(), 1), Ok(()));
        assert_eq!(commands.complete(1, b"two".to_vec(), 2), Ok(()));
        assert_eq!(commands.complete(1, b"three".to_vec(), 3), Ok(()));
        assert_eq!(commands.replay(1, b"one"), None);
        assert_eq!(commands.replay(1, b"two"), Some(2));
        assert_eq!(commands.replay(1, b"three"), Some(3));
    }

    #[test]
    fn unknown_sessions_and_empty_request_ids_fail_closed() {
        let mut commands = ReliableCommands::new(2);
        assert_eq!(
            commands.complete(4, b"request".to_vec(), 1),
            Err(ReliableError::MissingSession(4))
        );
        commands.attach_session(4);
        assert_eq!(
            commands.complete(4, Vec::new(), 1),
            Err(ReliableError::EmptyRequestId)
        );
    }
}
