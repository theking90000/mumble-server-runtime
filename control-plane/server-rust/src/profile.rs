use mumble_controller_core::{ProfileError, ProfileRef, negotiate_profile};
use mumble_controller_spaces::profile_ref;

use crate::actor_messages::ProfileRef as ProtocolProfileRef;

/// Select a profile compiled into this server before any session state exists.
pub(crate) fn negotiate(requested: &ProtocolProfileRef) -> Result<ProfileRef, ProfileError> {
    let supported = spaces()?;
    let requested = ProfileRef::new(
        requested.profile_id.clone(),
        requested.schema_version,
        requested.descriptor_digest.clone(),
    )?;
    negotiate_profile(&requested, &supported)
}

pub(crate) fn spaces() -> Result<ProfileRef, ProfileError> {
    profile_ref()
}

pub(crate) fn to_protocol(profile: &ProfileRef) -> ProtocolProfileRef {
    ProtocolProfileRef {
        profile_id: profile.profile_id().to_owned(),
        schema_version: profile.schema_version(),
        descriptor_digest: profile.descriptor_digest().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_exact_compiled_profile_is_accepted() {
        let Ok(supported) = spaces() else {
            panic!("compiled Spaces profile is valid");
        };
        let protocol = to_protocol(&supported);
        assert_eq!(negotiate(&protocol), Ok(supported.clone()));

        let mut unknown = protocol.clone();
        unknown.profile_id = "unknown".to_owned();
        assert_eq!(negotiate(&unknown), Err(ProfileError::UnknownProfile));

        let mut wrong_version = protocol.clone();
        wrong_version.schema_version = supported.schema_version() + 1;
        assert_eq!(
            negotiate(&wrong_version),
            Err(ProfileError::UnknownSchemaVersion)
        );

        let mut wrong_digest = protocol;
        wrong_digest.descriptor_digest = "wrong".to_owned();
        assert_eq!(
            negotiate(&wrong_digest),
            Err(ProfileError::DescriptorMismatch)
        );
    }
}
