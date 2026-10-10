//! The `FMENC1` envelope. Every file First Mate writes that holds user data
//! carries it, and `docs/ENCRYPTION.md` records the design.
//!
//! Each file carries its own AES-256 key. That key travels inside the file in
//! two wraps: one under the Platform Crypto Provider key, which the TPM holds,
//! and one under the recovery key, which the user keeps offline.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, OnceLock};

use sha2::{Digest, Sha256};
use windows::core::{Error, PCWSTR, w};
use windows::Win32::Security::Cryptography::{
    BCRYPT_AES_ALGORITHM, BCRYPT_ALG_HANDLE, BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO,
    BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO_VERSION, BCRYPT_CHAINING_MODE,
    BCRYPT_FLAGS, BCRYPT_HANDLE, BCRYPT_KEY_HANDLE, BCRYPT_OAEP_PADDING_INFO,
    BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS, BCRYPT_SHA256_ALGORITHM, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
    BCryptCloseAlgorithmProvider, BCryptDecrypt, BCryptDestroyKey, BCryptEncrypt, BCryptGenRandom,
    BCryptGenerateSymmetricKey, BCryptOpenAlgorithmProvider, BCryptSetProperty, CERT_KEY_SPEC,
    NCRYPT_HANDLE, NCRYPT_KEY_HANDLE, NCRYPT_PAD_OAEP_FLAG, NCRYPT_PROV_HANDLE, NCRYPT_SILENT_FLAG,
    NCryptCreatePersistedKey, NCryptDecrypt, NCryptEncrypt, NCryptFinalizeKey, NCryptOpenKey,
    NCryptOpenStorageProvider, NCryptSetProperty,
};

const PROVIDER: &str = "Microsoft Platform Crypto Provider";
const KEY_NAME: &str = "FirstMateFileKey";
const MAGIC: &[u8; 6] = b"FMENC1";
const VERSION: u8 = 1;
const FILE_KEY: usize = 32;
const NONCE: usize = 12;
const TAG: usize = 16;
const RSA_BITS: u32 = 2048;

fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn utf16_bytes(text: &str) -> Vec<u8> {
    utf16(text).iter().flat_map(|unit| unit.to_le_bytes()).collect()
}

fn why(error: Error) -> String {
    format!("0x{:08X}", error.code().0 as u32)
}

fn open_provider() -> Result<NCRYPT_PROV_HANDLE, String> {
    let name = utf16(PROVIDER);
    let mut handle = NCRYPT_PROV_HANDLE::default();
    unsafe { NCryptOpenStorageProvider(&mut handle, PCWSTR(name.as_ptr()), 0) }.map(|_| handle).map_err(why)
}

static PROVIDER_HANDLE: LazyLock<Result<NCRYPT_PROV_HANDLE, String>> = LazyLock::new(open_provider);

fn provider() -> Result<NCRYPT_PROV_HANDLE, String> {
    PROVIDER_HANDLE.clone()
}

/// The persisted RSA key `name`, or `Err` when that key does not exist.
fn open_key(name: &str) -> Result<NCRYPT_KEY_HANDLE, String> {
    let prov = provider()?;
    let name = utf16(name);
    let mut key = NCRYPT_KEY_HANDLE::default();
    unsafe { NCryptOpenKey(prov, &mut key, PCWSTR(name.as_ptr()), CERT_KEY_SPEC(0), NCRYPT_SILENT_FLAG) }
        .map_err(why)?;
    Ok(key)
}

/// The persisted RSA key `name`, created with no prompt when it is absent.
/// `NCRYPT_SILENT_FLAG` on create and on finalize is what keeps the TPM from
/// asking for consent, because the app must read its own files at logon.
fn persisted_key(name: &str) -> Result<NCRYPT_KEY_HANDLE, String> {
    if let Ok(key) = open_key(name) {
        return Ok(key);
    }
    let prov = provider()?;
    let name = utf16(name);
    let alg = utf16("RSA");
    let mut key = NCRYPT_KEY_HANDLE::default();
    unsafe {
        NCryptCreatePersistedKey(
            prov,
            &mut key,
            PCWSTR(alg.as_ptr()),
            PCWSTR(name.as_ptr()),
            CERT_KEY_SPEC(0),
            NCRYPT_SILENT_FLAG,
        )
    }
    .map_err(why)?;
    unsafe { NCryptSetProperty(NCRYPT_HANDLE(key.0), w!("Length"), &RSA_BITS.to_le_bytes(), NCRYPT_SILENT_FLAG) }
        .map_err(why)?;
    unsafe { NCryptFinalizeKey(key, NCRYPT_SILENT_FLAG) }.map_err(why)?;
    Ok(key)
}

static MASTER_KEY: LazyLock<Result<NCRYPT_KEY_HANDLE, String>> = LazyLock::new(|| persisted_key(KEY_NAME));

fn master_key() -> Result<NCRYPT_KEY_HANDLE, String> {
    MASTER_KEY.clone()
}

/// `false` when the machine has no Platform Crypto Provider, so no envelope can
/// open there.
#[cfg(test)]
pub(crate) fn tpm_ready() -> bool {
    provider().is_ok() && master_key().is_ok()
}

fn random(bytes: &mut [u8]) -> Result<(), String> {
    unsafe { BCryptGenRandom(None, bytes, BCRYPT_USE_SYSTEM_PREFERRED_RNG) }.ok().map_err(why)
}

fn oaep() -> BCRYPT_OAEP_PADDING_INFO {
    BCRYPT_OAEP_PADDING_INFO { pszAlgId: BCRYPT_SHA256_ALGORITHM, pbLabel: std::ptr::null_mut(), cbLabel: 0 }
}

fn wrap(plain: &[u8]) -> Result<Vec<u8>, String> {
    wrap_with(master_key()?, plain)
}

fn wrap_with(key: NCRYPT_KEY_HANDLE, plain: &[u8]) -> Result<Vec<u8>, String> {
    let info = oaep();
    let pad = Some(&info as *const BCRYPT_OAEP_PADDING_INFO as *const c_void);
    let flags = NCRYPT_PAD_OAEP_FLAG | NCRYPT_SILENT_FLAG;
    let mut need = 0u32;
    unsafe { NCryptEncrypt(key, Some(plain), pad, None, &mut need, flags) }.map_err(why)?;
    let mut out = vec![0u8; need as usize];
    let mut got = 0u32;
    unsafe { NCryptEncrypt(key, Some(plain), pad, Some(&mut out), &mut got, flags) }.map_err(why)?;
    out.truncate(got as usize);
    Ok(out)
}

fn unwrap(wrapped: &[u8]) -> Result<Vec<u8>, String> {
    unwrap_with(master_key()?, wrapped)
}

fn unwrap_with(key: NCRYPT_KEY_HANDLE, wrapped: &[u8]) -> Result<Vec<u8>, String> {
    let info = oaep();
    let pad = Some(&info as *const BCRYPT_OAEP_PADDING_INFO as *const c_void);
    let flags = NCRYPT_PAD_OAEP_FLAG | NCRYPT_SILENT_FLAG;
    let mut need = 0u32;
    unsafe { NCryptDecrypt(key, Some(wrapped), pad, None, &mut need, flags) }.map_err(why)?;
    let mut out = vec![0u8; need as usize];
    let mut got = 0u32;
    unsafe { NCryptDecrypt(key, Some(wrapped), pad, Some(&mut out), &mut got, flags) }.map_err(why)?;
    out.truncate(got as usize);
    Ok(out)
}

/// A fresh algorithm handle plus the key for one file key. A CNG handle holds a
/// raw pointer, so no static may cache it.
fn aes_key(secret: &[u8]) -> Result<(BCRYPT_ALG_HANDLE, BCRYPT_KEY_HANDLE), String> {
    let mut alg = BCRYPT_ALG_HANDLE::default();
    unsafe {
        BCryptOpenAlgorithmProvider(&mut alg, BCRYPT_AES_ALGORITHM, None, BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0))
            .ok()
            .map_err(why)?;
        BCryptSetProperty(BCRYPT_HANDLE(alg.0), BCRYPT_CHAINING_MODE, &utf16_bytes("ChainingModeGCM"), 0)
            .ok()
            .map_err(why)?;
        // The software AES provider answers NTE_NOT_SUPPORTED for
        // `AuthTagLength` on the algorithm handle, and its default tag length
        // is the 16 bytes `TAG` names.
    }
    let mut key = BCRYPT_KEY_HANDLE::default();
    unsafe { BCryptGenerateSymmetricKey(alg, &mut key, None, secret, 0) }
        .ok()
        .inspect_err(|_| {
            let _ = unsafe { BCryptCloseAlgorithmProvider(alg, 0) };
        })
        .map_err(why)?;
    Ok((alg, key))
}

fn auth_info<'a>(nonce: &'a mut [u8], tag: &'a mut [u8], aad: &'a [u8]) -> BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO {
    BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO {
        cbSize: std::mem::size_of::<BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO>() as u32,
        dwInfoVersion: BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO_VERSION,
        pbNonce: nonce.as_mut_ptr(),
        cbNonce: nonce.len() as u32,
        pbAuthData: aad.as_ptr() as *mut u8,
        cbAuthData: aad.len() as u32,
        pbTag: tag.as_mut_ptr(),
        cbTag: tag.len() as u32,
        ..Default::default()
    }
}

/// `(alg, key)` closed after one AES-256-GCM pass.
fn skey_seal(secret: &[u8], aad: &[u8], plain: &[u8]) -> Result<Vec<u8>, String> {
    let (alg, key) = aes_key(secret)?;
    let mut nonce = vec![0u8; NONCE];
    let mut tag = vec![0u8; TAG];
    random(&mut nonce)?;
    let mut info = auth_info(&mut nonce, &mut tag, aad);
    let mut out = vec![0u8; plain.len()];
    let mut got = 0u32;
    let status = unsafe {
        BCryptEncrypt(key, Some(plain), Some(&mut info as *mut _ as *const c_void), None, Some(&mut out), &mut got, BCRYPT_FLAGS(0))
    };
    close_aes(alg, key);
    status.ok().map_err(why)?;
    let mut record = nonce;
    record.extend_from_slice(&tag);
    record.extend_from_slice(&out[..got as usize]);
    Ok(record)
}

fn skey_open(secret: &[u8], aad: &[u8], record: &[u8]) -> Result<Vec<u8>, String> {
    if record.len() < NONCE + TAG {
        return Err("short GCM record".into());
    }
    let (alg, key) = aes_key(secret)?;
    let mut nonce = record[..NONCE].to_vec();
    let mut tag = record[NONCE..NONCE + TAG].to_vec();
    let mut info = auth_info(&mut nonce, &mut tag, aad);
    let cipher = &record[NONCE + TAG..];
    let mut out = vec![0u8; cipher.len()];
    let mut got = 0u32;
    let status = unsafe {
        BCryptDecrypt(key, Some(cipher), Some(&mut info as *mut _ as *const c_void), None, Some(&mut out), &mut got, BCRYPT_FLAGS(0))
    };
    close_aes(alg, key);
    status.ok().map_err(why)?;
    out.truncate(got as usize);
    Ok(out)
}

fn close_aes(alg: BCRYPT_ALG_HANDLE, key: BCRYPT_KEY_HANDLE) {
    unsafe {
        let _ = BCryptDestroyKey(key);
        let _ = BCryptCloseAlgorithmProvider(alg, 0);
    }
}

fn len_prefixed(blob: &[u8]) -> Vec<u8> {
    let mut out = (blob.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(blob);
    out
}

/// `FMENC1 | version | file key under the TPM key | file key under the recovery
/// key | AES-256-GCM record`. The header is the record's additional data, so
/// either wrap swapped with another file fails the tag.
pub fn seal(plain: &[u8]) -> Result<Vec<u8>, String> {
    let mut file_key = [0u8; FILE_KEY];
    random(&mut file_key)?;
    let mut header = MAGIC.to_vec();
    header.push(VERSION);
    header.extend_from_slice(&len_prefixed(&wrap(&file_key)?));
    let recovery = match recovery_secret() {
        Ok(secret) => skey_seal(&derive_recovery(&secret), MAGIC, &file_key)?,
        Err(e) => {
            crate::log::log(&format!("ENVELOPE recovery wrap unavailable: {e}"));
            Vec::new()
        }
    };
    header.extend_from_slice(&len_prefixed(&recovery));
    let body = skey_seal(&file_key, &header, plain)?;
    let mut out = header;
    out.extend_from_slice(&body);
    Ok(out)
}

pub fn open(sealed: &[u8]) -> Result<Vec<u8>, String> {
    if !looks_sealed(sealed) {
        return Err("not an FMENC1 file".into());
    }
    let mut at = MAGIC.len() + 1;
    let take = |at: &mut usize| -> Result<&[u8], String> {
        let len = u32::from_le_bytes(
            sealed.get(*at..*at + 4).ok_or("truncated FMENC1 header")?.try_into().map_err(|_| "truncated FMENC1 header")?,
        ) as usize;
        *at += 4;
        let blob = sealed.get(*at..*at + len).ok_or("truncated FMENC1 body")?;
        *at += len;
        Ok(blob)
    };
    let tpm_wrap = take(&mut at)?;
    let recovery_wrap = take(&mut at)?;
    let file_key = unwrap(tpm_wrap)?;
    if recovery_wrap.is_empty() {
        crate::log::log("ENVELOPE file carries no recovery wrap");
    }
    skey_open(&file_key, &sealed[..at], &sealed[at..])
}

pub fn looks_sealed(bytes: &[u8]) -> bool {
    bytes.len() > MAGIC.len() + 1 && bytes.starts_with(MAGIC) && bytes[MAGIC.len()] == VERSION
}

/// `%APPDATA%\com.dusti.firstmate`, where the recovery wrap lives. `lib.rs`
/// sets it before the first store read, because the path comes from Tauri.
static STORE_DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn init(dir: PathBuf) {
    let _ = STORE_DIR.set(dir);
}

fn store_dir() -> PathBuf {
    STORE_DIR.get().cloned().unwrap_or_else(crate::log::fm_dir)
}

/// A sealed file opens only inside First Mate's own directories, so
/// `read_file` never hands back decrypted bytes for a file the user owns.
/// `Path::canonicalize` returns a `\\?\` verbatim path, and no store prefix
/// matches that form, so the test compares the drive path.
pub fn is_store(path: &Path) -> bool {
    let text = path.to_string_lossy();
    let drive = text.strip_prefix(r"\\?\").unwrap_or(&text);
    Path::new(drive).starts_with(store_dir()) || Path::new(drive).starts_with(crate::log::fm_dir())
}

/// Bytes as stored. A sealed file inside First Mate's own directories opens, so
/// `read_file` reaches a spill. Any other file keeps its bytes, so the agent
/// never sees decrypted text from a file the user owns.
pub fn open_store_bytes(path: &Path, bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    if looks_sealed(&bytes) && is_store(path) {
        return open(&bytes);
    }
    Ok(bytes)
}

/// `false` means the file already carried the envelope, or does not exist.
pub fn seal_file(path: &Path) -> Result<bool, String> {
    let Ok(bytes) = std::fs::read(path) else {
        return Ok(false);
    };
    if looks_sealed(&bytes) {
        return Ok(false);
    }
    std::fs::write(path, seal(&bytes)?).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(true)
}

/// The first run after the upgrade rewrites every plaintext store, and deletes
/// the plaintext request dumps that older runs left in `%TEMP%`. The scratch
/// directory stays out, because PowerShell must read a generated script in the
/// clear. The counts are the rewritten files and the deleted dumps.
pub fn upgrade_stores(config_dir: &Path) -> (usize, usize) {
    let mut paths: Vec<PathBuf> =
        ["settings.json", "entity_registry.json"].iter().map(|name| config_dir.join(name)).collect();
    for dir in [crate::log::fm_out_dir(), crate::conversations::conv_dir()] {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        paths.extend(entries.flatten().map(|entry| entry.path()));
    }
    let sealed = paths.iter().filter(|path| seal_file(path).unwrap_or(false)).count();
    (sealed, remove_plain_dumps())
}

/// A failed request body holds the whole conversation, so no plaintext copy may
/// survive the upgrade. A new failure writes a sealed copy in the spill
/// directory.
fn remove_plain_dumps() -> usize {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("firstmate_req_t") || !name.ends_with(".json") {
            continue;
        }
        if std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// The recovery key is 32 random bytes, shown until the user says they kept
/// it. Its derived form wraps every file key, so a lost TPM costs the user one
/// typed key instead of every transcript. SHA-256 over a domain string is
/// enough because the secret holds 256 random bits, so no stretching applies.
fn recovery_secret() -> Result<Vec<u8>, String> {
    let path = store_dir().join("recovery.bin");
    if let Ok(wrapped) = std::fs::read(&path) {
        return unwrap(&wrapped);
    }
    std::fs::create_dir_all(store_dir()).map_err(|e| e.to_string())?;
    let mut secret = [0u8; FILE_KEY];
    random(&mut secret)?;
    std::fs::write(&path, wrap(&secret)?).map_err(|e| e.to_string())?;
    Ok(secret.to_vec())
}

fn derive_recovery(secret: &[u8]) -> Vec<u8> {
    let mut hash = Sha256::new();
    hash.update(b"firstmate-recovery-v1");
    hash.update(secret);
    hash.finalize().to_vec()
}

/// `recovery.bin` holds the wrapped secret, so any run can show the key
/// again. `recovery.ack` records that the user said they kept the key, so the
/// pending state survives a crash or a close during setup.
fn ack_path() -> std::path::PathBuf {
    store_dir().join("recovery.ack")
}

pub fn recovery_key_pending() -> bool {
    store_dir().join("recovery.bin").exists() && !ack_path().exists()
}

pub fn recovery_key() -> Option<String> {
    let wrapped = std::fs::read(store_dir().join("recovery.bin")).ok()?;
    let secret = unwrap(&wrapped).ok()?;
    Some(secret.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// `true` means a pending key stopped pending, so the Settings banner leaves
/// and the setup wizard may pass the Privacy step.
pub fn ack_recovery_key() -> bool {
    recovery_key_pending() && std::fs::write(ack_path(), b"").is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Security::Cryptography::NCryptDeleteKey;

    #[test]
    fn a_sealed_file_round_trips_and_holds_no_plaintext() {
        if !tpm_ready() {
            return;
        }
        init(std::env::temp_dir().join("fm-envelope"));
        let plain = b"card 4111 1111 1111 1111";
        let sealed = seal(plain).unwrap();
        assert!(looks_sealed(&sealed));
        assert!(!sealed.windows(4).any(|window| window == b"4111"), "the card number survived");
        assert_eq!(open(&sealed).unwrap(), plain);
    }

    #[test]
    fn a_swapped_wrap_fails_the_tag() {
        if !tpm_ready() {
            return;
        }
        init(std::env::temp_dir().join("fm-envelope"));
        let mut sealed = seal(b"alpha").unwrap();
        let flip = sealed.len() - 1;
        sealed[flip] ^= 0xFF;
        assert!(open(&sealed).is_err(), "a torn ciphertext must not open");
    }

    /// A sealed file carries no key material of its own: once the TPM key is
    /// gone, nothing opens the wrap, so the file key cannot return.
    #[test]
    fn a_deleted_tpm_key_leaves_the_file_closed() {
        if !tpm_ready() {
            return;
        }
        let name = format!("FirstMateSpikeKey-{}", std::process::id());
        let key = persisted_key(&name).unwrap();
        let mut file_key = [0u8; FILE_KEY];
        random(&mut file_key).unwrap();
        let wrapped = wrap_with(key, &file_key).unwrap();
        let record = skey_seal(&file_key, b"header", b"card 4111 1111 1111 1111").unwrap();
        unsafe { NCryptDeleteKey(key, 0) }.unwrap();
        assert!(open_key(&name).is_err(), "the deleted TPM key came back");
        assert!(unwrap(&wrapped).is_err(), "another TPM key opened the wrap");
        assert!(!record.windows(4).any(|window| window == b"4111"), "the card number survived");
    }

    /// The setup gate reads this state: a secret with no marker is pending, and
    /// the acknowledge ends the pending state without destroying the key.
    #[test]
    fn an_unacknowledged_recovery_key_stays_pending() {
        if !tpm_ready() {
            return;
        }
        init(std::env::temp_dir().join("fm-envelope"));
        let dir = store_dir();
        let _ = std::fs::remove_file(dir.join("recovery.bin"));
        let _ = std::fs::remove_file(dir.join("recovery.ack"));
        recovery_secret().unwrap();
        assert!(recovery_key_pending(), "a new secret reported no pending key");
        assert_eq!(recovery_key().unwrap().len(), FILE_KEY * 2);
        assert!(ack_recovery_key(), "the acknowledge found no pending key");
        assert!(!recovery_key_pending(), "the acknowledge left the key pending");
        assert!(!ack_recovery_key(), "a second acknowledge claimed a key");
        assert_eq!(recovery_key().unwrap().len(), FILE_KEY * 2);
    }
}
