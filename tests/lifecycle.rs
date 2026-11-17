use cnb_bridge::lifecycle::{is_autostart, is_shutdown_for_update, APP_MUTEX_NAME, AUTOSTART_ARG};

#[test]
fn recognizes_only_the_autostart_argument() {
    assert!(is_autostart(["cnb-bridge.exe", AUTOSTART_ARG]));
    assert!(!is_autostart(["cnb-bridge.exe", "--autostart-extra"]));
    assert!(!is_autostart(["cnb-bridge.exe"]));
}

#[test]
fn recognizes_only_the_installer_shutdown_argument() {
    assert!(is_shutdown_for_update([
        "cnb-bridge.exe",
        "--shutdown-for-update"
    ]));
    assert!(!is_shutdown_for_update([
        "cnb-bridge.exe",
        "--shutdown-for-update-extra"
    ]));
    assert!(!is_shutdown_for_update(["cnb-bridge.exe"]));
}

#[test]
fn uses_the_stable_installer_mutex_name() {
    assert_eq!(APP_MUTEX_NAME, r"Local\cool.cnb.bridge.desktop");
}
