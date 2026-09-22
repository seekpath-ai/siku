/// Suppress the console-window flash when a GUI app spawns console-subsystem
/// subprocesses on Windows (CREATE_NO_WINDOW). No-op on other platforms.
///
/// Apply to every short-lived console tool spawn (git probes, dependency
/// checks); GUI launches (explorer, screencapture) don't need it.
pub fn no_window(cmd: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    cmd
}
