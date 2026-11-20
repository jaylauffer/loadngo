//! One app running as the guest of another, in that app's window.
//!
//! A phone lets an app start no other app, so there the launcher holds its
//! games inside itself: it runs a game's own loop in its window and has the
//! list back on the screen when that loop returns. Two things differ for the
//! game, and it asks for both here rather than working them out:
//!
//! - [`offers_quit`]: alone on a phone a game offers no Quit (the platform
//!   closes apps); as a guest it must, because leaving is the only way back.
//! - [`assets_root`]: its files are not where they are when it is installed
//!   alone, since several guests share one bundle.

use std::path::PathBuf;
use std::sync::Mutex;

/// The key of the guest now running, if one is.
static GUEST: Mutex<Option<String>> = Mutex::new(None);

fn guest() -> Option<String> {
    GUEST
        .lock()
        .unwrap_or_else(|held| held.into_inner())
        .clone()
}

/// Says the loop about to run is another app's guest (`Some(key)`), or that
/// the guest has come back (`None`). The app that owns the window says so
/// around each guest it runs. `key` names the guest's folder of assets (see
/// [`assets_root`]); a game's repository name does.
pub fn set_guest(key: Option<&str>) {
    *GUEST.lock().unwrap_or_else(|held| held.into_inner()) = key.map(str::to_string);
}

/// Whether the loop now running is another app's guest (see [`set_guest`]).
#[must_use]
pub fn is_guest() -> bool {
    guest().is_some()
}

/// Whether an app should offer a way to leave it. On a computer it always
/// should: Quit closes it. A phone or tablet closes apps itself, so alone an
/// app offers none there; as a guest it does, and leaving goes back to the
/// app that owns the window.
#[must_use]
pub fn offers_quit() -> bool {
    offers_quit_where(
        cfg!(any(target_os = "android", target_os = "ios")),
        is_guest(),
    )
}

fn offers_quit_where(platform_closes_apps: bool, guest: bool) -> bool {
    guest || !platform_closes_apps
}

/// The folder the running app's own asset files are in, where the platform
/// keeps them as files beside the app: on a phone, where the host sets
/// `SNG_ASSETS_ROOT` to the app bundle's `assets`. `None` where it does not
/// (a computer), and an app there carries its assets or finds them itself.
///
/// A guest's are in `games/<key>` inside its owner's, laid out as its own
/// `assets` folder is. Read this, not the variable, so the same game finds
/// its files alone and as a guest.
#[must_use]
pub fn assets_root() -> Option<PathBuf> {
    assets_root_of(
        std::env::var_os("SNG_ASSETS_ROOT").map(PathBuf::from),
        guest().as_deref(),
    )
}

fn assets_root_of(owner: Option<PathBuf>, guest: Option<&str>) -> Option<PathBuf> {
    let owner = owner?;
    Some(match guest {
        Some(key) => owner.join("games").join(key),
        None => owner,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_phone_app_on_its_own_offers_no_way_out() {
        let (phone, computer, guest, alone) = (true, false, true, false);
        assert!(offers_quit_where(computer, alone));
        assert!(offers_quit_where(computer, guest));
        assert!(!offers_quit_where(phone, alone), "the platform closes it");
        assert!(offers_quit_where(phone, guest), "the only way back");
    }

    #[test]
    fn an_app_is_a_guest_from_when_its_owner_says_so_until_it_says_otherwise() {
        // The one test that touches the flag, so it has it to itself.
        assert!(!is_guest());
        set_guest(Some("tiles"));
        assert!(is_guest());
        assert_eq!(guest().as_deref(), Some("tiles"));
        assert!(offers_quit());
        set_guest(None);
        assert!(!is_guest());
    }

    #[test]
    fn a_guest_finds_its_files_in_its_own_folder_of_the_owners_assets() {
        let bundle = PathBuf::from("/app/assets");
        assert_eq!(
            assets_root_of(Some(bundle.clone()), None),
            Some(bundle.clone()),
            "alone, the app's assets are its own"
        );
        assert_eq!(
            assets_root_of(Some(bundle), Some("tiles")),
            Some(PathBuf::from("/app/assets/games/tiles"))
        );
        // A computer keeps no such folder, guest or not.
        assert_eq!(assets_root_of(None, None), None);
        assert_eq!(assets_root_of(None, Some("tiles")), None);
    }
}
