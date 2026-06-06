use std::path::PathBuf;

/// ATproto-side config for vlecht.
///
/// The audience DID is the knot's own identity — PDSes verify signed
/// XRPC calls by checking the `aud` claim matches this DID.
///
/// The service signing key signs the auth headers; its public counterpart
/// is served at `/.well-known/did.json`.
#[derive(Clone)]
pub struct AtpConfig {
    /// DID of this knot (e.g. `did:web:myhost.example.com`). Appears in
    /// the `aud` claim of inbound service auth tokens.
    pub audience_did: String,
    /// Public key file in multikey multibase format.
    /// Read from disk at startup; the multibase form is the same as on the
    /// wire in `verificationMethod[].publicKeyMultibase`.
    pub service_key_path: PathBuf,
    /// PLC directory URL used for handle/DID resolution. Default: official.
    pub plc_url: String,
}

impl AtpConfig {
    /// Read from env. All fields are optional; if `audience_did` is empty,
    /// ATproto features are disabled and the server falls back to plain
    /// HTTP behavior.
    pub fn from_env() -> Self {
        let audience_did = std::env::var("VLECHT_ATP_AUDIENCE_DID").unwrap_or_default();
        let service_key_path = std::env::var("VLECHT_ATP_SERVICE_KEY_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./vlecht-service-key.multikey"));
        let plc_url =
            std::env::var("VLECHT_ATP_PLC_URL").unwrap_or_else(|_| "https://plc.directory".into());

        Self {
            audience_did,
            service_key_path,
            plc_url,
        }
    }

    /// True if ATproto features should be enabled. Requires both an
    /// audience DID and a key on disk.
    pub fn is_enabled(&self) -> bool {
        !self.audience_did.is_empty() && self.service_key_path.exists()
    }

    /// Build a `DidDocument` for `did:web` resolution.
    ///
    /// Reads the public key from `service_key_path` (multikey multibase
    /// format) and constructs a `did:web` DID document with a single
    /// `verificationMethod` entry. Returns `None` if ATproto is not
    /// enabled (missing audience DID or key file).
    pub fn build_did_document(&self) -> Option<
        jacquard_common::types::did_doc::DidDocument<'static>,
    > {
        use jacquard_common::types::did_doc::{DidDocument, VerificationMethod};
        use jacquard_common::CowStr;

        if !self.is_enabled() {
            return None;
        }

        let multikey = std::fs::read_to_string(&self.service_key_path)
            .ok()
            .and_then(|s| {
                let trimmed = s.trim().to_owned();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                }
            })?;

        let audience = self.audience_did.clone();
        let vm_id = format!("{audience}#atproto");

        let doc = DidDocument {
            context: jacquard_common::types::did_doc::default_context(),
            id: jacquard_common::types::string::Did::new_owned(&audience)
                .ok()?,
            also_known_as: None,
            verification_method: Some(vec![VerificationMethod {
                id: CowStr::copy_from_str(&vm_id),
                r#type: CowStr::new_static("Multikey"),
                controller: Some(CowStr::copy_from_str(&audience)),
                public_key_multibase: Some(CowStr::copy_from_str(&multikey)),
                extra_data: Default::default(),
            }]),
            service: None,
            extra_data: Default::default(),
        };

        Some(doc)
    }
}
