//! Microsoft Store add-on ownership and in-app purchase. An unpackaged build
//! has no Store context and simply reports no entitlement.
#![allow(unsafe_code)]

use windows::core::{Interface, HSTRING};
use windows::Services::Store::{StoreContext, StorePurchaseStatus};
use windows::Win32::Foundation::HWND;
use windows::Win32::Globalization::GetUserDefaultGeoName;
use windows::Win32::UI::Shell::IInitializeWithWindow;

/// Developer-defined Product ID of the Premium add-on in Partner Center.
pub const PREMIUM_OFFER_TOKEN: &str = "atlas_premium";
/// Store ID of the same add-on; `RequestPurchaseAsync` takes this, not the token.
pub const PREMIUM_STORE_ID: &str = "9PJF82XG91GZ";

/// Blocking: call from a worker thread, never from the UI thread.
pub fn owns_premium() -> bool {
    check().unwrap_or(false)
}

fn check() -> windows::core::Result<bool> {
    let license = StoreContext::GetDefault()?.GetAppLicenseAsync()?.get()?;
    for entry in license.AddOnLicenses()? {
        let add_on = entry.Value()?;
        if add_on.IsActive()? && add_on.InAppOfferToken()? == PREMIUM_OFFER_TOKEN {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Result of the Store purchase dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Purchase {
    /// Bought now or already owned: the entitlement can be refreshed.
    Owned,
    /// The user closed the dialog without buying.
    Cancelled,
    Failed(String),
}

/// Shows the Store's own purchase dialog for the Premium add-on, owned by
/// `owner` (a desktop app must bind the dialog to its window). Blocking: call
/// from a worker thread. Only works in the packaged (Store) build.
pub fn purchase_premium(owner: isize) -> Purchase {
    match request_purchase(owner) {
        Ok(StorePurchaseStatus::Succeeded | StorePurchaseStatus::AlreadyPurchased) => {
            Purchase::Owned
        }
        Ok(StorePurchaseStatus::NotPurchased) => Purchase::Cancelled,
        Ok(other) => Purchase::Failed(format!("{other:?}")),
        Err(error) => Purchase::Failed(error.message()),
    }
}

fn request_purchase(owner: isize) -> windows::core::Result<StorePurchaseStatus> {
    let context = StoreContext::GetDefault()?;
    // SAFETY: `owner` is Atlas's live top-level window handle.
    unsafe {
        context
            .cast::<IInitializeWithWindow>()?
            .Initialize(HWND(owner as *mut core::ffi::c_void))?;
    }
    let result = context
        .RequestPurchaseAsync(&HSTRING::from(PREMIUM_STORE_ID))?
        .get()?;
    let status = result.Status()?;
    if matches!(
        status,
        StorePurchaseStatus::NetworkError | StorePurchaseStatus::ServerError
    ) {
        let code = result.ExtendedError()?;
        if code.is_err() {
            return Err(code.into());
        }
    }
    Ok(status)
}

/// ISO 3166-1 alpha-2 code of the user's Windows home region (Settings ›
/// Time & language › Region), which is also what the Store uses for its market.
pub fn home_region() -> Option<String> {
    let mut buf = [0u16; 16];
    // SAFETY: writes at most `buf.len()` UTF-16 units, NUL included.
    let len = unsafe { GetUserDefaultGeoName(&mut buf) };
    let len = usize::try_from(len).ok()?.checked_sub(1)?;
    let code = String::from_utf16_lossy(buf.get(..len)?);
    (code.len() == 2).then(|| code.to_ascii_uppercase())
}
