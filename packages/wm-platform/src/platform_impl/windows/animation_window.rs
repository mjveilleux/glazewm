use std::sync::Once;

use windows::{
  core::{w, ComInterface, PCWSTR},
  Foundation::TypedEventHandler,
  Graphics::{
    Capture::{
      Direct3D11CaptureFrame, Direct3D11CaptureFramePool,
      GraphicsCaptureAccess, GraphicsCaptureAccessKind,
      GraphicsCaptureItem, GraphicsCaptureSession,
    },
    DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
  },
  Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
    Graphics::{
      Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0},
      Direct3D11::{
        D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
        ID3D11Texture2D, D3D11_BOX, D3D11_CPU_ACCESS_READ,
        D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE,
        D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
        D3D11_USAGE_STAGING,
      },
      DirectComposition::{
        DCompositionCreateDevice2, IDCompositionAnimation,
        IDCompositionDesktopDevice, IDCompositionRectangleClip,
        IDCompositionScaleTransform, IDCompositionSurface,
        IDCompositionTarget, IDCompositionVisual, IDCompositionVisual3,
        DCOMPOSITION_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
        DCOMPOSITION_FRAME_STATISTICS,
      },
      Dxgi::{
        Common::{
          DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM,
          DXGI_SAMPLE_DESC,
        },
        IDXGIDevice,
      },
    },
    System::WinRT::{
      Direct3D11::{
        CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
      },
      Graphics::Capture::IGraphicsCaptureItemInterop,
    },
    UI::WindowsAndMessaging::{
      CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW,
      SetWindowPos, HTTRANSPARENT, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
      SWP_SHOWWINDOW, WM_NCHITTEST, WNDCLASSW, WS_EX_NOACTIVATE,
      WS_EX_NOREDIRECTIONBITMAP, WS_EX_TRANSPARENT, WS_POPUP,
    },
  },
};

use crate::{
  platform_impl::com::COM_INIT, AnimatedValue, AnimationCurve, Dispatcher,
  NativeWindow, OpacityValue, OverlayAnimation, Rect, ThreadBound,
};

struct Direct3DDevice(IDirect3DDevice);
unsafe impl Send for Direct3DDevice {}

struct GraphicsCaptureItemInterop(IGraphicsCaptureItemInterop);
unsafe impl Send for GraphicsCaptureItemInterop {}

/// Platform-specific implementation of [`AnimationContext`].
pub(crate) struct AnimationContext {
  _d3d_device: ID3D11Device,
  d3d_context: ThreadBound<ID3D11DeviceContext>,
  dcomp_device: ThreadBound<IDCompositionDesktopDevice>,
  winrt_device: Direct3DDevice,
  capture_interop: GraphicsCaptureItemInterop,
}

impl AnimationContext {
  /// Implements [`AnimationContext::new`].
  pub(crate) fn new(dispatcher: &Dispatcher) -> crate::Result<Self> {
    Self::request_borderless_capture();

    dispatcher.dispatch_sync(|| {
      COM_INIT.with(|_| {
        if !GraphicsCaptureSession::IsSupported()? {
          return Err(crate::Error::Platform(
            "Windows.Graphics.Capture isn't supported on this system."
              .to_string(),
          ));
        }

        let (d3d_device, d3d_context) = Self::create_d3d11_device()?;
        let dxgi_device: IDXGIDevice = d3d_device.cast()?;
        let dcomp_device: IDCompositionDesktopDevice =
          unsafe { DCompositionCreateDevice2(&dxgi_device)? };

        let inspectable =
          unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device)? };
        let winrt_device: IDirect3DDevice = inspectable.cast()?;

        let capture_interop = windows::core::factory::<
          GraphicsCaptureItem,
          IGraphicsCaptureItemInterop,
        >()?;

        Ok(Self {
          _d3d_device: d3d_device,
          d3d_context: ThreadBound::new(d3d_context, dispatcher.clone()),
          dcomp_device: ThreadBound::new(dcomp_device, dispatcher.clone()),
          winrt_device: Direct3DDevice(winrt_device),
          capture_interop: GraphicsCaptureItemInterop(capture_interop),
        })
      })
    })?
  }

  /// Implements [`AnimationContext::transaction`].
  pub(crate) fn transaction<F, R>(
    &self,
    update_fn: F,
    dispatcher: &Dispatcher,
  ) -> crate::Result<R>
  where
    F: FnOnce() -> R + Send,
    R: Send,
  {
    dispatcher.dispatch_sync(|| {
      COM_INIT.with(|_| {
        let result = update_fn();
        unsafe {
          self
            .dcomp_device
            .with(|dcomp_device| dcomp_device.Commit())??;
        }
        Ok(result)
      })
    })?
  }

  /// Implements [`AnimationContext::next_frame_time`].
  pub(crate) fn next_frame_time(
    &self,
    dispatcher: &Dispatcher,
  ) -> crate::Result<std::time::Instant> {
    dispatcher.dispatch_sync(|| {
      let mut stats = DCOMPOSITION_FRAME_STATISTICS::default();

      // SAFETY: `stats` is a valid, writable
      // `DCOMPOSITION_FRAME_STATISTICS` for the call to fill in.
      self.dcomp_device.with(|dcomp_device| unsafe {
        dcomp_device.GetFrameStatistics(&raw mut stats)
      })??;

      let now = std::time::Instant::now();

      if stats.timeFrequency <= 0 {
        return Ok(now);
      }

      // LINT: QPC tick counts within a frame or two of now; exact enough
      // as `f64`.
      #[allow(clippy::cast_precision_loss)]
      let until_next = (stats.nextEstimatedFrameTime - stats.currentTime)
        .max(0) as f64
        / stats.timeFrequency as f64;

      Ok(now + std::time::Duration::from_secs_f64(until_next))
    })?
  }

  /// Asks Windows to capture windows without the yellow border it
  /// otherwise draws around a captured window, which would flash on every
  /// window as its animation starts.
  ///
  /// Windows grants this to desktop apps when asked. It's only asked once
  /// per process, off the event loop thread since it waits for the answer.
  /// Windows versions without the request draw no border to begin with.
  fn request_borderless_capture() {
    static REQUESTED: Once = Once::new();

    // On a thread of its own, so its COM state is torn down when the
    // thread ends. Kept on the calling (main) thread, it was only
    // released as the process exited, from a TLS destructor, which
    // stalled the exit.
    REQUESTED.call_once(|| {
      let request = std::thread::spawn(|| {
        COM_INIT.with(
          |_| match GraphicsCaptureAccess::RequestAccessAsync(
            GraphicsCaptureAccessKind::Borderless,
          )
          .and_then(|operation| operation.get())
          {
            Ok(status) => {
              tracing::info!("Borderless capture access: {}.", status.0);
            }
            Err(err) => {
              tracing::info!("Borderless capture not available: {err}");
            }
          },
        );
      });

      if request.join().is_err() {
        tracing::warn!("Borderless capture request panicked.");
      }
    });
  }

  /// Creates a D3D11 device.
  fn create_d3d11_device(
  ) -> crate::Result<(ID3D11Device, ID3D11DeviceContext)> {
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;

    unsafe {
      D3D11CreateDevice(
        None,
        D3D_DRIVER_TYPE_HARDWARE,
        None,
        // BGRA support is required by DirectComposition.
        D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        Some(&[D3D_FEATURE_LEVEL_11_0]),
        D3D11_SDK_VERSION,
        Some(&raw mut device),
        None,
        Some(&raw mut context),
      )?;
    }

    let device = device.ok_or_else(|| {
      crate::Error::Platform(
        "D3D11CreateDevice returned null device.".to_string(),
      )
    })?;

    let context = context.ok_or_else(|| {
      crate::Error::Platform(
        "D3D11CreateDevice returned null context.".to_string(),
      )
    })?;

    Ok((device, context))
  }
}

unsafe impl Sync for AnimationContext {}

/// Side of the square surface that fills an overlay where the snapshot
/// doesn't reach. It's scaled to the window's size each frame.
const FILL_SIZE: u32 = 16;

/// Platform-specific implementation of [`AnimationWindow`].
///
/// The snapshot is never stretched: it's drawn at its own size and cut to
/// the window's current rect, like a window being resized. Where the rect
/// is larger than the snapshot, a fill in the color at the window's edge
/// shows instead.
pub(crate) struct AnimationWindow {
  handle: isize,
  dcomp_device: ThreadBound<IDCompositionDesktopDevice>,
  _dcomp_target: ThreadBound<IDCompositionTarget>,
  /// Holds the fill and the snapshot; moved to the window's current rect
  /// and clipped to its size.
  root_visual: ThreadBound<IDCompositionVisual3>,
  clip: ThreadBound<IDCompositionRectangleClip>,
  /// Scales the fill to the window's current size.
  fill_scale: ThreadBound<IDCompositionScaleTransform>,
  /// Scales the whole capture about its center (1.0 unless set by
  /// [`AnimationWindow::animate`]).
  root_scale: ThreadBound<IDCompositionScaleTransform>,
  /// Frame of the `AnimationWindow`.
  outer_rect: Rect,
  dispatcher: Dispatcher,
}

impl AnimationWindow {
  /// Implements [`AnimationWindow::new`].
  pub(crate) fn new(
    context: &AnimationContext,
    window: &NativeWindow,
    inner_rect: &Rect,
    outer_rect: &Rect,
    opacity: Option<OpacityValue>,
    dispatcher: &Dispatcher,
  ) -> crate::Result<Self> {
    let captured =
      PendingCapture::start(window.inner.hwnd().0, context)?.wait()?;

    Self::from_capture(
      context, window, &captured, inner_rect, outer_rect, opacity,
      dispatcher,
    )
  }

  /// Implements [`AnimationWindow::new_many`].
  ///
  /// All captures are started before any is waited on, so the wait for
  /// DWM's next frame is shared instead of paid once per window.
  pub(crate) fn new_many(
    context: &AnimationContext,
    requests: &[crate::AnimationWindowRequest],
    dispatcher: &Dispatcher,
  ) -> Vec<crate::Result<Self>> {
    let started_at = std::time::Instant::now();

    let pending = requests
      .iter()
      .map(|request| {
        PendingCapture::start(request.window.inner.hwnd().0, context)
      })
      .collect::<Vec<_>>();

    pending
      .into_iter()
      .zip(requests)
      .map(|(pending, request)| {
        let captured = pending?.wait()?;

        // Captures run in parallel, so this is when this window's frame
        // was ready, counted from when the first capture started.
        tracing::debug!(
          target: "glazewm::anim",
          capture_ms = started_at.elapsed().as_secs_f64() * 1000.0,
          "Window captured."
        );

        Self::from_capture(
          context,
          request.window,
          &captured,
          &request.inner_rect,
          &request.outer_rect,
          request.opacity,
          dispatcher,
        )
      })
      .collect()
  }

  /// Creates the overlay window from an already captured frame.
  fn from_capture(
    context: &AnimationContext,
    window: &NativeWindow,
    captured: &CapturedFrame,
    inner_rect: &Rect,
    outer_rect: &Rect,
    opacity: Option<OpacityValue>,
    dispatcher: &Dispatcher,
  ) -> crate::Result<Self> {
    dispatcher.dispatch_sync(|| {
      COM_INIT.with(|_| {
        // Window is spawned on the main thread - avoids having to create a
        // new message loop.
        let handle =
          Self::create_window(window.inner.hwnd().0, outer_rect)?;

        let dcomp_device = context.dcomp_device.get_ref()?;
        let dcomp_target =
          unsafe { dcomp_device.CreateTargetForHwnd(HWND(handle), true)? };

        let dcomp_surface = unsafe {
          dcomp_device.CreateSurface(
            inner_rect.width().cast_unsigned(),
            inner_rect.height().cast_unsigned(),
            DXGI_FORMAT_B8G8R8A8_UNORM,
            DXGI_ALPHA_MODE_PREMULTIPLIED,
          )?
        };

        let d3d_context = context.d3d_context.get_ref()?;

        Self::copy_texture_to_surface(
          d3d_context,
          captured.texture()?,
          &dcomp_surface,
        )?;

        // A failed sample only costs the fill its color.
        let fill_color =
          Self::edge_color(d3d_context, captured.texture()?)
            .unwrap_or_else(|err| {
              tracing::debug!("Failed to sample window edge color: {err}");
              [0; 4]
            });

        let fill_surface = unsafe {
          dcomp_device.CreateSurface(
            FILL_SIZE,
            FILL_SIZE,
            DXGI_FORMAT_B8G8R8A8_UNORM,
            DXGI_ALPHA_MODE_PREMULTIPLIED,
          )?
        };

        Self::fill_surface(d3d_context, &fill_surface, fill_color)?;

        let root_visual: IDCompositionVisual3 =
          unsafe { dcomp_device.CreateVisual()?.cast()? };
        let fill_visual: IDCompositionVisual3 =
          unsafe { dcomp_device.CreateVisual()?.cast()? };
        let snapshot_visual: IDCompositionVisual3 =
          unsafe { dcomp_device.CreateVisual()?.cast()? };

        let clip = unsafe { dcomp_device.CreateRectangleClip()? };
        let fill_scale = unsafe { dcomp_device.CreateScaleTransform()? };
        let root_scale = unsafe { dcomp_device.CreateScaleTransform()? };

        unsafe {
          // Nearest neighbor, so the scaled fill keeps hard edges instead
          // of blending into transparency.
          fill_visual.SetBitmapInterpolationMode(
            DCOMPOSITION_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
          )?;
          fill_visual.SetContent(&fill_surface)?;
          fill_visual.SetTransform(&fill_scale)?;
          snapshot_visual.SetContent(&dcomp_surface)?;

          clip.SetLeft2(0.0)?;
          clip.SetTop2(0.0)?;
          root_visual.SetClip(&clip)?;

          // Its own size until a close animation shrinks it.
          root_scale.SetScaleX2(1.0)?;
          root_scale.SetScaleY2(1.0)?;
          root_visual.SetTransform(&root_scale)?;

          let fill_visual: IDCompositionVisual = fill_visual.cast()?;
          root_visual.AddVisual(
            &fill_visual,
            false,
            None::<&IDCompositionVisual>,
          )?;
          root_visual.AddVisual(&snapshot_visual, true, &fill_visual)?;

          dcomp_target.SetRoot(&root_visual)?;
        }

        let root_visual =
          ThreadBound::new(root_visual, dispatcher.clone());
        let clip = ThreadBound::new(clip, dispatcher.clone());
        let fill_scale = ThreadBound::new(fill_scale, dispatcher.clone());
        let root_scale = ThreadBound::new(root_scale, dispatcher.clone());
        let dcomp_target =
          ThreadBound::new(dcomp_target, dispatcher.clone());

        context.transaction(
          || {
            Self::update_visual(
              root_visual.get_ref()?,
              clip.get_ref()?,
              fill_scale.get_ref()?,
              inner_rect,
              outer_rect,
              opacity.as_ref(),
            )
          },
          dispatcher,
        )??;

        Ok(Self {
          handle,
          dcomp_device: context.dcomp_device.clone(),
          _dcomp_target: dcomp_target,
          root_visual,
          clip,
          fill_scale,
          root_scale,
          outer_rect: outer_rect.clone(),
          dispatcher: dispatcher.clone(),
        })
      })
    })?
  }

  /// Implements [`AnimationWindow::resize`].
  pub(crate) fn resize(&mut self, outer_rect: &Rect) -> crate::Result<()> {
    self.outer_rect = outer_rect.clone();

    unsafe {
      SetWindowPos(
        HWND(self.handle),
        None,
        outer_rect.x(),
        outer_rect.y(),
        outer_rect.width(),
        outer_rect.height(),
        SWP_NOACTIVATE,
      )
    }
    .map_err(crate::Error::from)
  }

  /// Implements [`AnimationWindow::update`].
  pub(crate) fn update(
    &self,
    inner_rect: &Rect,
    opacity: Option<&OpacityValue>,
  ) -> crate::Result<()> {
    self.dispatcher.dispatch_sync(|| {
      Self::update_visual(
        self.root_visual.get_ref()?,
        self.clip.get_ref()?,
        self.fill_scale.get_ref()?,
        inner_rect,
        &self.outer_rect,
        opacity,
      )
    })?
  }

  /// Implements [`AnimationWindow::animate`].
  pub(crate) fn animate(
    &self,
    animation: &OverlayAnimation,
  ) -> crate::Result<()> {
    self.dispatcher.dispatch_sync(|| {
      let device = self.dcomp_device.get_ref()?;

      self.animate_rect(device, &animation.rect)?;

      if let Some(opacity) = &animation.opacity {
        let channel = Channel::new(
          device,
          opacity.from.0,
          opacity.to.0,
          &opacity.curve,
          OPACITY_ERROR,
        )?;

        let root_visual = self.root_visual.get_ref()?;

        // SAFETY: The visual belongs to this overlay's device, on whose
        // thread this runs.
        unsafe {
          channel.apply(
            |value| root_visual.SetOpacity2(value),
            |anim| root_visual.SetOpacity(anim),
          )?;
        }
      }

      let scale = animation.scale.clone().unwrap_or(AnimatedValue {
        from: 1.0,
        to: 1.0,
        curve: animation.rect.curve.clone(),
      });

      self.animate_scale(device, &scale, &animation.rect.to)
    })?
  }

  /// Animates where the snapshot is drawn within the overlay, and its
  /// size (the clip and the fill).
  #[allow(clippy::cast_precision_loss)]
  fn animate_rect(
    &self,
    device: &IDCompositionDesktopDevice,
    rect: &AnimatedValue<Rect>,
  ) -> crate::Result<()> {
    let fill_size = FILL_SIZE as f32;
    let outer = &self.outer_rect;

    // Builds the channel of one value of the rect.
    let channel = |value: &dyn Fn(&Rect) -> f32, max_error: f32| {
      Channel::new(
        device,
        value(&rect.from),
        value(&rect.to),
        &rect.curve,
        max_error,
      )
    };

    let offset_x =
      channel(&|rect| (rect.x() - outer.x()) as f32, PIXEL_ERROR)?;
    let offset_y =
      channel(&|rect| (rect.y() - outer.y()) as f32, PIXEL_ERROR)?;
    let clip_right = channel(&|rect| rect.width() as f32, PIXEL_ERROR)?;
    let clip_bottom = channel(&|rect| rect.height() as f32, PIXEL_ERROR)?;
    let fill_x = channel(
      &|rect| rect.width() as f32 / fill_size,
      PIXEL_ERROR / fill_size,
    )?;
    let fill_y = channel(
      &|rect| rect.height() as f32 / fill_size,
      PIXEL_ERROR / fill_size,
    )?;

    let root_visual = self.root_visual.get_ref()?;
    let clip = self.clip.get_ref()?;
    let fill_scale = self.fill_scale.get_ref()?;

    // SAFETY: The visual, clip and transform belong to this overlay's
    // device, on whose thread this runs.
    unsafe {
      offset_x.apply(
        |value| root_visual.SetOffsetX2(value),
        |anim| root_visual.SetOffsetX(anim),
      )?;
      offset_y.apply(
        |value| root_visual.SetOffsetY2(value),
        |anim| root_visual.SetOffsetY(anim),
      )?;
      clip_right.apply(
        |value| clip.SetRight2(value),
        |anim| clip.SetRight(anim),
      )?;
      clip_bottom.apply(
        |value| clip.SetBottom2(value),
        |anim| clip.SetBottom(anim),
      )?;
      fill_x.apply(
        |value| fill_scale.SetScaleX2(value),
        |anim| fill_scale.SetScaleX(anim),
      )?;
      fill_y.apply(
        |value| fill_scale.SetScaleY2(value),
        |anim| fill_scale.SetScaleY(anim),
      )?;
    }

    Ok(())
  }

  /// Animates the scale of the whole snapshot, about the center of `rect`
  /// (where it ends).
  #[allow(clippy::cast_precision_loss)]
  fn animate_scale(
    &self,
    device: &IDCompositionDesktopDevice,
    scale: &AnimatedValue<f32>,
    rect: &Rect,
  ) -> crate::Result<()> {
    let scale_x = Channel::new(
      device,
      scale.from,
      scale.to,
      &scale.curve,
      SCALE_ERROR,
    )?;
    let scale_y = Channel::new(
      device,
      scale.from,
      scale.to,
      &scale.curve,
      SCALE_ERROR,
    )?;

    let root_scale = self.root_scale.get_ref()?;

    // SAFETY: The transform belongs to this overlay's device, on whose
    // thread this runs.
    unsafe {
      // In the root visual's own coordinates.
      root_scale.SetCenterX2(rect.width() as f32 / 2.0)?;
      root_scale.SetCenterY2(rect.height() as f32 / 2.0)?;
      scale_x.apply(
        |value| root_scale.SetScaleX2(value),
        |anim| root_scale.SetScaleX(anim),
      )?;
      scale_y.apply(
        |value| root_scale.SetScaleY2(value),
        |anim| root_scale.SetScaleY(anim),
      )?;
    }

    Ok(())
  }

  /// Moves the visuals to the window's current rect within the overlay,
  /// and cuts them to its size.
  ///
  /// The window's frame isn't changed; only the visuals are updated.
  ///
  /// Shared by [`AnimationWindow::new`] and [`AnimationWindow::update`].
  /// Must be called inside `AnimationContext::transaction`.
  #[allow(clippy::cast_precision_loss)]
  fn update_visual(
    root_visual: &IDCompositionVisual3,
    clip: &IDCompositionRectangleClip,
    fill_scale: &IDCompositionScaleTransform,
    inner_rect: &Rect,
    outer_rect: &Rect,
    opacity: Option<&OpacityValue>,
  ) -> crate::Result<()> {
    let width = inner_rect.width() as f32;
    let height = inner_rect.height() as f32;

    unsafe {
      root_visual.SetOffsetX2((inner_rect.x() - outer_rect.x()) as f32)?;
      root_visual.SetOffsetY2((inner_rect.y() - outer_rect.y()) as f32)?;

      clip.SetRight2(width)?;
      clip.SetBottom2(height)?;

      fill_scale.SetScaleX2(width / FILL_SIZE as f32)?;
      fill_scale.SetScaleY2(height / FILL_SIZE as f32)?;
    }

    if let Some(opacity) = opacity {
      unsafe { root_visual.SetOpacity2(opacity.0)? };
    }

    Ok(())
  }

  /// Reads the color of the window near its bottom right corner, the
  /// part a larger rect uncovers.
  ///
  /// Sampled a little inside the edge, past the rounded corner and border.
  fn edge_color(
    context: &ID3D11DeviceContext,
    texture: &ID3D11Texture2D,
  ) -> crate::Result<[u8; 4]> {
    const INSET: u32 = 12;

    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { texture.GetDesc(&raw mut desc) };

    let x = desc.Width.saturating_sub(INSET);
    let y = desc.Height.saturating_sub(INSET);

    #[allow(clippy::cast_sign_loss)]
    let staging_desc = D3D11_TEXTURE2D_DESC {
      Width: 1,
      Height: 1,
      MipLevels: 1,
      ArraySize: 1,
      Format: desc.Format,
      SampleDesc: DXGI_SAMPLE_DESC {
        Count: 1,
        Quality: 0,
      },
      Usage: D3D11_USAGE_STAGING,
      BindFlags: 0,
      CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
      MiscFlags: 0,
    };

    let device = unsafe { context.GetDevice()? };
    let mut staging: Option<ID3D11Texture2D> = None;
    unsafe {
      device.CreateTexture2D(
        &raw const staging_desc,
        None,
        Some(&raw mut staging),
      )?;
    }
    let staging = staging.ok_or_else(|| {
      crate::Error::Platform("No staging texture.".to_string())
    })?;

    let pixel = D3D11_BOX {
      left: x,
      top: y,
      front: 0,
      right: x + 1,
      bottom: y + 1,
      back: 1,
    };

    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();

    unsafe {
      context.CopySubresourceRegion(
        &staging,
        0,
        0,
        0,
        0,
        texture,
        0,
        Some(&raw const pixel),
      );
      context.Map(
        &staging,
        0,
        D3D11_MAP_READ,
        0,
        Some(&raw mut mapped),
      )?;
      let color = *mapped.pData.cast::<[u8; 4]>();
      context.Unmap(&staging, 0);
      Ok(color)
    }
  }

  /// Fills a surface with one color (premultiplied BGRA, as captured).
  fn fill_surface(
    context: &ID3D11DeviceContext,
    surface: &IDCompositionSurface,
    color: [u8; 4],
  ) -> crate::Result<()> {
    let pixels = vec![color; (FILL_SIZE * FILL_SIZE) as usize];
    let mut offset = POINT::default();

    let texture: ID3D11Texture2D =
      unsafe { surface.BeginDraw(None, &raw mut offset)? };

    #[allow(clippy::cast_sign_loss)]
    let area = D3D11_BOX {
      left: offset.x as u32,
      top: offset.y as u32,
      front: 0,
      right: offset.x as u32 + FILL_SIZE,
      bottom: offset.y as u32 + FILL_SIZE,
      back: 1,
    };

    unsafe {
      context.UpdateSubresource(
        &texture,
        0,
        Some(&raw const area),
        pixels.as_ptr().cast(),
        FILL_SIZE * 4,
        0,
      );
      surface.EndDraw()?;
    }

    Ok(())
  }

  /// Implements [`AnimationWindow::destroy`].
  pub(crate) fn destroy(self) -> crate::Result<()> {
    self.dispatcher.dispatch_sync(|| {
      COM_INIT.with(|_| {
        unsafe {
          // Clear the visuals so that the compositor releases the GPU
          // resources on the next commit.
          if let Err(err) = self
            .root_visual
            .with(|root_visual| root_visual.RemoveAllVisuals())
          {
            tracing::warn!("Failed to clear DComp visual content: {err}");
          }

          if let Err(err) =
            self.dcomp_device.with(|dcomp_device| dcomp_device.Commit())
          {
            tracing::warn!("Failed to commit DComp teardown: {err}");
          }

          if let Err(err) = DestroyWindow(HWND(self.handle)) {
            tracing::warn!("Failed to destroy overlay HWND: {err}");
          }
        }
      });
    })
  }

  /// Creates the window, ordered above `source_hwnd`.
  fn create_window(
    source_hwnd: isize,
    rect: &Rect,
  ) -> crate::Result<isize> {
    const CLASS_NAME: PCWSTR = w!("AnimationWindow");

    static CLASS_REGISTERED: Once = Once::new();
    CLASS_REGISTERED.call_once(|| {
      let wnd_class = WNDCLASSW {
        lpszClassName: CLASS_NAME,
        lpfnWndProc: Some(AnimationWindow::overlay_wnd_proc),
        ..Default::default()
      };
      unsafe { RegisterClassW(&raw const wnd_class) };
    });

    let hwnd = unsafe {
      CreateWindowExW(
        WS_EX_NOREDIRECTIONBITMAP | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
        CLASS_NAME,
        w!(""),
        WS_POPUP,
        rect.x(),
        rect.y(),
        rect.width(),
        rect.height(),
        None,
        None,
        None,
        None,
      )
    };

    if hwnd.0 == 0 {
      return Err(crate::Error::Platform(
        "Failed to create animation window.".to_string(),
      ));
    }

    // Order the animation window just above the source window and show it.
    unsafe {
      SetWindowPos(
        hwnd,
        HWND(source_hwnd),
        0,
        0,
        0,
        0,
        SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
      )?;
    }

    Ok(hwnd.0)
  }

  /// Window procedure for the overlay class.
  unsafe extern "system" fn overlay_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
  ) -> LRESULT {
    // Route all mouse inputs to the window below.
    if msg == WM_NCHITTEST {
      LRESULT(HTTRANSPARENT as isize)
    } else {
      DefWindowProcW(hwnd, msg, wparam, lparam)
    }
  }

  /// Copies the contents of a 2D texture into a DirectComposition
  /// surface.
  fn copy_texture_to_surface(
    context: &ID3D11DeviceContext,
    src_texture: &ID3D11Texture2D,
    surface: &IDCompositionSurface,
  ) -> crate::Result<()> {
    let mut offset = POINT::default();

    let update_texture: ID3D11Texture2D =
      unsafe { surface.BeginDraw(None, &raw mut offset)? };

    #[allow(clippy::cast_sign_loss)]
    unsafe {
      context.CopySubresourceRegion(
        &update_texture,
        0,
        offset.x as u32,
        offset.y as u32,
        0,
        src_texture,
        0,
        None,
      );
    }

    unsafe { surface.EndDraw()? };

    Ok(())
  }
}

/// A screen capture of a window via `Windows.Graphics.Capture`.
struct CapturedFrame {
  texture: Option<ID3D11Texture2D>,
  frame: Direct3D11CaptureFrame,
  session: GraphicsCaptureSession,
  frame_pool: Direct3D11CaptureFramePool,
}

/// A window capture that has been started but not yet received a frame.
struct PendingCapture {
  session: GraphicsCaptureSession,
  frame_pool: Direct3D11CaptureFramePool,
  frame_rx: std::sync::mpsc::Receiver<()>,
}

impl PendingCapture {
  /// How long to wait for DWM to deliver a frame.
  const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

  /// Starts capturing a single frame of a given window.
  ///
  /// Returns immediately; call [`PendingCapture::wait`] for the frame.
  fn start(
    hwnd: isize,
    context: &AnimationContext,
  ) -> crate::Result<Self> {
    let capture_item: GraphicsCaptureItem =
      unsafe { context.capture_interop.0.CreateForWindow(HWND(hwnd))? };

    // NOTE: `Direct3D11CaptureFramePool` cannot be reused across captures,
    // so we can't store it in `AnimationContext`.
    let frame_pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
      &context.winrt_device.0,
      DirectXPixelFormat::B8G8R8A8UIntNormalized,
      1,
      capture_item.Size()?,
    )?;

    let session = frame_pool.CreateCaptureSession(&capture_item)?;

    // Prevent the cursor from being shown in the capture.
    let _ = session.SetIsCursorCaptureEnabled(false);

    // Disable the yellow capture border on Windows 11 (Build 22000+).
    if let Err(err) = session.SetIsBorderRequired(false) {
      tracing::debug!("Capture border stays on: {err}");
    }

    let (tx, frame_rx) = std::sync::mpsc::sync_channel::<()>(1);

    frame_pool.FrameArrived(&TypedEventHandler::new(
      move |_: &Option<Direct3D11CaptureFramePool>, _| {
        // A full channel means a frame is already waiting to be read.
        let _ = tx.try_send(());
        Ok(())
      },
    ))?;

    session.StartCapture()?;

    Ok(Self {
      session,
      frame_pool,
      frame_rx,
    })
  }

  /// Blocks until the capture's first frame has arrived.
  ///
  /// Perf: up to one DWM frame (e.g. 16.7ms at 60Hz) after `start`.
  fn wait(self) -> crate::Result<CapturedFrame> {
    let close = |pending: &Self| {
      let _ = pending.session.Close();
      let _ = pending.frame_pool.Close();
    };

    if self.frame_rx.recv_timeout(Self::TIMEOUT).is_err() {
      close(&self);
      return Err(crate::Error::Platform("WGC capture timed out.".into()));
    }

    let frame = match self.frame_pool.TryGetNextFrame() {
      Ok(frame) => frame,
      Err(err) => {
        close(&self);
        return Err(crate::Error::Platform(format!(
          "Failed to get WGC frame: {err}"
        )));
      }
    };

    // From here on, `CapturedFrame` owns the resources and closes them
    // when dropped.
    let mut captured = CapturedFrame {
      texture: None,
      frame,
      session: self.session,
      frame_pool: self.frame_pool,
    };

    let surface = captured.frame.Surface()?;
    let access: IDirect3DDxgiInterfaceAccess = surface.cast()?;
    captured.texture = Some(unsafe { access.GetInterface()? });

    Ok(captured)
  }
}

impl CapturedFrame {
  /// Gets the captured texture.
  fn texture(&self) -> crate::Result<&ID3D11Texture2D> {
    self.texture.as_ref().ok_or_else(|| {
      crate::Error::Platform("Captured frame has no texture.".to_string())
    })
  }

  /// Releases WGC resources in dependency order.
  fn close(&self) -> crate::Result<()> {
    self.frame.Close()?;
    self.session.Close()?;
    self.frame_pool.Close()?;
    Ok(())
  }
}

impl Drop for CapturedFrame {
  fn drop(&mut self) {
    let _ = self.close();
  }
}

/// Largest error of an animated value from its curve, in the value's own
/// units: half a pixel for positions and sizes.
const PIXEL_ERROR: f32 = 0.5;
const OPACITY_ERROR: f32 = 0.002;
const SCALE_ERROR: f32 = 0.0005;

/// One animated property of an overlay: a fixed value, or an animation
/// for the compositor to play.
enum Channel {
  Fixed(f32),
  Animated(IDCompositionAnimation),
}

impl Channel {
  /// Builds the animation of a value from `from` to `to` along `curve`,
  /// within `max_error` of the curve. A value that doesn't change, or
  /// changes at once, is fixed at `to`.
  fn new(
    device: &IDCompositionDesktopDevice,
    from: f32,
    to: f32,
    curve: &AnimationCurve,
    max_error: f32,
  ) -> crate::Result<Self> {
    let duration_s = curve.duration.as_secs_f64();

    if (from - to).abs() < f32::EPSILON || duration_s <= 0.0 {
      return Ok(Self::Fixed(to));
    }

    // SAFETY: Called on the device's thread, with plain values.
    unsafe {
      let animation = device.CreateAnimation()?;

      for segment in
        curve.easing.cubic_segments(from, to, duration_s, max_error)
      {
        animation.AddCubic(
          segment.begin,
          segment.constant,
          segment.linear,
          segment.quadratic,
          segment.cubic,
        )?;
      }

      // Holds the last value from here on.
      animation.End(duration_s, to)?;

      Ok(Self::Animated(animation))
    }
  }

  /// Sets the property: `set_value` for a fixed value, `set_animation`
  /// for an animation.
  fn apply(
    &self,
    set_value: impl FnOnce(f32) -> windows::core::Result<()>,
    set_animation: impl FnOnce(
      &IDCompositionAnimation,
    ) -> windows::core::Result<()>,
  ) -> crate::Result<()> {
    match self {
      Self::Fixed(value) => set_value(*value)?,
      Self::Animated(animation) => set_animation(animation)?,
    }

    Ok(())
  }
}
