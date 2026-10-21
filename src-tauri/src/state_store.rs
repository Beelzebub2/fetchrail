//! Versioned state snapshots with validated previous generations. Windows seals
//! the entire private catalog with user-scoped DPAPI, including signed source URLs.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Serialize, Deserialize)]
struct Envelope {
    fetchrail_state: u8,
    sha256: String,
    protected: bool,
    payload: String,
}

#[cfg(windows)]
fn crypt(bytes: &[u8], protect: bool) -> Result<Vec<u8>, String> {
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::{LocalFree, HLOCAL},
            Security::Cryptography::{
                CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
            },
        },
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len().try_into().map_err(|_| "State is too large")?,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        if protect { CryptProtectData(&input, PCWSTR::null(), None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output) }
        else { CryptUnprotectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output) }
    }.map_err(|_| "Protected state could not be accessed by this Windows user. Restore using the original account or reauthorize downloads.".to_string())?;
    let result =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        LocalFree(Some(HLOCAL(output.pbData.cast())));
    }
    Ok(result)
}

pub fn encode(bytes: &[u8]) -> Result<Vec<u8>, String> {
    #[cfg(windows)]
    let payload = crypt(bytes, true)?;
    #[cfg(not(windows))]
    let payload = bytes.to_vec();
    serde_json::to_vec(&Envelope {
        fetchrail_state: 1,
        sha256: format!("{:x}", Sha256::digest(&payload)),
        protected: cfg!(windows),
        payload: STANDARD.encode(payload),
    })
    .map_err(|e| e.to_string())
}

pub fn decode(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if value.get("fetchrail_state").is_none() {
        return Ok(bytes.to_vec());
    } // Legacy plaintext migration.
    let envelope: Envelope = serde_json::from_value(value).map_err(|e| e.to_string())?;
    if envelope.fetchrail_state != 1 {
        return Err("Unsupported state format; data was preserved.".into());
    }
    let payload = STANDARD
        .decode(envelope.payload)
        .map_err(|_| "State payload is damaged")?;
    if envelope.sha256 != format!("{:x}", Sha256::digest(&payload)) {
        return Err("State checksum mismatch; data was preserved.".into());
    }
    let payload = if envelope.protected {
        #[cfg(windows)]
        {
            crypt(&payload, false)?
        }
        #[cfg(not(windows))]
        {
            return Err(
                "This state belongs to a Windows account. Restore on its original computer.".into(),
            );
        }
    } else {
        payload
    };
    Ok(payload)
}

pub fn read<T: DeserializeOwned>(path: &Path) -> Result<T, String> {
    serde_json::from_slice(&decode(&std::fs::read(path).map_err(|e| e.to_string())?)?)
        .map_err(|e| e.to_string())
}

pub fn write<T: Serialize + DeserializeOwned>(path: &Path, value: &T) -> Result<(), String> {
    // Never replace the last known-good backup with corrupted or unvalidated state.
    if let Ok(previous) = std::fs::read(path) {
        if let Ok(decoded) = decode(&previous) {
            if serde_json::from_slice::<T>(&decoded).is_ok() {
                // Reuse validated ciphertext; reserializing and encrypting an unchanged
                // previous generation costs O(history) work at every catalog update.
                let protected = serde_json::from_slice::<serde_json::Value>(&previous)
                    .ok()
                    .is_some_and(|v| v["fetchrail_state"] == 1 && v["protected"] == true);
                let backup = if cfg!(windows) && !protected {
                    encode(&decoded)?
                } else {
                    previous
                };
                crate::platform::write_private_atomic(&path.with_extension("json.backup"), &backup)
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    crate::platform::write_private_atomic(path, &encode(&bytes)?).map_err(|e| e.to_string())
}

/// Apply the secret policy to both generations, including legacy plaintext.
pub fn write_settings(
    path: &Path,
    settings: &crate::model::DownloadSettings,
) -> Result<(), String> {
    fn persisted(mut value: crate::model::DownloadSettings) -> crate::model::DownloadSettings {
        if !cfg!(windows) {
            value.torrent.proxy_password.clear();
        }
        value.extra.remove("runtimeWarning");
        value
    }
    let backup = path.with_extension("json.backup");
    if let Ok(previous) = read::<crate::model::DownloadSettings>(path)
        .or_else(|_| read::<crate::model::DownloadSettings>(&backup))
    {
        let bytes = serde_json::to_vec(&persisted(previous)).map_err(|e| e.to_string())?;
        crate::platform::write_private_atomic(&backup, &encode(&bytes)?)
            .map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_vec(&persisted(settings.clone())).map_err(|e| e.to_string())?;
    crate::platform::write_private_atomic(path, &encode(&bytes)?).map_err(|e| e.to_string())
}

/// Publication cleanup must be recoverable even if the primary catalog is later
/// damaged. A completed transition updates the last-valid backup before its journal goes away.
pub fn acknowledge<T: DeserializeOwned>(path: &Path) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let _: T = serde_json::from_slice(&decode(&bytes)?).map_err(|e| e.to_string())?;
    crate::platform::write_private_atomic(&path.with_extension("json.backup"), &bytes)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validated_previous_encoding_is_preserved_byte_for_byte_in_backup() {
        let dir =
            std::env::temp_dir().join(format!("fetchrail-state-copy-{}", uuid::Uuid::new_v4()));
        let path = dir.join("downloads.json");
        let previous = vec!["synthetic-previous-generation".to_string()];
        let encoded = encode(&serde_json::to_vec(&previous).unwrap()).unwrap();
        // Noncanonical whitespace also makes needless re-encoding observable off Windows.
        let envelope: Envelope = serde_json::from_slice(&encoded).unwrap();
        let stored = serde_json::to_vec_pretty(&envelope).unwrap();
        crate::platform::write_private_atomic(&path, &stored).unwrap();
        let next = vec!["replacement-generation".to_string()];

        write(&path, &next).unwrap();

        let backup = std::fs::read(path.with_extension("json.backup")).unwrap();
        assert_eq!(backup, stored);
        assert_eq!(
            read::<Vec<String>>(&path.with_extension("json.backup")).unwrap(),
            previous
        );
        assert_eq!(read::<Vec<String>>(&path).unwrap(), next);
        #[cfg(windows)]
        assert!(
            serde_json::from_slice::<Envelope>(&backup)
                .unwrap()
                .protected
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn damaged_or_wrong_typed_primary_never_replaces_a_valid_backup() {
        let dir =
            std::env::temp_dir().join(format!("fetchrail-state-valid-{}", uuid::Uuid::new_v4()));
        let path = dir.join("downloads.json");
        write(&path, &vec!["known-good-backup".to_string()]).unwrap();
        write(&path, &vec!["current".to_string()]).unwrap();
        let backup_path = path.with_extension("json.backup");
        let known_good = std::fs::read(&backup_path).unwrap();
        let mut damaged: Envelope =
            serde_json::from_slice(&encode(br#"["damaged-generation"]"#).unwrap()).unwrap();
        damaged.sha256 = "0".repeat(64);
        let invalid_generations = [
            b"{".to_vec(),
            serde_json::to_vec(&damaged).unwrap(),
            encode(br#"{"wrongType":true}"#).unwrap(),
        ];
        for invalid in invalid_generations {
            std::fs::write(&path, invalid).unwrap();
            let replacement = vec!["valid-replacement".to_string()];
            write(&path, &replacement).unwrap();
            assert_eq!(std::fs::read(&backup_path).unwrap(), known_good);
            assert_eq!(read::<Vec<String>>(&path).unwrap(), replacement);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn legacy_plaintext_generations_are_protected_before_becoming_backups() {
        let previous = vec!["synthetic-legacy-secret".to_string()];
        let plaintext = serde_json::to_vec(&previous).unwrap();
        let unprotected = serde_json::to_vec(&Envelope {
            fetchrail_state: 1,
            sha256: format!("{:x}", Sha256::digest(&plaintext)),
            protected: false,
            payload: STANDARD.encode(&plaintext),
        })
        .unwrap();
        for legacy in [plaintext.clone(), unprotected] {
            let dir = std::env::temp_dir().join(format!(
                "fetchrail-state-migration-{}",
                uuid::Uuid::new_v4()
            ));
            let path = dir.join("downloads.json");
            crate::platform::write_private_atomic(&path, &legacy).unwrap();

            write(&path, &vec!["new-generation".to_string()]).unwrap();

            let backup = std::fs::read(path.with_extension("json.backup")).unwrap();
            let envelope: Envelope = serde_json::from_slice(&backup).unwrap();
            assert!(envelope.protected);
            assert_ne!(backup, legacy);
            assert_eq!(decode(&backup).unwrap(), plaintext);
            assert_eq!(
                read::<Vec<String>>(&path.with_extension("json.backup")).unwrap(),
                previous
            );
            assert!(!String::from_utf8(backup)
                .unwrap()
                .contains("synthetic-legacy-secret"));
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
    #[test]
    fn checksum_covers_stored_payload_without_exposing_plaintext_digest() {
        let plain = br#"{"password":"synthetic-test-password"}"#;
        let bytes = encode(plain).unwrap();
        let mut envelope: Envelope = serde_json::from_slice(&bytes).unwrap();
        let stored = STANDARD.decode(&envelope.payload).unwrap();
        assert_eq!(envelope.sha256, format!("{:x}", Sha256::digest(&stored)));
        #[cfg(windows)]
        assert_ne!(envelope.sha256, format!("{:x}", Sha256::digest(plain)));
        assert_eq!(decode(&bytes).unwrap(), plain);
        envelope.payload = STANDARD.encode(b"damaged encrypted payload");
        assert!(decode(&serde_json::to_vec(&envelope).unwrap())
            .unwrap_err()
            .contains("checksum"));
    }
    #[test]
    fn validates_backup_and_preserves_previous_generation_after_corruption() {
        let dir = std::env::temp_dir().join(format!("fetchrail-state-{}", uuid::Uuid::new_v4()));
        let path = dir.join("downloads.json");
        write(&path, &vec!["signed-secret-token".to_string()]).unwrap();
        write(&path, &vec!["second".to_string()]).unwrap();
        #[cfg(windows)]
        assert!(
            !std::fs::read_to_string(&path.with_extension("json.backup"))
                .unwrap()
                .contains("signed-secret-token")
        );
        std::fs::write(&path, b"{").unwrap();
        assert!(read::<Vec<String>>(&path).is_err());
        assert_eq!(
            read::<Vec<String>>(&path.with_extension("json.backup")).unwrap(),
            vec!["signed-secret-token"]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
