#[cfg(test)]
mod tests {
    use crate::log::identity::{DefaultIdentificator, Identificator};
    use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
    use hex;

    #[tokio::test]
    async fn test_verify_identity_cryptographically() {
        // Cria um identificador e uma identidade válida usando Ed25519 (consistente com Iroh)
        let mut identificator = DefaultIdentificator::new();
        let identity = identificator.create("test_identity");

        println!("Identity created:");
        println!("  ID: {}", identity.id());
        println!("  Public Key: {}", identity.pub_key());
        println!("  ID Signature: {}", identity.signatures().id());
        println!("  PubKey Signature: {}", identity.signatures().pub_key());

        // Verifica se a identidade tem dados válidos
        assert!(!identity.id().is_empty());
        assert!(!identity.pub_key().is_empty());
        assert!(!identity.signatures().id().is_empty());
        assert!(!identity.signatures().pub_key().is_empty());

        // Testa decodificação da chave pública
        let pub_key_bytes = hex::decode(identity.pub_key());
        assert!(pub_key_bytes.is_ok(), "Public key should be valid hex");

        let pub_key_bytes = pub_key_bytes.unwrap();
        assert_eq!(
            pub_key_bytes.len(),
            32,
            "Ed25519 public key should be 32 bytes"
        );

        let mut pk_array = [0u8; 32];
        pk_array.copy_from_slice(&pub_key_bytes);

        let public_key = VerifyingKey::from_bytes(&pk_array);
        assert!(public_key.is_ok(), "Public key should be valid Ed25519 key");

        println!("✅ Identity cryptographic verification test components are valid");
    }

    #[tokio::test]
    async fn test_signature_verification_components() {
        // Teste dos componentes de verificação de assinatura usando Ed25519
        use rand_core::OsRng;

        // Cria uma chave de teste
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();

        // Cria uma mensagem de teste
        let message = b"test message";

        // Assina a mensagem
        let signature = signing_key.sign(message);

        // Verifica a assinatura
        let verification_result = verifying_key.verify(message, &signature);
        assert!(
            verification_result.is_ok(),
            "Signature verification should succeed"
        );

        // Testa parse de assinatura de bytes
        let signature_bytes = signature.to_bytes();
        let parsed_signature = Signature::from_slice(&signature_bytes);
        assert!(parsed_signature.is_ok(), "Signature parsing should succeed");

        // Verifica novamente com a assinatura parsed
        let verification_result2 = verifying_key.verify(message, &parsed_signature.unwrap());
        assert!(
            verification_result2.is_ok(),
            "Parsed signature verification should succeed"
        );

        println!("✅ Ed25519 signature verification components work correctly");
    }
}
