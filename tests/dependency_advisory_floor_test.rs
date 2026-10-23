//! Guard against regressing below dependency versions that fix known advisories.
//!
//! Found by osv-scanner on 2026-10-23 against `Cargo.lock`. Each floor is the
//! first release on our semver line that fixes every advisory listed for it.
//! A `cargo update` or a new dependency that pulls in an older copy fails here.

const FLOORS: &[(&str, &str, &str)] = &[
    // RUSTSEC-2026-0044..0048: X.509 name constraints, PKCS7_verify, CRL scope
    (
        "aws-lc-sys",
        "0.39.0",
        "RUSTSEC-2026-0044/0045/0046/0047/0048",
    ),
    // RUSTSEC-2026-0049/0098/0099/0104/0382: CRL, name constraints, distrusted intermediates
    (
        "rustls-webpki",
        "0.103.16",
        "RUSTSEC-2026-0049/0098/0099/0104/0382",
    ),
    // RUSTSEC-2026-0179/0180: unbounded SCRAM iterations, hstore panic
    ("postgres-protocol", "0.6.12", "RUSTSEC-2026-0179/0180"),
    // RUSTSEC-2026-0037/0185/0338/0339/0340/0342: remote DoS in QUIC endpoints
    (
        "quinn-proto",
        "0.11.18",
        "RUSTSEC-2026-0037/0185/0338/0339/0340/0342",
    ),
    // RUSTSEC-2026-0041: decompression leaks uninitialized memory
    ("lz4_flex", "0.11.6", "RUSTSEC-2026-0041"),
    // RUSTSEC-2026-0194/0195: quadratic attribute check, unbounded NsReader allocation
    ("quick-xml", "0.41.0", "RUSTSEC-2026-0194/0195"),
];

fn parse(v: &str) -> Vec<u64> {
    v.split(['-', '+'])
        .next()
        .unwrap()
        .split('.')
        .map(|p| p.parse().unwrap())
        .collect()
}

fn locked_versions(lock: &str, name: &str) -> Vec<String> {
    let name_line = format!("name = \"{name}\"");
    let mut lines = lock.lines();
    let mut out = Vec::new();
    while let Some(line) = lines.next() {
        if line == name_line {
            if let Some(v) = lines.next().and_then(|l| l.strip_prefix("version = \"")) {
                out.push(v.trim_end_matches('"').to_string());
            }
        }
    }
    out
}

#[test]
fn locked_dependencies_include_advisory_fixes() {
    let lock = include_str!("../Cargo.lock");
    let mut stale = Vec::new();
    for (name, floor, advisories) in FLOORS {
        for v in locked_versions(lock, name) {
            if parse(&v) < parse(floor) {
                stale.push(format!("{name} {v} < {floor} ({advisories})"));
            }
        }
    }
    assert!(
        stale.is_empty(),
        "vulnerable locked versions:\n{}",
        stale.join("\n")
    );
}
