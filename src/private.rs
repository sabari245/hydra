//! Files and directories only this user can read. They hold API keys and
//! transcripts. On Windows, files under the user's profile already are.

use std::{
    fs::{self, OpenOptions},
    io,
    path::Path,
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

/// Creates `path` and its parents, readable only by this user.
pub fn create_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Open options that create files readable only by this user.
pub fn options() -> OpenOptions {
    #[cfg_attr(not(unix), expect(unused_mut))]
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    options.mode(0o600);
    options
}

/// Makes an existing file readable only by this user.
pub fn restrict(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Whether the file can be read by users other than its owner.
pub fn is_shared(path: &Path) -> bool {
    #[cfg(unix)]
    return fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o077 != 0)
        .unwrap_or(false);
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}
