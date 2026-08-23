use crate::config::AtpConfig;
use jacquard_identity::resolver::ResolverOptions;
use jacquard_identity::{JacquardResolver, PublicResolver};
use std::sync::Arc;

/// The shared identity resolver used for DID/handle lookups.
///
/// `JacquardResolver` is `Clone` (it's just an `Arc` over its internals), so
/// we hold it directly in shared state.
#[derive(Clone)]
pub struct AtpIdentity {
    pub resolver: Arc<PublicResolver>,
}

impl AtpIdentity {
    pub fn new(_cfg: &AtpConfig) -> anyhow::Result<Self> {
        let http = reqwest_client();
        let resolver = JacquardResolver::new(http, ResolverOptions::default());
        Ok(Self {
            resolver: Arc::new(resolver),
        })
    }
}

fn reqwest_client() -> reqwest::Client {
    // 10s is the indigo default. Knot does the same.
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("reqwest client build")
}
