// TODO(Lane I): remove pairing surface; superseded by API-key provisioning.
//
// Device pairing (NIP-AB) existed solely to transfer a private key between
// devices. Under the API-key auth model there is no per-device private key to
// move — "add a device" becomes "issue a new key" (`buzz-admin issue-key`,
// Lane J). Lane K deleted `buzz_core::pairing`, `KIND_PAIRING`, and the
// `buzz-pair-relay` / `buzz-pairing-cli` crates from the workspace. This file
// is a temporary compile stub that keeps the desktop Tauri command surface and
// its registrations building until Lane I removes the pairing UX (frontend +
// these commands) entirely.

use tauri::{AppHandle, State};

use crate::app_state::AppState;

/// Stub error returned by every pairing command until Lane I removes the
/// pairing surface entirely.
const PAIRING_REMOVED: &str =
    "device pairing is removed; use API keys (buzz-admin issue-key) — see Lane I";

/// Managed Tauri state placeholder for the removed pairing session.
///
// TODO(Lane I): delete this type and its `.manage(...)` registration in
// `lib.rs` once the pairing frontend is gone.
#[derive(Default)]
pub struct PairingHandle;

impl PairingHandle {
    /// Construct the placeholder handle.
    pub fn new() -> Self {
        Self
    }
}

/// Stub for the removed NIP-AB pairing start command.
// TODO(Lane I): remove pairing surface; superseded by API-key provisioning.
#[tauri::command]
pub async fn start_pairing(
    _app: AppHandle,
    _state: State<'_, AppState>,
    _pairing: State<'_, PairingHandle>,
) -> Result<String, String> {
    Err(PAIRING_REMOVED.to_string())
}

/// Stub for the removed NIP-AB SAS-confirmation command.
// TODO(Lane I): remove pairing surface; superseded by API-key provisioning.
#[tauri::command]
pub async fn confirm_pairing_sas(_pairing: State<'_, PairingHandle>) -> Result<(), String> {
    Err(PAIRING_REMOVED.to_string())
}

/// Stub for the removed NIP-AB pairing cancel command.
// TODO(Lane I): remove pairing surface; superseded by API-key provisioning.
#[tauri::command]
pub async fn cancel_pairing(_pairing: State<'_, PairingHandle>) -> Result<(), String> {
    Err(PAIRING_REMOVED.to_string())
}
