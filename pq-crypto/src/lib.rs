//! Post-quantum signature primitives: Dilithium2 and Falcon-512 keys, signatures, and a
//! scheme registry that resolves a `SignatureSchemeId` to an implementation.
//!
//! This code was moved verbatim from `qcoin-crypto` (same BSD-3-Clause owner) so that it
//! has one home and the dependency runs one way. loadngo owns it; qcoin and the
//! entitlement-achievement-blockchain depend on loadngo, never the reverse. They keep
//! spelling it `qcoin_crypto` through a dependency alias, so no call site changed.

use serde::{
    de::Error as DeError, ser::Error as SerError, Deserialize, Deserializer, Serialize, Serializer,
};
use std::collections::HashMap;
use std::fmt::Display;

use pqcrypto_dilithium::dilithium2;
use pqcrypto_falcon::falcon512;
use pqcrypto_traits::sign::{
    DetachedSignature, PublicKey as PqPublicKeyTrait, SecretKey as PqSecretKeyTrait,
};
use zeroize::Zeroizing;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SignatureSchemeId {
    Dilithium2,
    Falcon512,
    Unknown(u16),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CryptoError {
    #[error("signature scheme mismatch")]
    WrongScheme,
    #[error("invalid public key")]
    InvalidPublicKey,
    #[error("invalid secret key")]
    InvalidSecretKey,
    #[error("invalid signature")]
    InvalidSignature,
    #[error("unsupported scheme {0}")]
    UnsupportedScheme(SignatureSchemeId),
}

impl SignatureSchemeId {
    pub const DILITHIUM2_ID: u16 = 0x01;
    pub const FALCON512_ID: u16 = 0x02;

    pub fn from_u16(id: u16) -> Self {
        match id {
            Self::DILITHIUM2_ID => SignatureSchemeId::Dilithium2,
            Self::FALCON512_ID => SignatureSchemeId::Falcon512,
            other => SignatureSchemeId::Unknown(other),
        }
    }

    pub fn to_u16(self) -> u16 {
        match self {
            SignatureSchemeId::Dilithium2 => Self::DILITHIUM2_ID,
            SignatureSchemeId::Falcon512 => Self::FALCON512_ID,
            SignatureSchemeId::Unknown(value) => value,
        }
    }
}

impl Display for SignatureSchemeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignatureSchemeId::Dilithium2 => write!(f, "dilithium2"),
            SignatureSchemeId::Falcon512 => write!(f, "falcon512"),
            SignatureSchemeId::Unknown(value) => write!(f, "unknown-{}", value),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKey {
    pub scheme: SignatureSchemeId,
    pub bytes: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct PrivateKey {
    pub scheme: SignatureSchemeId,
    pub bytes: Zeroizing<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub scheme: SignatureSchemeId,
    pub bytes: Vec<u8>,
}

impl PublicKey {
    pub fn new(scheme: SignatureSchemeId, bytes: Vec<u8>) -> Result<Self, CryptoError> {
        validate_key_size(
            public_key_size,
            scheme,
            bytes.len(),
            false,
            CryptoError::InvalidPublicKey,
        )?;
        Ok(Self { scheme, bytes })
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, CryptoError> {
        encode_with_scheme(
            self.scheme,
            public_key_size,
            &self.bytes,
            false,
            CryptoError::InvalidPublicKey,
        )
    }

    pub fn from_bytes(encoded: &[u8]) -> Result<Self, CryptoError> {
        let (scheme, payload) = decode_with_scheme(encoded, CryptoError::InvalidPublicKey)?;
        validate_key_size(
            public_key_size,
            scheme,
            payload.len(),
            false,
            CryptoError::InvalidPublicKey,
        )?;
        Ok(Self {
            scheme,
            bytes: payload,
        })
    }
}

impl PrivateKey {
    pub fn new(scheme: SignatureSchemeId, bytes: Vec<u8>) -> Result<Self, CryptoError> {
        validate_key_size(
            secret_key_size,
            scheme,
            bytes.len(),
            false,
            CryptoError::InvalidSecretKey,
        )?;
        Ok(Self {
            scheme,
            bytes: Zeroizing::new(bytes),
        })
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, CryptoError> {
        encode_with_scheme(
            self.scheme,
            secret_key_size,
            &self.bytes,
            false,
            CryptoError::InvalidSecretKey,
        )
    }

    pub fn from_bytes(encoded: &[u8]) -> Result<Self, CryptoError> {
        let (scheme, payload) = decode_with_scheme(encoded, CryptoError::InvalidSecretKey)?;
        validate_key_size(
            secret_key_size,
            scheme,
            payload.len(),
            false,
            CryptoError::InvalidSecretKey,
        )?;
        Ok(Self {
            scheme,
            bytes: Zeroizing::new(payload),
        })
    }
}

impl Signature {
    pub fn new(scheme: SignatureSchemeId, bytes: Vec<u8>) -> Result<Self, CryptoError> {
        validate_key_size(
            signature_size,
            scheme,
            bytes.len(),
            true,
            CryptoError::InvalidSignature,
        )?;
        Ok(Self { scheme, bytes })
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, CryptoError> {
        encode_with_scheme(
            self.scheme,
            signature_size,
            &self.bytes,
            true,
            CryptoError::InvalidSignature,
        )
    }

    pub fn from_bytes(encoded: &[u8]) -> Result<Self, CryptoError> {
        let (scheme, payload) = decode_with_scheme(encoded, CryptoError::InvalidSignature)?;
        validate_key_size(
            signature_size,
            scheme,
            payload.len(),
            true,
            CryptoError::InvalidSignature,
        )?;
        Ok(Self {
            scheme,
            bytes: payload,
        })
    }
}

impl Serialize for PublicKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let bytes = self.to_bytes().map_err(SerError::custom)?;
        serializer.serialize_bytes(&bytes)
    }
}

impl<'de> Deserialize<'de> for PublicKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct PublicKeyVisitor;

        impl<'de> serde::de::Visitor<'de> for PublicKeyVisitor {
            type Value = PublicKey;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(f, "byte-encoded public key")
            }

            fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
            where
                E: DeError,
            {
                PublicKey::from_bytes(v).map_err(DeError::custom)
            }

            fn visit_byte_buf<E>(self, v: Vec<u8>) -> Result<Self::Value, E>
            where
                E: DeError,
            {
                PublicKey::from_bytes(&v).map_err(DeError::custom)
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let mut bytes = Vec::new();
                while let Some(byte) = seq.next_element::<u8>()? {
                    bytes.push(byte);
                }
                PublicKey::from_bytes(&bytes).map_err(DeError::custom)
            }
        }

        deserializer.deserialize_bytes(PublicKeyVisitor)
    }
}

impl Serialize for PrivateKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let bytes = self.to_bytes().map_err(SerError::custom)?;
        serializer.serialize_bytes(&bytes)
    }
}

impl<'de> Deserialize<'de> for PrivateKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct PrivateKeyVisitor;

        impl<'de> serde::de::Visitor<'de> for PrivateKeyVisitor {
            type Value = PrivateKey;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(f, "byte-encoded private key")
            }

            fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
            where
                E: DeError,
            {
                PrivateKey::from_bytes(v).map_err(DeError::custom)
            }

            fn visit_byte_buf<E>(self, v: Vec<u8>) -> Result<Self::Value, E>
            where
                E: DeError,
            {
                PrivateKey::from_bytes(&v).map_err(DeError::custom)
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let mut bytes = Vec::new();
                while let Some(byte) = seq.next_element::<u8>()? {
                    bytes.push(byte);
                }
                PrivateKey::from_bytes(&bytes).map_err(DeError::custom)
            }
        }

        deserializer.deserialize_bytes(PrivateKeyVisitor)
    }
}

impl Serialize for Signature {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let bytes = self.to_bytes().map_err(SerError::custom)?;
        serializer.serialize_bytes(&bytes)
    }
}

impl<'de> Deserialize<'de> for Signature {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct SignatureVisitor;

        impl<'de> serde::de::Visitor<'de> for SignatureVisitor {
            type Value = Signature;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(f, "byte-encoded signature")
            }

            fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
            where
                E: DeError,
            {
                Signature::from_bytes(v).map_err(DeError::custom)
            }

            fn visit_byte_buf<E>(self, v: Vec<u8>) -> Result<Self::Value, E>
            where
                E: DeError,
            {
                Signature::from_bytes(&v).map_err(DeError::custom)
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let mut bytes = Vec::new();
                while let Some(byte) = seq.next_element::<u8>()? {
                    bytes.push(byte);
                }
                Signature::from_bytes(&bytes).map_err(DeError::custom)
            }
        }

        deserializer.deserialize_bytes(SignatureVisitor)
    }
}

fn validate_key_size(
    size_fn: fn(SignatureSchemeId) -> Option<usize>,
    scheme: SignatureSchemeId,
    len: usize,
    allow_shorter: bool,
    mismatch_error: CryptoError,
) -> Result<(), CryptoError> {
    match size_fn(scheme) {
        Some(expected)
            if (!allow_shorter && expected == len) || (allow_shorter && len <= expected) =>
        {
            Ok(())
        }
        Some(_) => Err(mismatch_error),
        None => Err(CryptoError::UnsupportedScheme(scheme)),
    }
}

fn encode_with_scheme(
    scheme: SignatureSchemeId,
    size_fn: fn(SignatureSchemeId) -> Option<usize>,
    bytes: &[u8],
    allow_shorter: bool,
    mismatch_error: CryptoError,
) -> Result<Vec<u8>, CryptoError> {
    validate_key_size(size_fn, scheme, bytes.len(), allow_shorter, mismatch_error)?;

    let len = bytes.len() as u32;
    let mut encoded = Vec::with_capacity(2 + 4 + bytes.len());
    encoded.extend_from_slice(&scheme.to_u16().to_le_bytes());
    encoded.extend_from_slice(&len.to_le_bytes());
    encoded.extend_from_slice(bytes);
    Ok(encoded)
}

fn decode_with_scheme(
    encoded: &[u8],
    mismatch_error: CryptoError,
) -> Result<(SignatureSchemeId, Vec<u8>), CryptoError> {
    const PREFIX_LEN: usize = 2 + 4;
    if encoded.len() < PREFIX_LEN {
        return Err(mismatch_error);
    }

    let scheme = SignatureSchemeId::from_u16(u16::from_le_bytes([encoded[0], encoded[1]]));
    let len = u32::from_le_bytes([encoded[2], encoded[3], encoded[4], encoded[5]]) as usize;

    if encoded.len() != PREFIX_LEN + len {
        return Err(mismatch_error);
    }

    let payload = encoded[PREFIX_LEN..].to_vec();
    Ok((scheme, payload))
}

fn public_key_size(scheme: SignatureSchemeId) -> Option<usize> {
    match scheme {
        SignatureSchemeId::Dilithium2 => Some(dilithium2::public_key_bytes()),
        SignatureSchemeId::Falcon512 => Some(falcon512::public_key_bytes()),
        SignatureSchemeId::Unknown(_) => None,
    }
}

fn secret_key_size(scheme: SignatureSchemeId) -> Option<usize> {
    match scheme {
        SignatureSchemeId::Dilithium2 => Some(dilithium2::secret_key_bytes()),
        SignatureSchemeId::Falcon512 => Some(falcon512::secret_key_bytes()),
        SignatureSchemeId::Unknown(_) => None,
    }
}

fn signature_size(scheme: SignatureSchemeId) -> Option<usize> {
    match scheme {
        SignatureSchemeId::Dilithium2 => Some(dilithium2::signature_bytes()),
        SignatureSchemeId::Falcon512 => Some(falcon512::signature_bytes()),
        SignatureSchemeId::Unknown(_) => None,
    }
}

pub trait PqSignatureScheme: Send + Sync {
    fn id(&self) -> SignatureSchemeId;
    fn keygen(&self) -> Result<(PublicKey, PrivateKey), CryptoError>;
    fn sign(&self, sk: &PrivateKey, msg: &[u8]) -> Result<Signature, CryptoError>;
    fn verify(&self, pk: &PublicKey, msg: &[u8], sig: &Signature) -> Result<(), CryptoError>;
}

pub trait PqSchemeRegistry {
    fn get(&self, id: &SignatureSchemeId) -> Option<&dyn PqSignatureScheme>;
}

pub struct InMemoryRegistry {
    schemes: HashMap<SignatureSchemeId, Box<dyn PqSignatureScheme>>,
}

impl Default for InMemoryRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            schemes: HashMap::new(),
        }
    }

    pub fn with_scheme(mut self, scheme: Box<dyn PqSignatureScheme>) -> Self {
        let id = scheme.id();
        self.schemes.insert(id, scheme);
        self
    }

    pub fn add_scheme(&mut self, scheme: Box<dyn PqSignatureScheme>) {
        let id = scheme.id();
        self.schemes.insert(id, scheme);
    }
}

impl PqSchemeRegistry for InMemoryRegistry {
    fn get(&self, id: &SignatureSchemeId) -> Option<&dyn PqSignatureScheme> {
        self.schemes.get(id).map(|boxed| boxed.as_ref())
    }
}

pub struct Dilithium2Scheme;

impl PqSignatureScheme for Dilithium2Scheme {
    fn id(&self) -> SignatureSchemeId {
        SignatureSchemeId::Dilithium2
    }

    fn keygen(&self) -> Result<(PublicKey, PrivateKey), CryptoError> {
        let (pk, sk) = dilithium2::keypair();
        Ok((
            PublicKey::new(self.id(), pk.as_bytes().to_vec())?,
            PrivateKey::new(self.id(), sk.as_bytes().to_vec())?,
        ))
    }

    fn sign(&self, sk: &PrivateKey, msg: &[u8]) -> Result<Signature, CryptoError> {
        if sk.scheme != self.id() {
            return Err(CryptoError::WrongScheme);
        }

        let sk = dilithium2::SecretKey::from_bytes(&sk.bytes)
            .map_err(|_| CryptoError::InvalidSecretKey)?;

        let signature = dilithium2::detached_sign(msg, &sk);
        Signature::new(self.id(), signature.as_bytes().to_vec())
    }

    fn verify(&self, pk: &PublicKey, msg: &[u8], sig: &Signature) -> Result<(), CryptoError> {
        if pk.scheme != self.id() || sig.scheme != self.id() {
            return Err(CryptoError::WrongScheme);
        }

        let pk = dilithium2::PublicKey::from_bytes(&pk.bytes)
            .map_err(|_| CryptoError::InvalidPublicKey)?;
        let sig = dilithium2::DetachedSignature::from_bytes(&sig.bytes)
            .map_err(|_| CryptoError::InvalidSignature)?;

        dilithium2::verify_detached_signature(&sig, msg, &pk)
            .map_err(|_| CryptoError::InvalidSignature)
    }
}

pub struct Falcon512Scheme;

impl PqSignatureScheme for Falcon512Scheme {
    fn id(&self) -> SignatureSchemeId {
        SignatureSchemeId::Falcon512
    }

    fn keygen(&self) -> Result<(PublicKey, PrivateKey), CryptoError> {
        let (pk, sk) = falcon512::keypair();
        Ok((
            PublicKey::new(self.id(), pk.as_bytes().to_vec())?,
            PrivateKey::new(self.id(), sk.as_bytes().to_vec())?,
        ))
    }

    fn sign(&self, sk: &PrivateKey, msg: &[u8]) -> Result<Signature, CryptoError> {
        if sk.scheme != self.id() {
            return Err(CryptoError::WrongScheme);
        }

        let sk = falcon512::SecretKey::from_bytes(&sk.bytes)
            .map_err(|_| CryptoError::InvalidSecretKey)?;

        let signature = falcon512::detached_sign(msg, &sk);
        Signature::new(self.id(), signature.as_bytes().to_vec())
    }

    fn verify(&self, pk: &PublicKey, msg: &[u8], sig: &Signature) -> Result<(), CryptoError> {
        if pk.scheme != self.id() || sig.scheme != self.id() {
            return Err(CryptoError::WrongScheme);
        }

        let pk = falcon512::PublicKey::from_bytes(&pk.bytes)
            .map_err(|_| CryptoError::InvalidPublicKey)?;
        let sig = falcon512::DetachedSignature::from_bytes(&sig.bytes)
            .map_err(|_| CryptoError::InvalidSignature)?;

        falcon512::verify_detached_signature(&sig, msg, &pk)
            .map_err(|_| CryptoError::InvalidSignature)
    }
}

pub fn default_registry() -> InMemoryRegistry {
    InMemoryRegistry::new()
        .with_scheme(Box::new(Dilithium2Scheme))
        .with_scheme(Box::new(Falcon512Scheme))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MESSAGE: &[u8] = b"qcoin-pq-test";

    #[test]
    fn dilithium_roundtrip() {
        let scheme = Dilithium2Scheme;
        let (pk, sk) = scheme.keygen().expect("keygen should succeed");
        let sig = scheme.sign(&sk, MESSAGE).expect("signing should succeed");

        assert!(scheme.verify(&pk, MESSAGE, &sig).is_ok());
    }

    #[test]
    fn falcon_roundtrip() {
        let scheme = Falcon512Scheme;
        let (pk, sk) = scheme.keygen().expect("keygen should succeed");
        let sig = scheme.sign(&sk, MESSAGE).expect("signing should succeed");

        assert!(scheme.verify(&pk, MESSAGE, &sig).is_ok());
    }

    #[test]
    fn signing_rejects_wrong_scheme_key() {
        let dilithium = Dilithium2Scheme;
        let falcon = Falcon512Scheme;

        let (_, sk) = dilithium.keygen().expect("keygen should succeed");
        let result = falcon.sign(&sk, MESSAGE);

        assert!(matches!(result, Err(CryptoError::WrongScheme)));
    }

    #[test]
    fn verify_rejects_wrong_scheme_signature() {
        let dilithium = Dilithium2Scheme;
        let falcon = Falcon512Scheme;

        let (dili_pk, _dili_sk) = dilithium.keygen().expect("keygen should succeed");
        let falcon_sig = falcon
            .sign(
                &falcon.keygen().expect("falcon keygen should succeed").1,
                MESSAGE,
            )
            .expect("falcon signing should succeed");

        let result = dilithium.verify(&dili_pk, MESSAGE, &falcon_sig);
        assert!(matches!(result, Err(CryptoError::WrongScheme)));

        let sig_from_wrong_scheme = Signature {
            scheme: SignatureSchemeId::Dilithium2,
            bytes: falcon_sig.bytes.clone(),
        };
        let result = dilithium.verify(&dili_pk, MESSAGE, &sig_from_wrong_scheme);
        assert!(matches!(result, Err(CryptoError::InvalidSignature)));
    }

    #[test]
    fn verify_rejects_corrupted_public_key() {
        let scheme = Dilithium2Scheme;
        let (pk, sk) = scheme.keygen().expect("keygen should succeed");
        let sig = scheme.sign(&sk, MESSAGE).expect("signing should succeed");

        let mut corrupted_bytes = pk.bytes.clone();
        corrupted_bytes.push(0);
        let corrupted_pk = PublicKey {
            scheme: pk.scheme,
            bytes: corrupted_bytes,
        };

        let result = scheme.verify(&corrupted_pk, MESSAGE, &sig);
        assert!(matches!(result, Err(CryptoError::InvalidPublicKey)));
    }

    #[test]
    fn verify_rejects_corrupted_signature() {
        let scheme = Falcon512Scheme;
        let (pk, sk) = scheme.keygen().expect("keygen should succeed");
        let mut sig = scheme.sign(&sk, MESSAGE).expect("signing should succeed");
        sig.bytes.push(1);

        let result = scheme.verify(&pk, MESSAGE, &sig);
        assert!(matches!(result, Err(CryptoError::InvalidSignature)));
    }

    #[test]
    fn verify_rejects_corrupted_message() {
        let scheme = Dilithium2Scheme;
        let (pk, sk) = scheme.keygen().expect("keygen should succeed");
        let sig = scheme.sign(&sk, MESSAGE).expect("signing should succeed");

        let result = scheme.verify(&pk, b"tampered", &sig);
        assert!(matches!(result, Err(CryptoError::InvalidSignature)));
    }

    #[test]
    fn roundtrip_serialization_is_canonical() {
        let scheme = Falcon512Scheme;
        let (pk, sk) = scheme.keygen().expect("keygen should succeed");
        let sig = scheme.sign(&sk, MESSAGE).expect("signing should succeed");

        let encoded_pk = pk.to_bytes().expect("public key encoding");
        let decoded_pk = PublicKey::from_bytes(&encoded_pk).expect("public key decoding");
        assert_eq!(pk, decoded_pk);

        let encoded_sig = sig.to_bytes().expect("signature encoding");
        let decoded_sig = Signature::from_bytes(&encoded_sig).expect("signature decoding");
        assert_eq!(sig, decoded_sig);
    }

    #[test]
    fn json_roundtrip_accepts_byte_array_encoding() {
        let scheme = Dilithium2Scheme;
        let (pk, sk) = scheme.keygen().expect("keygen should succeed");
        let sig = scheme.sign(&sk, MESSAGE).expect("signing should succeed");

        let encoded_pk = serde_json::to_string(&pk).expect("public key json encoding");
        let decoded_pk: PublicKey =
            serde_json::from_str(&encoded_pk).expect("public key json decoding");
        assert_eq!(pk, decoded_pk);

        let encoded_sig = serde_json::to_string(&sig).expect("signature json encoding");
        let decoded_sig: Signature =
            serde_json::from_str(&encoded_sig).expect("signature json decoding");
        assert_eq!(sig, decoded_sig);
    }
}
