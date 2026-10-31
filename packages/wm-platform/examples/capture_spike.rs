//! Spike for workspace transitions (docs/hyprland-parity.md, phase 3.1):
//! can Windows.Graphics.Capture take a picture of a window that's cloaked
//! (on a hidden workspace), or that's fully transparent (layered alpha 0)?
//!
//! Usage: `capture_spike <mode> <hwnd>...`, where `mode` is:
//!
//! - `capture`: captures each window as it is and prints what came back.
//! - `transparent`: makes each window layered with alpha 0, captures it,
//!   and leaves it transparent (for a screenshot to show whether it's
//!   really invisible). `opaque` restores it.
//! - `opaque`: gives each window alpha 255 back.
//!
//! Prints one line per window: whether it's cloaked, whether a frame came,
//! how long it took, and how much of the frame isn't blank.

#[cfg(target_os = "windows")]
fn main() {
  let args = std::env::args().collect::<Vec<_>>();

  let Some(mode) = args.get(1) else {
    eprintln!(
      "Usage: capture_spike <capture|transparent|opaque> <hwnd>..."
    );
    std::process::exit(2);
  };

  let handles = args[2..]
    .iter()
    .filter_map(|arg| arg.parse::<isize>().ok())
    .collect::<Vec<_>>();

  if let Err(err) = spike::run(mode, &handles) {
    eprintln!("Spike failed: {err}");
    std::process::exit(1);
  }
}

#[cfg(not(target_os = "windows"))]
fn main() {
  eprintln!("The capture spike is Windows only.");
}

#[cfg(target_os = "windows")]
mod spike {
  use std::time::{Duration, Instant};

  use windows::{
    core::ComInterface,
    Foundation::TypedEventHandler,
    Graphics::{
      Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem},
      DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
    },
    Win32::{
      Foundation::{BOOL, COLORREF, HWND},
      Graphics::{
        Direct3D::D3D_DRIVER_TYPE_HARDWARE,
        Direct3D11::{
          D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
          ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
          D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE,
          D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
          D3D11_USAGE_STAGING,
        },
        Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED},
        Dxgi::IDXGIDevice,
      },
      System::{
        Com::{CoInitializeEx, COINIT_MULTITHREADED},
        WinRT::{
          Direct3D11::{
            CreateDirect3D11DeviceFromDXGIDevice,
            IDirect3DDxgiInterfaceAccess,
          },
          Graphics::Capture::IGraphicsCaptureItemInterop,
        },
      },
      UI::WindowsAndMessaging::{
        GetClassNameW, GetWindowLongPtrW, SetLayeredWindowAttributes,
        SetWindowLongPtrW, GWL_EXSTYLE, LWA_ALPHA, WS_EX_LAYERED,
      },
    },
  };

  type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

  /// Runs the spike in `mode` on each window.
  pub fn run(mode: &str, handles: &[isize]) -> Result<()> {
    // SAFETY: Initializes COM for this thread, once, before any COM call.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED)? };

    let (device, context) = d3d_device()?;

    for &handle in handles {
      let hwnd = HWND(handle);

      match mode {
        "capture" => {}
        "transparent" => set_alpha(hwnd, 0)?,
        "opaque" => {
          set_alpha(hwnd, 255)?;
          println!("{handle} opaque again");
          continue;
        }
        _ => return Err(format!("Unknown mode '{mode}'.").into()),
      }

      let started = Instant::now();
      let result = capture(hwnd, &device, &context);
      let elapsed = started.elapsed().as_secs_f64() * 1000.0;

      let outcome = match result {
        Ok(filled) => format!(
          "frame in {elapsed:.1} ms, {:.0}% not blank",
          filled * 100.0
        ),
        Err(err) => format!("no frame after {elapsed:.1} ms: {err}"),
      };

      println!(
        "{handle} class={} cloaked={} mode={mode}: {outcome}",
        class_name(hwnd),
        is_cloaked(hwnd),
      );
    }

    Ok(())
  }

  /// Creates a D3D11 device to capture with and read pixels back.
  fn d3d_device() -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    let mut device = None;
    let mut context = None;

    // SAFETY: The out pointers point at locals that outlive the call.
    unsafe {
      D3D11CreateDevice(
        None,
        D3D_DRIVER_TYPE_HARDWARE,
        None,
        D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        None,
        D3D11_SDK_VERSION,
        Some(&raw mut device),
        None,
        Some(&raw mut context),
      )?;
    }

    Ok((
      device.ok_or("No D3D11 device.")?,
      context.ok_or("No D3D11 context.")?,
    ))
  }

  /// Captures one frame of the window with Windows.Graphics.Capture.
  ///
  /// Returns the share of its pixels that aren't blank (fully transparent
  /// or black).
  fn capture(
    hwnd: HWND,
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
  ) -> Result<f64> {
    let interop = windows::core::factory::<
      GraphicsCaptureItem,
      IGraphicsCaptureItemInterop,
    >()?;

    // SAFETY: `hwnd` is a window handle from the command line; an invalid
    // one makes the call fail.
    let item: GraphicsCaptureItem =
      unsafe { interop.CreateForWindow(hwnd)? };

    let dxgi: IDXGIDevice = device.cast()?;
    // SAFETY: `dxgi` is a valid DXGI device.
    let winrt_device: IDirect3DDevice =
      unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? }.cast()?;

    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
      &winrt_device,
      DirectXPixelFormat::B8G8R8A8UIntNormalized,
      1,
      item.Size()?,
    )?;

    let session = pool.CreateCaptureSession(&item)?;
    let _ = session.SetIsBorderRequired(false);

    let (tx, rx) = std::sync::mpsc::sync_channel::<()>(1);
    pool.FrameArrived(&TypedEventHandler::new(move |_, _| {
      let _ = tx.try_send(());
      Ok(())
    }))?;

    session.StartCapture()?;

    let arrived = rx.recv_timeout(Duration::from_secs(1)).is_ok();
    let frame = if arrived {
      Some(pool.TryGetNextFrame()?)
    } else {
      None
    };

    let filled = match &frame {
      Some(frame) => {
        let access: IDirect3DDxgiInterfaceAccess =
          frame.Surface()?.cast()?;
        // SAFETY: The surface of a frame from a D3D11 frame pool is a
        // D3D11 texture.
        let texture: ID3D11Texture2D = unsafe { access.GetInterface()? };
        Some(not_blank_share(device, context, &texture)?)
      }
      None => None,
    };

    if let Some(frame) = frame {
      frame.Close()?;
    }

    session.Close()?;
    pool.Close()?;

    filled.ok_or_else(|| "no frame within 1 s".into())
  }

  /// Reads a texture back and returns the share of pixels that aren't
  /// fully transparent or black.
  fn not_blank_share(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    texture: &ID3D11Texture2D,
  ) -> Result<f64> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: `desc` is a valid, writable descriptor.
    unsafe { texture.GetDesc(&raw mut desc) };

    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
    desc.MiscFlags = 0;

    let mut staging = None;
    // SAFETY: `desc` describes a staging copy of `texture`; the out
    // pointer points at a local.
    unsafe {
      device.CreateTexture2D(
        &raw const desc,
        None,
        Some(&raw mut staging),
      )?;
    }
    let staging: ID3D11Texture2D = staging.ok_or("No staging texture.")?;

    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();

    // SAFETY: Both textures have the same size and format; the mapped
    // memory is read only within its row pitch and the texture's size,
    // before `Unmap`.
    unsafe {
      context.CopyResource(&staging, texture);
      context.Map(
        &staging,
        0,
        D3D11_MAP_READ,
        0,
        Some(&raw mut mapped),
      )?;

      let mut filled = 0_u64;
      let mut total = 0_u64;

      for row in (0..desc.Height).step_by(4) {
        let line = mapped
          .pData
          .cast::<u8>()
          .add(row as usize * mapped.RowPitch as usize);

        for column in (0..desc.Width).step_by(4) {
          let pixel =
            std::slice::from_raw_parts(line.add(column as usize * 4), 4);
          total += 1;

          if pixel[3] != 0 && (pixel[0] | pixel[1] | pixel[2]) != 0 {
            filled += 1;
          }
        }
      }

      context.Unmap(&staging, 0);

      Ok(if total == 0 {
        0.0
      } else {
        filled as f64 / total as f64
      })
    }
  }

  /// Makes the window layered with the given alpha.
  fn set_alpha(hwnd: HWND, alpha: u8) -> Result<()> {
    // SAFETY: Reads and sets the window's extended style, and its layered
    // alpha, which apply to any top-level window.
    unsafe {
      let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
      SetWindowLongPtrW(
        hwnd,
        GWL_EXSTYLE,
        style | WS_EX_LAYERED.0 as isize,
      );
      SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA)?;
    }

    Ok(())
  }

  /// Whether DWM has the window cloaked.
  fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked = BOOL(0);

    // SAFETY: `cloaked` is a writable `BOOL`, the size the attribute
    // takes.
    unsafe {
      DwmGetWindowAttribute(
        hwnd,
        DWMWA_CLOAKED,
        (&raw mut cloaked).cast(),
        std::mem::size_of::<BOOL>() as u32,
      )
    }
    .is_ok_and(|()| cloaked.0 != 0)
  }

  /// The window's class name.
  fn class_name(hwnd: HWND) -> String {
    let mut buffer = [0_u16; 128];
    // SAFETY: Writes at most the buffer's length.
    let length = unsafe { GetClassNameW(hwnd, &mut buffer) };
    String::from_utf16_lossy(
      &buffer[..usize::try_from(length).unwrap_or(0)],
    )
  }
}
