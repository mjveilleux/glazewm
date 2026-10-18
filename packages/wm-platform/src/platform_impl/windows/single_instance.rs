use windows::{
  core::{HSTRING, PWSTR},
  Win32::{
    Foundation::{
      CloseHandle, GetLastError, LocalFree, ERROR_ALREADY_EXISTS,
      ERROR_FILE_NOT_FOUND, HANDLE, HLOCAL,
    },
    Security::{
      Authorization::ConvertSidToStringSidW, GetTokenInformation,
      TokenUser, TOKEN_QUERY, TOKEN_USER,
    },
    System::Threading::{
      CreateMutexW, GetCurrentProcess, OpenMutexW, OpenProcessToken,
      ReleaseMutex, SYNCHRONIZATION_ACCESS_RIGHTS,
    },
  },
};

/// Arbitrary GUID to uniquely identify the application.
const APP_GUID: &str = "Global\\325d0ed7-7f60-4925-8d1b-aa287b26b218";

/// The mutex's name: the GUID and the user's SID, so it's one instance per
/// Windows account. Several accounts can be signed in at once, and a
/// `Global\` mutex made by one denies the others (`Access is denied`).
/// Falls back to the GUID alone if the SID can't be read.
fn mutex_name() -> HSTRING {
  match user_sid() {
    Some(sid) => HSTRING::from(format!("{APP_GUID}-{sid}")),
    None => HSTRING::from(APP_GUID),
  }
}

/// The current process's user SID as a string (`S-1-5-21-...`).
fn user_sid() -> Option<String> {
  unsafe {
    let mut token = HANDLE::default();
    OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token)
      .ok()?;

    // Sized by a first call; `u64`s so the buffer is aligned for
    // `TOKEN_USER`.
    let mut len = 0u32;
    let _ = GetTokenInformation(token, TokenUser, None, 0, &raw mut len);
    let mut buffer = vec![0u64; (len as usize).div_ceil(8)];
    let res = GetTokenInformation(
      token,
      TokenUser,
      Some(buffer.as_mut_ptr().cast()),
      len,
      &raw mut len,
    );
    let _ = CloseHandle(token);
    res.ok()?;

    let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
    let mut sid = PWSTR::null();
    ConvertSidToStringSidW(user.User.Sid, &raw mut sid).ok()?;
    let sid_string = sid.to_string().ok();
    let _ = LocalFree(HLOCAL(sid.0.cast()));
    sid_string
  }
}

/// Platform-specific implementation of [`SingleInstance`].
pub struct SingleInstance {
  handle: HANDLE,
}

impl SingleInstance {
  /// Implements [`SingleInstance::new`].
  pub(crate) fn new() -> crate::Result<Self> {
    // Create a named system-wide mutex.
    let handle = unsafe { CreateMutexW(None, true, &mutex_name()) }?;

    if let Err(err) = unsafe { GetLastError() } {
      if err == ERROR_ALREADY_EXISTS.into() {
        return Err(crate::Error::Platform(
          "Another instance of the application is already running."
            .to_string(),
        ));
      }
    }

    Ok(Self { handle })
  }

  /// Implements [`SingleInstance::is_running`].
  #[must_use]
  pub(crate) fn is_running() -> bool {
    let res = unsafe {
      OpenMutexW(
        SYNCHRONIZATION_ACCESS_RIGHTS::default(),
        false,
        &mutex_name(),
      )
    };

    // If the mutex exists, then another instance is running.
    match res {
      Ok(_) => false,
      Err(err) => err == ERROR_FILE_NOT_FOUND.into(),
    }
  }
}

impl Drop for SingleInstance {
  fn drop(&mut self) {
    unsafe {
      let _ = ReleaseMutex(self.handle);
      let _ = CloseHandle(self.handle);
    }
  }
}
