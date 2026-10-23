//! Guard against trusting the compromised eMudhra root.
//!
//! In Oct 2026 eight eMudhra intermediate CA private keys were recovered and
//! distrusted (Go CVE-2026-111943, Mozilla, Chrome, Apple). tidx's outbound
//! TLS (reqwest -> api.kaspa.org, rpc.igralabs.com) uses the `webpki-roots`
//! trust anchors compiled into the binary, NOT the host CA store, so a host
//! `ca-certificates` update does not protect it.
//!
//! webpki-roots 1.0.10 removed the affected root `emSign ECC Root CA - G3`
//! (present through 1.0.5). This test fails if that root is trusted again,
//! i.e. a regression below 1.0.10.
//!
//! Scope note: this only covers the directly-distrusted emSign root. The other
//! emSign roots (C1, G1, ECC C3) are unrelated and remain legitimately trusted.
//! A roots bump does NOT address the Sectigo "AAA Certificate Services"
//! cross-signed path for the compromised intermediates — closing that in Rust
//! would require an intermediate-SPKI denylist in a custom verifier (as Go
//! added), tracked separately.

#[test]
fn compromised_emsign_g3_root_not_trusted() {
    let marker: &[u8] = b"emSign ECC Root CA - G3";
    let present = webpki_roots::TLS_SERVER_ROOTS.iter().any(|anchor| {
        let subject: &[u8] = anchor.subject.as_ref();
        subject.windows(marker.len()).any(|w| w == marker)
    });
    assert!(
        !present,
        "compromised root `emSign ECC Root CA - G3` is trusted (need webpki-roots >= 1.0.10)"
    );
}
