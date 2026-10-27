//! Work off the caller's thread. A page (the browser build) has only the one, so there the work
//! runs at once, on the caller's, and is done when [`spawn`] returns: the app waits for it.

/// Runs `f` on a new thread named `name` (`Err`: none could be started); in a page, runs it
/// before returning.
pub fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = name;
        f();
        Ok(())
    }
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .map(drop)
}
