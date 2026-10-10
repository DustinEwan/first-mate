# Encryption at Rest

`crypto.rs` implements this, and `README.md` states the user-visible behavior.
This document holds the mechanism and the measurement behind it.

## The envelope

- First Mate encrypts every file it writes. Encryption is not a setting, and no
  store stays plaintext.
- AES-256-GCM encrypts each file, and the header `FMENC1` names the format.
- Each file carries its own key. The envelope stores that key twice: one wrap
  under the Platform Crypto Provider key, which the TPM holds, and one wrap under
  the recovery key, which the user keeps offline.
- The Platform Crypto Provider exposes itself as
  `MS_PLATFORM_KEY_STORAGE_PROVIDER`. `NCryptEncrypt` wraps each file key with
  RSA-OAEP and SHA-256, and `NCryptDecrypt` unwraps it. `NCryptExportKey` cannot
  wrap a key, because the Microsoft providers reject the `hExportKey` parameter
  with `NTE_INVALID_HANDLE`.
- The app unwraps that key at start with no prompt. The app must read its own
  settings, registry, and transcripts at logon, so no prompt may block a read.
  Every `NCrypt` call passes `NCRYPT_SILENT_FLAG`.
- No key material is ever on disk in the clear.
- The sealed stores are each `.chat` transcript under
  `~/.firstmate/conversations`, `settings.json` and `entity_registry.json` under
  `%APPDATA%\com.dusti.firstmate`, each `fm_out` exec spill under
  `~/.firstmate/output`, `firstmate.log`, and each request dump.
- `entity_registry.json` holds raw sensitive values by design, so it needs the
  same envelope as a transcript.
- Windows Hello does not gate this key. Hello gates display, and roadmap §4
  defines that policy.
- The first run after an upgrade rewrites every plaintext file inside the
  envelope, so no store stays readable after that run. `remove_plain_dumps`
  deletes the plaintext request dumps an older run left in the temp directory.

## The recovery key

- A TPM reset or hardware loss makes the data unrecoverable, so each file key
  carries a second wrap under a key derived from a 32-byte random secret. SHA-256
  over a domain string derives that key, because the secret holds 256 random
  bits, so no stretching applies.
- The setup wizard shows the secret as hex, and keeps it on screen until the user
  chooses **I saved it**. The app never shows the key again after that choice.
- `%APPDATA%\com.dusti.firstmate\recovery.ack` records the choice, so a run that
  ends before the acknowledge opens the wizard on the Privacy step again.
- `recovery.bin` holds the wrapped secret, so any run can show the key text
  before the acknowledge.

## Spike result (2026-10-10)

The provider works, and no call showed a prompt. `PCP_PLATFORM_TYPE` returns
`TPM-Version:2.0`, vendor `AMD`, firmware 196700.5. `NCryptFinalizeKey` created a
persisted RSA-2048 key with `NCRYPT_SILENT_FLAG`. `NCryptEncrypt` wrapped a
32-byte file key with OAEP and SHA-256. A fresh process unwrapped that key with
`NCryptDecrypt` in 72 ms. AES-256-GCM in `BCrypt` then opened the file with that
key, and the auth tag passed. The key reports `Export Policy` `0`, and
`NCryptExportKey` refuses `RSAPRIVATEBLOB` with `NTE_BAD_TYPE`, so the key cannot
leave the TPM. The store keeps the key at
`%LOCALAPPDATA%\Microsoft\Crypto\PCPKSP\<sid hash>\*.PCPKEY`. `icacls` grants
that folder to the user, `BUILTIN\Administrators`, and `SYSTEM`, so a second
standard account cannot read the blob. `crypto.rs` now writes the envelope. The
live `settings.json` starts with `FMENC1`, and no byte of the stored API key
appears in it.

## Tests

`cargo test` covers the envelope. A sealed file round-trips and holds no
plaintext byte. A torn ciphertext fails the auth tag. A test deletes a second PCP
key with `NCryptDeleteKey`, and that key's wrap then refuses to open, so no key
material travels with a file. One test asserts the recovery states: a secret with
no marker is pending, and the acknowledge ends the pending state without
destroying the key.
