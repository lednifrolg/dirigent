//! Pinning the hub's TLS certificate.
//!
//! The hub's certificate comes from an IKEA device CA but names no host a client could check, so
//! most clients accept any certificate. This crate pins it instead: pairing records the SHA-256
//! fingerprint of the certificate the hub presents, and every later connection must present the
//! same one. A factory reset gives the hub a new certificate, and needs a new pairing anyway.

use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, Mutex, PoisonError};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, ClientConfig, DigitallySignedStruct, OtherError, SignatureScheme};
use sha2::{Digest, Sha256};

/// SHA-256 fingerprint of a certificate in DER form. Shown and parsed as 32 hex byte pairs
/// separated by colons, like `openssl x509 -fingerprint -sha256` prints it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    pub fn of(der: &[u8]) -> Self {
        Self(Sha256::digest(der).into())
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, byte) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(":")?;
            }
            write!(f, "{byte:02X}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({self})")
    }
}

/// The text is not a SHA-256 fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a SHA-256 fingerprint: expected 64 hex digits, optionally separated by colons")]
pub struct ParseFingerprintError;

impl FromStr for Fingerprint {
    type Err = ParseFingerprintError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let digits: Vec<u8> = text.bytes().filter(|&b| b != b':').collect();
        if digits.len() != 64 || !digits.iter().all(u8::is_ascii_hexdigit) {
            return Err(ParseFingerprintError);
        }
        let mut bytes = [0; 32];
        for (byte, pair) in bytes.iter_mut().zip(digits.as_chunks::<2>().0) {
            let pair = std::str::from_utf8(pair).map_err(|_| ParseFingerprintError)?;
            *byte = u8::from_str_radix(pair, 16).map_err(|_| ParseFingerprintError)?;
        }
        Ok(Self(bytes))
    }
}

/// The hub presented another certificate than the pinned one.
#[derive(Debug, thiserror::Error)]
#[error("the hub presented certificate {found}, not the pinned {expected}")]
struct PinMismatch {
    expected: Fingerprint,
    found: Fingerprint,
}

/// Accepts the pinned certificate, or any certificate when there is no pin yet, and records the
/// one it saw. Handshake signatures are verified as usual, so the peer must hold the
/// certificate's key.
#[derive(Debug)]
struct PinVerifier {
    pin: Option<Fingerprint>,
    seen: Arc<Mutex<Option<Fingerprint>>>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let found = Fingerprint::of(end_entity);
        *self.seen.lock().unwrap_or_else(PoisonError::into_inner) = Some(found);
        match self.pin {
            Some(expected) if expected != found => Err(rustls::Error::InvalidCertificate(
                CertificateError::Other(OtherError(Arc::new(PinMismatch { expected, found }))),
            )),
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// TLS settings for one hub, and the certificate it presented last.
#[derive(Debug, Clone)]
pub(crate) struct Tls {
    pub(crate) config: Arc<ClientConfig>,
    pin: Option<Fingerprint>,
    seen: Arc<Mutex<Option<Fingerprint>>>,
}

impl Tls {
    /// Accepts only the certificate with `pin`; without a pin, any certificate, for pairing.
    pub(crate) fn new(pin: Option<Fingerprint>) -> Result<Self, rustls::Error> {
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let seen = Arc::default();
        let verifier = Arc::new(PinVerifier {
            pin,
            seen: Arc::clone(&seen),
            provider: Arc::clone(&provider),
        });
        let config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        Ok(Self {
            config: Arc::new(config),
            pin,
            seen,
        })
    }

    /// Fingerprint of the certificate in the latest handshake.
    pub(crate) fn seen(&self) -> Option<Fingerprint> {
        *self.seen.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The pinned and the presented certificate, if the latest handshake failed on the pin.
    pub(crate) fn mismatch(&self) -> Option<(Fingerprint, Fingerprint)> {
        let expected = self.pin?;
        self.seen()
            .filter(|&found| found != expected)
            .map(|found| (expected, found))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_print_and_parse_like_openssl() {
        let print = Fingerprint::of(b"certificate");
        let text = print.to_string();
        assert_eq!(text.len(), 32 * 3 - 1);
        assert_eq!(text.parse::<Fingerprint>(), Ok(print));
        assert_eq!(text.replace(':', "").to_lowercase().parse(), Ok(print));
        assert_eq!("AB:CD".parse::<Fingerprint>(), Err(ParseFingerprintError));
        assert_eq!(
            "zz".repeat(32).parse::<Fingerprint>(),
            Err(ParseFingerprintError)
        );
        assert_eq!(
            "+a".repeat(32).parse::<Fingerprint>(),
            Err(ParseFingerprintError)
        );
    }
}
