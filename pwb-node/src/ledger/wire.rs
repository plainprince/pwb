//! Wire conversions for signed balance attestations and quorum certificates.

use pwb_core::net::{BalanceAttestationWire, QuorumCertificateWire};

use super::attest::{BalanceAttestation, QuorumCertificate};

impl From<&BalanceAttestation> for BalanceAttestationWire {
    fn from(attestation: &BalanceAttestation) -> Self {
        BalanceAttestationWire {
            subject: attestation.subject,
            epoch: attestation.epoch,
            balance: attestation.balance,
            head: attestation.head,
            attestor: attestation.attestor,
            signature: attestation.signature,
        }
    }
}

impl From<&BalanceAttestationWire> for BalanceAttestation {
    fn from(wire: &BalanceAttestationWire) -> Self {
        BalanceAttestation {
            subject: wire.subject,
            epoch: wire.epoch,
            balance: wire.balance,
            head: wire.head,
            attestor: wire.attestor,
            signature: wire.signature,
        }
    }
}

impl From<&QuorumCertificate> for QuorumCertificateWire {
    fn from(certificate: &QuorumCertificate) -> Self {
        QuorumCertificateWire {
            subject: certificate.subject,
            epoch: certificate.epoch,
            balance: certificate.balance,
            head: certificate.head,
            attestations: certificate
                .attestations
                .iter()
                .map(BalanceAttestationWire::from)
                .collect(),
        }
    }
}

impl From<&QuorumCertificateWire> for QuorumCertificate {
    fn from(wire: &QuorumCertificateWire) -> Self {
        QuorumCertificate {
            subject: wire.subject,
            epoch: wire.epoch,
            balance: wire.balance,
            head: wire.head,
            attestations: wire.attestations.iter().map(Into::into).collect(),
        }
    }
}
