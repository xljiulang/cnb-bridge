//! Opens URLs in the user's default browser.

use std::process::Command;

/// Opens `url` with the operating system's default browser.
pub fn open_url(url: &str) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        Command::new("cmd").args(["/C", "start", "", url]).spawn()?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(url).spawn()?;
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open").arg(url).spawn()?;
        Ok(())
    }
}
