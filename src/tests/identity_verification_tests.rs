#[cfg(test)]
mod tests {
    use crate::ipfs_log::identity::{DefaultIdentificator, Identificator};
    use hex;
    use secp256k1;

    #[tokio::test]
    async fn test_verify_identity_cryptographically() {
        // Cria um identificador e uma identidade válida
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

        let _secp = secp256k1::Secp256k1::new();
        let public_key = secp256k1::PublicKey::from_slice(&pub_key_bytes.unwrap());
        assert!(
            public_key.is_ok(),
            "Public key should be valid secp256k1 key"
        );

        println!("✅ Identity cryptographic verification test components are valid");
    }

    #[tokio::test]
    async fn test_signature_verification_components() {
        use secp256k1::{Message, PublicKey, Secp256k1, SecretKey, ecdsa::Signature};
        use sha2::{Digest, Sha256};
        use std::str::FromStr;

        // Teste dos componentes de verificação de assinatura
        let secp = Secp256k1::new();

        // Cria uma chave de teste
        let secret_key = SecretKey::from_byte_array([1u8; 32]).expect("Valid secret key");
        let public_key = PublicKey::from_secret_key(&secp, &secret_key);

        // Cria uma mensagem de teste
        let message = "test message";
        let mut hasher = Sha256::new();
        hasher.update(message.as_bytes());
        let message_hash = hasher.finalize();
        let message_hash_array: [u8; 32] = message_hash.into();
        let secp_message = Message::from_digest(message_hash_array);

        // Assina a mensagem
        let signature = secp.sign_ecdsa(secp_message, &secret_key);

        // Verifica a assinatura
        let verification_result = secp.verify_ecdsa(secp_message, &signature, &public_key);
        assert!(
            verification_result.is_ok(),
            "Signature verification should succeed"
        );

        // Testa parse de assinatura de string
        let signature_str = signature.to_string();
        let parsed_signature = Signature::from_str(&signature_str);
        assert!(parsed_signature.is_ok(), "Signature parsing should succeed");

        println!("✅ Signature verification components work correctly");
    }
}
