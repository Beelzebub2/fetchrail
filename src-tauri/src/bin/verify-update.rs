use minisign_verify::{PublicKey, Signature};
fn main() {
    if let Err(error) = verify() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
fn verify() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments.len() != 4 {
        return Err("Usage: verify-update ARTIFACT SIGNATURE PUBLIC_KEY VERSION".into());
    }
    use base64::{engine::general_purpose::STANDARD, Engine};
    use std::io::Read;
    // Tauri serializes the minisign text as base64 in its configuration and manifests.
    let decode = |encoded: &str| -> Result<String, Box<dyn std::error::Error>> {
        Ok(String::from_utf8(STANDARD.decode(encoded.trim())?)?)
    };
    let key = PublicKey::decode(&decode(&arguments[2])?)?;
    let text = std::fs::read_to_string(&arguments[1])?;
    let signature = Signature::decode(&decode(&text)?)?;
    let mut bytes = Vec::new();
    std::fs::File::open(&arguments[0])?.read_to_end(&mut bytes)?;
    key.verify(&bytes, &signature, true)?;
    let version = signature
        .trusted_comment()
        .split('\t')
        .find_map(|part| part.strip_prefix("version:"));
    if version != Some(arguments[3].as_str()) {
        return Err("Signed artifact version does not match the release".into());
    }
    Ok(())
}
