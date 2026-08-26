//! Resolve a user's SSH public keys from their PDS.
//!
//! knot2 authenticates SSH clients against `sh.tangled.publicKey` records
//! stored in the user's own PDS (collection `sh.tangled.publicKey`, record
//! values shaped `{key, name, createdAt}`). This module resolves a DID to
//! its PDS via the DID document and lists those records.
//!
//! vlecht keeps its local `public_keys` table as the fast path; this PDS
//! lookup is the fallback for keys registered on-protocol, so a user who
//! rotated keys in their PDS needs no knot-side action.

use crate::identity::AtpIdentity;
use jacquard_common::types::did::Did;
use jacquard_common::types::did_doc::Service;
use jacquard_identity::resolver::IdentityResolver;
use std::sync::OnceLock;

/// Collection NSID for SSH public key records, matching knot2.
pub const PUBKEY_COLLECTION: &str = "sh.tangled.publicKey";

fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("reqwest client build")
    })
}

/// List a DID's `sh.tangled.publicKey` record key strings from their PDS.
///
/// Errors on resolution failure, transport failure, or a non-200 from the
/// PDS (callers negative-cache these).
pub async fn fetch_pds_pubkeys(identity: &AtpIdentity, did: &str) -> Result<Vec<String>, String> {
    let did_typed: Did = Did::new_owned(did).map_err(|e| format!("invalid DID {did}: {e}"))?;
    let doc_response = identity
        .resolver
        .resolve_did_doc(&did_typed)
        .await
        .map_err(|e| format!("DID resolution failed for {did}: {e}"))?;
    let doc = doc_response
        .parse()
        .map_err(|e| format!("DID document parse failed for {did}: {e}"))?;

    let pds = doc
        .service
        .unwrap_or_default()
        .into_iter()
        .find(|s: &Service<_>| s.id.ends_with("#atproto_pds"))
        .ok_or_else(|| format!("no #atproto_pds service in {did}'s DID document"))?;

    let endpoint = pds
        .service_endpoint
        .as_ref()
        .and_then(|e| e.as_str())
        .ok_or_else(|| format!("no service endpoint for #atproto_pds in {did}'s DID document"))?
        .trim_end_matches('/');
    let resp = http_client()
        .get(format!("{endpoint}/xrpc/com.atproto.repo.listRecords"))
        .query(&[
            ("repo", did),
            ("collection", PUBKEY_COLLECTION),
            ("limit", "100"),
        ])
        .send()
        .await
        .map_err(|e| format!("listRecords request to {endpoint} failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!(
            "listRecords from {endpoint} returned {}",
            resp.status()
        ));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("listRecords response not JSON: {e}"))?;
    Ok(parse_pubkey_records(&body))
}

/// Extract the authorized-keys lines from a `listRecords` response body,
/// skipping malformed records (mirrors knot2's `offered_page`).
pub fn parse_pubkey_records(body: &serde_json::Value) -> Vec<String> {
    body["records"]
        .as_array()
        .map(|records| {
            records
                .iter()
                .filter_map(|r| r["value"]["key"].as_str().map(str::to_owned))
                .filter(|k| k.starts_with("ssh-") || k.starts_with("ecdsa-"))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::parse_pubkey_records;

    #[test]
    fn parses_record_values_and_skips_garbage() {
        let body = serde_json::json!({
            "records": [
                {"uri": "at://did:plc:x/sh.tangled.publicKey/a",
                 "value": {"$type": "sh.tangled.publicKey", "key": "ssh-ed25519 AAAAC3 one", "name": "laptop"}},
                {"uri": "at://did:plc:x/sh.tangled.publicKey/b",
                 "value": {"$type": "sh.tangled.publicKey", "key": "garbage line"}},
                {"uri": "at://did:plc:x/sh.tangled.publicKey/c",
                 "value": {"$type": "sh.tangled.publicKey", "key": "ssh-rsa AAAAB3 two"}},
                {"uri": "at://did:plc:x/sh.tangled.publicKey/d",
                 "value": {"$type": "sh.tangled.publicKey"}},
            ]
        });
        assert_eq!(
            parse_pubkey_records(&body),
            vec![
                "ssh-ed25519 AAAAC3 one".to_string(),
                "ssh-rsa AAAAB3 two".to_string()
            ]
        );
    }

    #[test]
    fn empty_when_no_records() {
        assert!(parse_pubkey_records(&serde_json::json!({})).is_empty());
        assert!(parse_pubkey_records(&serde_json::json!({"records": []})).is_empty());
    }
}
