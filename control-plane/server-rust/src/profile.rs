use mumble_controller_core::{ProfileError, ProfileRef, negotiate_profile};

use crate::protocol::ProfileRef as ProtocolProfileRef;

pub(crate) const SPACES_PROFILE_ID: &str = "mumble.controller.spaces";
pub(crate) const SPACES_SCHEMA_VERSION: u32 = 1;
const SPACES_DESCRIPTOR_DIGEST: &str =
    include_str!("../../implementations/spaces/contract/controller-spaces-v1.pb.sha256");

/// Select a profile compiled into this server before any session state exists.
pub(crate) fn negotiate(
    requested: Option<&ProtocolProfileRef>,
) -> Result<ProfileRef, ProfileError> {
    let supported = spaces()?;
    let requested = match requested {
        Some(requested) => ProfileRef::new(
            requested.profile_id.clone(),
            requested.schema_version,
            requested.descriptor_digest.clone(),
        )?,
        None => {
            // The migration stack keeps the current v1 client usable until the Java
            // modules switch to explicit negotiation. This path is removed at cutover.
            supported.clone()
        }
    };
    negotiate_profile(&requested, &supported)
}

pub(crate) fn spaces() -> Result<ProfileRef, ProfileError> {
    ProfileRef::new(
        SPACES_PROFILE_ID.to_owned(),
        SPACES_SCHEMA_VERSION,
        SPACES_DESCRIPTOR_DIGEST.trim().to_owned(),
    )
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
        assert_eq!(negotiate(Some(&protocol)), Ok(supported.clone()));

        let mut unknown = protocol.clone();
        unknown.profile_id = "unknown".to_owned();
        assert_eq!(negotiate(Some(&unknown)), Err(ProfileError::UnknownProfile));

        let mut wrong_version = protocol.clone();
        wrong_version.schema_version = supported.schema_version() + 1;
        assert_eq!(
            negotiate(Some(&wrong_version)),
            Err(ProfileError::UnknownSchemaVersion)
        );

        let mut wrong_digest = protocol;
        wrong_digest.descriptor_digest = "wrong".to_owned();
        assert_eq!(
            negotiate(Some(&wrong_digest)),
            Err(ProfileError::DescriptorMismatch)
        );
    }
}
