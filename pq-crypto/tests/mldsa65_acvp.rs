use aws_lc_rs::encoding::AsRawBytes;
use aws_lc_rs::signature::{KeyPair, PqdsaKeyPair, ML_DSA_65_SIGNING};
use loadngo_pq_crypto::{
    MlDsa65Scheme, PqSignatureScheme, PrivateKey, PublicKey, Signature, SignatureSchemeId,
};
use serde::Deserialize;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Deserialize)]
struct KeyGenerationVector {
    seed: String,
    public_key: String,
    private_key: String,
}

#[test]
fn mldsa65_keygen_matches_nist_acvp_fips204_vector() {
    let vector: KeyGenerationVector =
        serde_json::from_str(include_str!("vectors/mldsa65_keygen_acvp.json"))
            .expect("parse checked-in NIST ACVP vector");
    let seed = decode_hex(&vector.seed);
    let expected_public_key = decode_hex(&vector.public_key);
    let expected_private_key = decode_hex(&vector.private_key);

    let key_pair =
        PqdsaKeyPair::from_seed(&ML_DSA_65_SIGNING, &seed).expect("derive ML-DSA-65 keypair");
    assert_eq!(key_pair.public_key().as_ref(), expected_public_key);
    assert_eq!(
        key_pair
            .private_key()
            .as_raw_bytes()
            .expect("encode expanded ML-DSA-65 private key")
            .as_ref(),
        expected_private_key
    );
}

#[test]
#[ignore = "requires an OpenSSL build with ML-DSA enabled"]
fn mldsa65_signatures_interoperate_with_openssl() {
    let vector: KeyGenerationVector =
        serde_json::from_str(include_str!("vectors/mldsa65_keygen_acvp.json"))
            .expect("parse checked-in NIST ACVP vector");
    let seed = decode_hex(&vector.seed);
    let key_pair =
        PqdsaKeyPair::from_seed(&ML_DSA_65_SIGNING, &seed).expect("derive ML-DSA-65 keypair");
    let private_key_bytes = key_pair
        .private_key()
        .as_raw_bytes()
        .expect("encode expanded ML-DSA-65 private key");
    let private_key = PrivateKey::new(
        SignatureSchemeId::MlDsa65,
        private_key_bytes.as_ref().to_vec(),
    )
    .expect("wrap raw ML-DSA-65 private key");
    let public_key = PublicKey::new(
        SignatureSchemeId::MlDsa65,
        key_pair.public_key().as_ref().to_vec(),
    )
    .expect("wrap raw ML-DSA-65 public key");

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "loadngo-mldsa-openssl-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir(&directory).expect("create temporary interop directory");
    let _cleanup = TempDirectory(directory.clone());
    let private_der_path = directory.join("private.der");
    let public_pem_path = directory.join("public.pem");
    let message_path = directory.join("message.bin");
    let aws_signature_path = directory.join("aws-lc.sig");
    let openssl_signature_path = directory.join("openssl.sig");
    fs::write(
        &private_der_path,
        key_pair
            .to_pkcs8v1()
            .expect("encode PKCS#8 keypair")
            .as_ref(),
    )
    .expect("write PKCS#8 keypair");

    let message = b"loadngo ML-DSA-65 OpenSSL interoperability";
    fs::write(&message_path, message).expect("write signature message");
    let scheme = MlDsa65Scheme;
    let aws_signature = scheme
        .sign(&private_key, message)
        .expect("sign with loadngo ML-DSA wrapper");
    fs::write(&aws_signature_path, &aws_signature.bytes).expect("write AWS-LC signature");

    run_openssl([
        "pkey",
        "-inform",
        "DER",
        "-in",
        path_arg(&private_der_path),
        "-pubout",
        "-out",
        path_arg(&public_pem_path),
    ]);
    run_openssl([
        "pkeyutl",
        "-verify",
        "-pubin",
        "-inkey",
        path_arg(&public_pem_path),
        "-rawin",
        "-in",
        path_arg(&message_path),
        "-sigfile",
        path_arg(&aws_signature_path),
    ]);

    run_openssl([
        "pkeyutl",
        "-sign",
        "-inkey",
        path_arg(&private_der_path),
        "-keyform",
        "DER",
        "-rawin",
        "-in",
        path_arg(&message_path),
        "-out",
        path_arg(&openssl_signature_path),
    ]);
    let openssl_signature = fs::read(&openssl_signature_path).expect("read OpenSSL signature");
    scheme
        .verify(
            &public_key,
            message,
            &Signature::new(SignatureSchemeId::MlDsa65, openssl_signature)
                .expect("wrap OpenSSL ML-DSA signature"),
        )
        .expect("verify OpenSSL signature with loadngo ML-DSA wrapper");
}

fn path_arg(path: &Path) -> &str {
    path.to_str().expect("temporary path is valid UTF-8")
}

fn run_openssl<const N: usize>(arguments: [&str; N]) {
    let output = Command::new("openssl")
        .args(arguments)
        .output()
        .expect("run OpenSSL; install a build with ML-DSA enabled");
    assert!(
        output.status.success(),
        "OpenSSL {:?} failed:\n{}",
        arguments,
        String::from_utf8_lossy(&output.stderr)
    );
}

struct TempDirectory(std::path::PathBuf);

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn decode_hex(encoded: &str) -> Vec<u8> {
    let (pairs, remainder) = encoded.as_bytes().as_chunks::<2>();
    assert!(remainder.is_empty(), "hex input has an odd length");
    pairs
        .iter()
        .map(|pair| {
            let digit = |byte| match byte {
                b'0'..=b'9' => byte - b'0',
                b'A'..=b'F' => byte - b'A' + 10,
                b'a'..=b'f' => byte - b'a' + 10,
                _ => panic!("invalid hexadecimal character"),
            };
            digit(pair[0]) * 16 + digit(pair[1])
        })
        .collect()
}
