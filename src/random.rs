//! OS randomness for file and commit ids (replaces the `getrandom` crate).
//!
//! Unix reads `/dev/urandom`; Windows uses `BCryptGenRandom` via raw FFI.
//! Any other target fails at compile time rather than degrading to weak
//! randomness: ids must be unique, and time/pid stew is not.

pub fn fill(buf: &mut [u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Read as _;
        std::fs::File::open("/dev/urandom")?.read_exact(buf)
    }

    #[cfg(windows)]
    {
        #[link(name = "bcryptprimitives")]
        unsafe extern "system" {
            fn BCryptGenRandom(
                halgorithm: *mut u8,
                pbbuffer: *mut u8,
                cbbuffer: u32,
                dwflags: u32,
            ) -> i32;
        }
        // NULL algorithm + USE_SYSTEM_PREFERRED_RNG.
        const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x00000002;
        let mut offset = 0;
        while offset < buf.len() {
            let n = (buf.len() - offset).min(u32::MAX as usize) as u32;
            // SAFETY: null algorithm handle with the system-preferred flag
            // only fills the buffer; `n` never exceeds the spare capacity.
            let status = unsafe {
                BCryptGenRandom(
                    std::ptr::null_mut(),
                    buf[offset..].as_mut_ptr(),
                    n,
                    BCRYPT_USE_SYSTEM_PREFERRED_RNG,
                )
            };
            if status != 0 {
                return Err(std::io::Error::other(format!(
                    "BCryptGenRandom failed: {status:#x}"
                )));
            }
            offset += n as usize;
        }
        Ok(())
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = buf;
        compile_error!(
            "xitdb needs OS randomness (file/commit ids) on this target; \
             add a backend in src/random.rs"
        )
    }
}
