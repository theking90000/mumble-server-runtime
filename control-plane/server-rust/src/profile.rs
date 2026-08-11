use crate::protocol::ProfileRef;

pub(crate) const SPACES_PROFILE_ID: &str = "mumble.controller.spaces";
pub(crate) const SPACES_SCHEMA_VERSION: u32 = 1;
const SPACES_DESCRIPTOR_DIGEST: &str = include_str!("../../contract/controller-v1.pb.sha256");

/// Select a profile compiled into this server before any session state exists.
pub(crate) fn negotiate(requested: Option<&ProfileRef>) -> Result<ProfileRef, ProfileError> {
    let supported = spaces();
    let Some(requested) = requested else {
        // The migration stack keeps the current v1 client usable until the Java
        // modules switch to explicit negotiation. This path is removed at cutover.
        return Ok(supported);
    };
    if requested.profile_id != supported.profile_id {
        return Err(ProfileError::UnknownProfile);
    }
    if requested.schema_version != supported.schema_version {
        return Err(ProfileError::UnknownSchemaVersion);
    }
    if requested.descriptor_digest != supported.descriptor_digest {
        return Err(ProfileError::DescriptorMismatch);
    }
    Ok(supported)
}

pub(crate) fn spaces() -> ProfileRef {
    ProfileRef {
        profile_id: SPACES_PROFILE_ID.to_owned(),
        schema_version: SPACES_SCHEMA_VERSION,
        descriptor_digest: SPACES_DESCRIPTOR_DIGEST.trim().to_owned(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ProfileError {
    #[error("the requested Controller profile is not compiled into this server")]
    UnknownProfile,
    #[error("the requested Controller profile schema version is not supported")]
    UnknownSchemaVersion,
    #[error("the requested Controller profile descriptor digest does not match")]
    DescriptorMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_exact_compiled_profile_is_accepted() {
        let supported = spaces();
        assert_eq!(negotiate(Some(&supported)), Ok(supported.clone()));

        let mut unknown = supported.clone();
        unknown.profile_id = "unknown".to_owned();
        assert_eq!(negotiate(Some(&unknown)), Err(ProfileError::UnknownProfile));

        let mut wrong_version = supported.clone();
        wrong_version.schema_version = supported.schema_version + 1;
        assert_eq!(
            negotiate(Some(&wrong_version)),
            Err(ProfileError::UnknownSchemaVersion)
        );

        let mut wrong_digest = supported;
        wrong_digest.descriptor_digest = "wrong".to_owned();
        assert_eq!(
            negotiate(Some(&wrong_digest)),
            Err(ProfileError::DescriptorMismatch)
        );
    }
}
