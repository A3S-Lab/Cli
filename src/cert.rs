use crate::error::{DevError, Result};
use rcgen::{CertificateParams, DistinguishedName, DnType, SanType};
use std::path::Path;

/// Generate a self-signed certificate for localhost development.
pub fn generate_self_signed_cert() -> Result<(Vec<u8>, Vec<u8>)> {
    let mut params =
        CertificateParams::new(vec!["localhost".to_string(), "*.localhost".to_string()])
            .map_err(|e| DevError::Config(format!("failed to create certificate params: {}", e)))?;

    // Set subject
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "localhost");
    dn.push(DnType::OrganizationName, "A3S Development");
    params.distinguished_name = dn;

    // Add IP SAN
    params
        .subject_alt_names
        .push(SanType::IpAddress(std::net::IpAddr::V4(
            std::net::Ipv4Addr::new(127, 0, 0, 1),
        )));

    // Generate certificate
    let key_pair = rcgen::KeyPair::generate()
        .map_err(|e| DevError::Config(format!("failed to generate key pair: {}", e)))?;

    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| DevError::Config(format!("failed to generate certificate: {}", e)))?;

    let cert_pem = cert.pem().into_bytes();
    let key_pem = key_pair.serialize_pem().into_bytes();

    Ok((cert_pem, key_pem))
}

/// Get or create certificate files in the project directory.
pub async fn get_or_create_cert(project_dir: &Path) -> Result<(Vec<u8>, Vec<u8>)> {
    let cert_dir = project_dir.join(".a3s");
    let cert_path = cert_dir.join("cert.pem");
    let key_path = cert_dir.join("key.pem");

    // Check if certificate already exists
    if cert_path.exists() && key_path.exists() {
        let cert = tokio::fs::read(&cert_path)
            .await
            .map_err(|e| DevError::Config(format!("failed to read certificate: {}", e)))?;
        let key = tokio::fs::read(&key_path)
            .await
            .map_err(|e| DevError::Config(format!("failed to read private key: {}", e)))?;
        return Ok((cert, key));
    }

    // Generate new certificate
    let (cert, key) = generate_self_signed_cert()?;

    // Create .a3s directory if it doesn't exist
    tokio::fs::create_dir_all(&cert_dir)
        .await
        .map_err(|e| DevError::Config(format!("failed to create .a3s directory: {}", e)))?;

    // Save certificate and key
    tokio::fs::write(&cert_path, &cert)
        .await
        .map_err(|e| DevError::Config(format!("failed to write certificate: {}", e)))?;
    tokio::fs::write(&key_path, &key)
        .await
        .map_err(|e| DevError::Config(format!("failed to write private key: {}", e)))?;

    tracing::info!(
        "generated self-signed certificate at {}",
        cert_path.display()
    );

    Ok((cert, key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_self_signed_cert() {
        let result = generate_self_signed_cert();
        assert!(result.is_ok());

        let (cert_pem, key_pem) = result.unwrap();

        // Check that cert and key are not empty
        assert!(!cert_pem.is_empty());
        assert!(!key_pem.is_empty());

        // Check that cert starts with PEM header
        let cert_str = String::from_utf8_lossy(&cert_pem);
        assert!(cert_str.contains("-----BEGIN CERTIFICATE-----"));
        assert!(cert_str.contains("-----END CERTIFICATE-----"));

        // Check that key starts with PEM header
        let key_str = String::from_utf8_lossy(&key_pem);
        assert!(
            key_str.contains("-----BEGIN PRIVATE KEY-----")
                || key_str.contains("-----BEGIN RSA PRIVATE KEY-----")
        );
    }

    #[tokio::test]
    async fn test_get_or_create_cert_creates_new() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_dir = temp_dir.path();

        let result = get_or_create_cert(project_dir).await;
        assert!(result.is_ok());

        let (cert, key) = result.unwrap();
        assert!(!cert.is_empty());
        assert!(!key.is_empty());

        // Check that files were created
        assert!(project_dir.join(".a3s/cert.pem").exists());
        assert!(project_dir.join(".a3s/key.pem").exists());
    }

    #[tokio::test]
    async fn test_get_or_create_cert_reuses_existing() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_dir = temp_dir.path();

        // First call creates cert
        let (cert1, key1) = get_or_create_cert(project_dir).await.unwrap();

        // Second call should reuse existing cert
        let (cert2, key2) = get_or_create_cert(project_dir).await.unwrap();

        // Should be identical
        assert_eq!(cert1, cert2);
        assert_eq!(key1, key2);
    }
}
