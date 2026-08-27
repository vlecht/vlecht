//! Generate a did:web service signing key for the knot.
//!
//! Prints two pieces, in this order:
//! 1. the multikey-multibase PUBLIC key (goes in service_key_path — it's
//!    what the served did.json publishes)
//! 2. the raw 32-byte SECRET as hex (keep private; for any future
//!    knot-signed artifacts)
//!
//! ES256K (secp256k1 compressed-pub multicodec 0xe701), same family as
//! PLC-registered keys and upstream knot identities.

use k256::ecdsa::{SigningKey, VerifyingKey};
use multibase::Base;

fn main() {
    let signing_key = SigningKey::random(&mut k256::elliptic_curve::rand_core::OsRng);
    let verifying_key: &VerifyingKey = signing_key.verifying_key();

    let point = verifying_key.to_encoded_point(true);
    let mut multicodec = vec![0xe7, 0x01]; // secp256k1-pub
    multicodec.extend_from_slice(point.as_bytes());
    let public_multibase = multibase::encode(Base::Base58Btc, &multicodec);

    println!("{public_multibase}");
    println!("{:x}", signing_key.to_bytes());
}
