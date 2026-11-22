use rsmpeg_codec::Frame;
use rsmpeg_util::{unsupported, PixelFormat, Rational, RsError, RsResult};

use crate::colorspace::{ColorConversion, ColorRange, ColorSpace};

/// Interpolation method used during scaling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterpolationMethod {
    NearestNeighbor,
    Bilinear,
    Bicubic,
    Lanczos,
    Sinc,
    Spline,
    Gaussian,
}

impl InterpolationMethod {
    pub fn name(&self) -> &'static str {
        match self {
            InterpolationMethod::NearestNeighbor => "nearest",
            InterpolationMethod::Bilinear => "bilinear",
            InterpolationMethod::Bicubic => "bicubic",
            InterpolationMethod::Lanczos => "lanczos",
            InterpolationMethod::Sinc => "sinc",
            InterpolationMethod::Spline => "spline",
            InterpolationMethod::Gaussian => "gaussian",
        }
    }
}

bitflags::bitflags! {
    /// Scaler control flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ScalerFlags: u32 {
        const NONE = 0;
        const PRINT_INFO = 1 << 0;
        const ACCURATE_ROUNDING = 1 << 1;
        const FULL_CHROMA_INT = 1 << 2;
        const BILINEAR = 1 << 3;
        const BICUBIC = 1 << 4;
    }
}

/// Configuration for setting up a Scaler.
#[derive(Debug, Clone)]
pub struct ScalerConfig {
    pub src_width: u32,
    pub src_height: u32,
    pub src_format: PixelFormat,
    pub dst_width: u32,
    pub dst_height: u32,
    pub dst_format: PixelFormat,
    pub interpolation: InterpolationMethod,
    pub color_conversion: Option<ColorConversion>,
    pub flags: ScalerFlags,
}

/// Borrowed YUV420P planes plus their strides (`Y`, `U`, `V`, `y_stride`, `u_stride`, `v_stride`).
type Yuv420pPlanes<'a> = (&'a [u8], &'a [u8], &'a [u8], usize, usize, usize);

impl ScalerConfig {
    pub fn new(
        src_width: u32,
        src_height: u32,
        src_format: PixelFormat,
        dst_width: u32,
        dst_height: u32,
        dst_format: PixelFormat,
    ) -> Self {
        ScalerConfig {
            src_width,
            src_height,
            src_format,
            dst_width,
            dst_height,
            dst_format,
            interpolation: InterpolationMethod::Bilinear,
            color_conversion: None,
            flags: ScalerFlags::NONE,
        }
    }

    pub fn with_interpolation(mut self, method: InterpolationMethod) -> Self {
        self.interpolation = method;
        self
    }

    pub fn with_color_conversion(mut self, conv: ColorConversion) -> Self {
        self.color_conversion = Some(conv);
        self
    }
}

/// Video scaler — converts between pixel formats, dimensions, and color spaces.
///
/// Equivalent to FFmpeg's SwsContext. Reuses the stored [`ScalerConfig`] across
/// frames; construct a new [`Scaler`] only when dimensions or formats change.
pub struct Scaler {
    config: ScalerConfig,
    /// Effective color conversion (default BT.601 limited → full RGB).
    color: ColorConversion,
}

impl Scaler {
    pub fn new(config: ScalerConfig) -> RsResult<Self> {
        if config.src_width == 0 || config.src_height == 0 {
            return Err(RsError::InvalidData(
                "scaler source size must be non-zero".into(),
            ));
        }
        if config.dst_width == 0 || config.dst_height == 0 {
            return Err(RsError::InvalidData(
                "scaler destination size must be non-zero".into(),
            ));
        }

        let color = config.color_conversion.clone().unwrap_or_else(|| {
            ColorConversion::new(
                ColorSpace::BT601,
                ColorSpace::RGB,
                ColorRange::Limited,
                ColorRange::Full,
            )
        });

        tracing::debug!(
            "Scaler: {}x{} {:?} -> {}x{} {:?} ({})",
            config.src_width,
            config.src_height,
            config.src_format,
            config.dst_width,
            config.dst_height,
            config.dst_format,
            config.interpolation.name(),
        );
        Ok(Scaler { config, color })
    }

    pub fn config(&self) -> &ScalerConfig {
        &self.config
    }

    /// Scale a frame from source to destination format/dimensions.
    ///
    /// Currently implements real conversion for:
    /// - `Yuv420P` → `Rgba` / `Rgb24` / `Bgr24`
    /// - `Nv12` → `Rgba` / `Rgb24`
    /// - `Yuv422P` → `Rgba`
    /// - `Rgb24` → `Rgba`
    /// - same format → same format (with dimension scaling)
    ///
    /// Dimension changes use nearest-neighbor sampling of the source.
    /// Returns a new frame with correctly sized packed planes.
    pub fn scale(&self, frame: &Frame) -> RsResult<Frame> {
        tracing::debug!(
            "Scaling frame {}x{} -> {}x{}",
            frame.width,
            frame.height,
            self.config.dst_width,
            self.config.dst_height,
        );

        // Same format with a different size still needs a real resample, so the
        // identity fast path below only covers the no-op case.
        if self.config.src_format == PixelFormat::Yuv420P
            && self.config.dst_format == PixelFormat::Yuv420P
            && (self.config.src_width != self.config.dst_width
                || self.config.src_height != self.config.dst_height)
        {
            return self.scale_yuv420p_to_yuv420p(frame);
        }

        match (self.config.src_format, self.config.dst_format) {
            (PixelFormat::Yuv420P, PixelFormat::Rgba) => self.scale_yuv420p_to_packed(frame, 4),
            (PixelFormat::Yuv420P, PixelFormat::Rgb24) => self.scale_yuv420p_to_packed(frame, 3),
            (PixelFormat::Yuv420P, PixelFormat::Bgr24) => {
                self.scale_yuv420p_to_packed(frame, 0)
            }
            (PixelFormat::Nv12, PixelFormat::Rgba) => self.scale_nv12_to_rgba(frame),
            (PixelFormat::Nv12, PixelFormat::Rgb24) => self.scale_nv12_to_rgb24(frame),
            (PixelFormat::Yuv422P, PixelFormat::Rgba) => self.scale_yuv422p_to_rgba(frame),
            (PixelFormat::Rgb24, PixelFormat::Rgba) => self.scale_rgb24_to_rgba(frame),
            (src, dst)
                if src == dst
                    && self.config.src_width == self.config.dst_width
                    && self.config.src_height == self.config.dst_height =>
            {
                // Same format / size: the frame is returned as-is, which is only
                // safe if it really carries the configured format. Without this
                // check a `Bgr24` frame passed to an `Rgb24` scaler came back
                // labelled `Bgr24` — the caller then feeds blue where red is
                // expected, and nothing downstream can notice.
                Self::require_source_format(frame, src)?;
                // The fast path only applies to a frame whose geometry already
                // matches the configured source. A 1×1 frame handed to a 2×2
                // scaler used to come back as 1×1 while the caller believes it
                // asked for 2×2.
                Self::validate_source_geometry(frame, &self.config)?;
                // Validate the planes too: an identity copy of an overlapping
                // or truncated plane set hands the corruption straight on.
                Self::validate_source_planes(frame, src)?;
                // The output *is* the input, so its size is bounded by the actual
                // plane lengths. Estimating as "first plane length × pixel count"
                // was nonsense (1920×1080 RGB24 → 12.9e12 "bytes") and rejected
                // valid frames on 32-bit targets.
                Self::validate_identity_capacity(frame)?;
                Ok(clone_frame_metadata(
                    frame,
                    frame.data.clone(),
                    frame.linesize.clone(),
                ))
            }
            (src, dst) => Err(unsupported!(
                "scale conversion {:?} -> {:?} is not implemented yet",
                src,
                dst
            )),
        }
    }

    /// Validate the three planes of a 4:2:0 planar frame against `use_w × use_h`.
    ///
    /// Returns the plane references and strides on success. Extra padding in `linesize`
    /// is tolerated; short buffers are reported as [`RsError::InvalidData`] instead of
    /// panicking later on an out-of-bounds index.
    /// Check that `frame` really is a `expected`-format frame before its planes are
    /// interpreted with that format's layout.
    ///
    /// The scaler's configuration alone is not evidence: passing a `Bgr24` frame to a
    /// scaler configured for `Rgb24` produced valid-looking (but red/blue swapped)
    /// output. Rejecting the mismatch is the only safe answer, since the two layouts
    /// are indistinguishable from the byte counts alone.
    fn require_source_format(frame: &Frame, expected: PixelFormat) -> RsResult<()> {
        if frame.pixel_format != expected {
            return Err(RsError::InvalidData(
                format!(
                    "scaler configured for {:?} source but got a {:?} frame",
                    expected, frame.pixel_format
                )
                .into(),
            ));
        }
        Ok(())
    }

    /// Validate one plane's stride and buffer length for `width × height` samples.
    ///
    /// A stride shorter than the pixel row makes consecutive rows overlap, which
    /// silently shifts pixels (`linesize = 3` for a 2-pixel RGB24 row produced a
    /// sheared image). `bytes_per_sample` is 1 for 8-bit planes and 3/4 for the
    /// packed RGB layouts.
    fn validate_plane(
        plane: &[u8],
        linesize: usize,
        width: usize,
        height: usize,
        bytes_per_sample: usize,
        what: &str,
    ) -> RsResult<()> {
        let row_bytes = width
            .checked_mul(bytes_per_sample)
            .ok_or_else(|| RsError::InvalidData(format!("{what}: row size overflow").into()))?;
        if linesize < row_bytes {
            return Err(RsError::InvalidData(
                format!(
                    "{what}: linesize {linesize} is smaller than one pixel row ({row_bytes} \
                     bytes); rows would overlap"
                )
                .into(),
            ));
        }
        let need = if height == 0 {
            0
        } else {
            linesize
                .checked_mul(height - 1)
                .and_then(|o| o.checked_add(row_bytes))
                .ok_or_else(|| RsError::InvalidData(format!("{what}: size overflow").into()))?
        };
        if plane.len() < need {
            return Err(RsError::InvalidData(
                format!(
                    "{what}: plane buffer is too small (have {} bytes, need {need} for \
                     {width}x{height} with linesize {linesize})",
                    plane.len()
                )
                .into(),
            ));
        }
        Ok(())
    }

    /// Check that `frame`'s own dimensions equal the configured source size.
    ///
    /// The identity path returns the frame untouched, so a mismatched geometry
    /// would silently hand back the wrong resolution.
    fn validate_source_geometry(frame: &Frame, config: &ScalerConfig) -> RsResult<()> {
        let (want_w, want_h) = (config.src_width as usize, config.src_height as usize);
        if frame.width != want_w || frame.height != want_h {
            return Err(RsError::InvalidData(
                format!(
                    "scale: source frame is {}x{} but the scaler is configured for {want_w}x{want_h}",
                    frame.width, frame.height
                )
                .into(),
            ));
        }
        Ok(())
    }

    /// Validate that the planes an identity copy would move are addressable.
    ///
    /// Checks each plane's real length (and the total against `isize::MAX`)
    /// instead of multiplying a plane length by a pixel count.
    fn validate_identity_capacity(frame: &Frame) -> RsResult<()> {
        let mut total: usize = 0;
        for (i, plane) in frame.data.iter().enumerate() {
            if plane.len() > isize::MAX as usize {
                return Err(RsError::InvalidData(
                    format!("scale: plane {i} is larger than an addressable buffer").into(),
                ));
            }
            total = total
                .checked_add(plane.len())
                .filter(|t| *t <= isize::MAX as usize)
                .ok_or_else(|| {
                    RsError::InvalidData(
                        "scale: identity copy would exceed the maximum addressable buffer".into(),
                    )
                })?;
        }
        Ok(())
    }

    /// Check that the destination frame of `width × height × bytes_per_sample` can be
    /// allocated without overflowing `usize`.
    fn validate_output_size(width: usize, height: usize, bytes_per_pixel: usize) -> RsResult<()> {
        width
            .checked_mul(height)
            .and_then(|n| n.checked_mul(bytes_per_pixel))
            .ok_or_else(|| {
                RsError::InvalidData(
                    format!(
                        "scale: output size {width}x{height}x{bytes_per_pixel} bytes overflows"
                    )
                    .into(),
                )
            })
            // `Vec` cannot describe more than `isize::MAX` bytes, so a size that
            // merely avoids `usize` overflow can still be unallocatable. Reject
            // it here rather than aborting inside the allocator.
            .and_then(|n| {
                if n > isize::MAX as usize {
                    Err(RsError::InvalidData(
                        format!(
                            "scale: output size {width}x{height}x{bytes_per_pixel} = {n} bytes                              exceeds the maximum allocatable buffer"
                        )
                        .into(),
                    ))
                } else {
                    Ok(())
                }
            })
            .map(|_| ())
    }

    /// Validate every plane of a source `frame` for its declared format.
    ///
    /// Used by the identity path, which copies planes verbatim: an overlapping
    /// stride or truncated plane would be copied as-is and the corruption would
    /// surface far from here.
    fn validate_source_planes(frame: &Frame, format: PixelFormat) -> RsResult<()> {
        let (w, h) = (frame.width, frame.height);
        if w == 0 || h == 0 {
            return Err(RsError::InvalidData(
                "scale: source frame has zero width or height".into(),
            ));
        }
        // Bytes per sample: 10/12-bit and 16-bit formats store one sample in a
        // 16-bit word, so their strides are twice the pixel count. Treating them
        // as 8-bit accepted a half-width plane and copied it verbatim.
        let bps = |format: PixelFormat| -> usize {
            match format {
                PixelFormat::Gray16
                | PixelFormat::Yuv420P10
                | PixelFormat::Yuv420P12 => 2,
                _ => 1,
            }
        };
        let layouts: Vec<(usize, usize, usize, &'static str)> = match format {
            PixelFormat::Yuv420P => vec![
                (w, h, 1, "Y"),
                (w.div_ceil(2), h.div_ceil(2), 1, "U"),
                (w.div_ceil(2), h.div_ceil(2), 1, "V"),
            ],
            PixelFormat::Yuv420P10 | PixelFormat::Yuv420P12 => vec![
                (w, h, 2, "Y"),
                (w.div_ceil(2), h.div_ceil(2), 2, "U"),
                (w.div_ceil(2), h.div_ceil(2), 2, "V"),
            ],
            PixelFormat::Yuv422P => vec![
                (w, h, 1, "Y"),
                (w.div_ceil(2), h, 1, "U"),
                (w.div_ceil(2), h, 1, "V"),
            ],
            PixelFormat::Yuv444P => vec![(w, h, 1, "Y"), (w, h, 1, "U"), (w, h, 1, "V")],
            PixelFormat::Nv12 | PixelFormat::Nv21 => {
                vec![(w, h, 1, "Y"), (w.div_ceil(2), h.div_ceil(2), 2, "UV")]
            }
            PixelFormat::Rgb24 | PixelFormat::Bgr24 => vec![(w, h, 3, "packed")],
            PixelFormat::Rgba | PixelFormat::Bgra | PixelFormat::Argb => vec![(w, h, 4, "packed")],
            PixelFormat::Gray8 => vec![(w, h, 1, "Y")],
            PixelFormat::Gray16 => vec![(w, h, 2, "Y")],
            // Unknown layout: require at least one 8-bit plane of the full size
            // rather than silently accepting anything.
            PixelFormat::None => vec![(w, h, bps(format), "plane")],
        };
        if frame.data.len() < layouts.len() || frame.linesize.len() < layouts.len() {
            return Err(RsError::InvalidData(
                format!(
                    "scale: {format:?} frame has {} plane(s), expected {}",
                    frame.data.len(),
                    layouts.len()
                )
                .into(),
            ));
        }
        for (i, (pw, ph, bpp, name)) in layouts.into_iter().enumerate() {
            Self::validate_plane(
                &frame.data[i],
                frame.linesize[i],
                pw,
                ph,
                bpp,
                &format!("{format:?} {name}"),
            )?;
        }
        Ok(())
    }


    fn validate_yuv420p_planes<'a>(
        &self,
        frame: &'a Frame,
        use_w: usize,
        use_h: usize,
    ) -> RsResult<Yuv420pPlanes<'a>> {
        if frame.data.len() < 3 || frame.linesize.len() < 3 {
            return Err(RsError::InvalidData(
                "YUV420P frame requires 3 planes (Y, U, V)".into(),
            ));
        }

        // Explicit strides: the 4:2:0 layout is derived from `use_w`/`use_h`, so a
        // declared `linesize` that does not match shifts every row.
        let chroma_w = use_w.div_ceil(2);
        let chroma_h = use_h.div_ceil(2);
        Self::validate_plane(&frame.data[0], frame.linesize[0], use_w, use_h, 1, "YUV420P Y")?;
        Self::validate_plane(
            &frame.data[1],
            frame.linesize[1],
            chroma_w,
            chroma_h,
            1,
            "YUV420P U",
        )?;
        Self::validate_plane(
            &frame.data[2],
            frame.linesize[2],
            chroma_w,
            chroma_h,
            1,
            "YUV420P V",
        )?;

        let y_stride = frame.linesize[0];
        let u_stride = frame.linesize[1];
        let v_stride = frame.linesize[2];

        // Rows are `stride` apart, each row uses `width` bytes.
        let y_need = y_stride
            .checked_mul(use_h.saturating_sub(1))
            .and_then(|o| o.checked_add(use_w));
        let u_need = u_stride
            .checked_mul(chroma_h.saturating_sub(1))
            .and_then(|o| o.checked_add(chroma_w));
        let v_need = v_stride
            .checked_mul(chroma_h.saturating_sub(1))
            .and_then(|o| o.checked_add(chroma_w));

        let (y_plane, u_plane, v_plane) = (&frame.data[0], &frame.data[1], &frame.data[2]);
        match (y_need, u_need, v_need) {
            (Some(yn), Some(un), Some(vn))
                if y_plane.len() >= yn && u_plane.len() >= un && v_plane.len() >= vn => {}
            _ => {
                return Err(RsError::InvalidData(
                    "YUV420P plane buffers are too small for declared size/linesize".into(),
                ));
            }
        }

        Ok((y_plane, u_plane, v_plane, y_stride, u_stride, v_stride))
    }

    fn scale_yuv420p_to_packed(&self, frame: &Frame, bpp: usize) -> RsResult<Frame> {
        Self::require_source_format(frame, PixelFormat::Yuv420P)?;
        let src_w = self.source_width();
        let src_h = self.source_height();
        let dst_w = self.config.dst_width as usize;
        let dst_h = self.config.dst_height as usize;

        // Prefer configured size; fall back to frame metadata when they match.
        let (use_w, use_h) = if frame.width == src_w && frame.height == src_h {
            (src_w, src_h)
        } else if frame.width > 0 && frame.height > 0 {
            // Allow slight mismatch if the frame carries the true size.
            (frame.width, frame.height)
        } else {
            (src_w, src_h)
        };

        let (y_plane, u_plane, v_plane, y_stride, u_stride, v_stride) =
            self.validate_yuv420p_planes(frame, use_w, use_h)?;

        let limited = self.color.src_range == ColorRange::Limited;
        // `bpp == 0` selects packed BGR24, which has the same 3-byte stride as RGB24
        // but a different byte order.
        let bgr = bpp == 0;
        let bytes_per_pixel = if bgr { 3 } else { bpp };
        Self::validate_output_size(dst_w, dst_h, bytes_per_pixel)?;
        let mut out = vec![0u8; dst_w * dst_h * bytes_per_pixel];
        let out_stride = dst_w * bytes_per_pixel;
        let src_w_safe = use_w.max(1);
        let src_h_safe = use_h.max(1);

        // Nearest-neighbor map destination → source.
        for dy in 0..dst_h {
            let sy = if dst_h == src_h_safe {
                dy
            } else {
                (dy as u64 * src_h_safe as u64 / dst_h as u64) as usize
            };
            let sy = sy.min(src_h_safe - 1);
            let cy = sy / 2;

            for dx in 0..dst_w {
                let sx = if dst_w == src_w_safe {
                    dx
                } else {
                    (dx as u64 * src_w_safe as u64 / dst_w as u64) as usize
                };
                let sx = sx.min(src_w_safe - 1);
                let cx = sx / 2;

                let y = y_plane[sy * y_stride + sx];
                let u = u_plane[cy * u_stride + cx];
                let v = v_plane[cy * v_stride + cx];

                let (r, g, b) = yuv_to_rgb_bt601(y, u, v, limited);
                let off = dy * out_stride + dx * bytes_per_pixel;
                out[off] = if bgr { b } else { r };
                out[off + 1] = g;
                out[off + 2] = if bgr { r } else { b };
                if bytes_per_pixel == 4 {
                    out[off + 3] = 255;
                }
            }
        }

        Ok(self.packed_video_frame(frame, out, out_stride, dst_w, dst_h))
    }

    /// Source width from the config, saturating into `usize`.
    #[inline]
    fn source_width(&self) -> usize {
        usize::try_from(self.config.src_width).unwrap_or(usize::MAX / 4)
    }

    /// Source height from the config, saturating into `usize`.
    #[inline]
    fn source_height(&self) -> usize {
        usize::try_from(self.config.src_height).unwrap_or(usize::MAX / 4)
    }

    /// Resolve the usable source geometry for `frame`.
    ///
    /// The configured size is authoritative; a frame that carries its own non-zero
    /// dimensions is accepted when it differs (matching the historical behaviour).
    fn source_geometry(&self, frame: &Frame) -> (usize, usize) {
        let src_w = self.source_width();
        let src_h = self.source_height();
        if frame.width == src_w && frame.height == src_h {
            (src_w, src_h)
        } else if frame.width > 0 && frame.height > 0 {
            (frame.width, frame.height)
        } else {
            (src_w, src_h)
        }
    }

    /// Build a packed (single-plane, 8-bit) video frame from an output buffer.
    fn packed_video_frame(
        &self,
        src: &Frame,
        out: Vec<u8>,
        out_stride: usize,
        dst_w: usize,
        dst_h: usize,
    ) -> Frame {
        let mut result = Frame {
            data: vec![out],
            linesize: vec![out_stride],
            width: dst_w,
            height: dst_h,
            pixel_format: self.config.dst_format,
            sample_format: src.sample_format,
            sample_rate: 0,
            channels: 0,
            samples: 0,
            pts: src.pts,
            duration: src.duration,
            time_base: src.time_base,
            key_frame: src.key_frame,
            pict_type: src.pict_type,
        };
        // Keep audio fields zeroed for video output; ensure time_base is valid.
        if result.time_base.den == 0 {
            result.time_base = Rational::new(1, 1000);
        }
        result
    }

    /// `Nv12` → `Rgba`, optionally rescaling with nearest-neighbor sampling.
    fn scale_nv12_to_rgba(&self, frame: &Frame) -> RsResult<Frame> {
        self.scale_semi_planar_to_packed(frame, PixelFormat::Nv12, 2, true, true)
    }

    /// `Nv12` → `Rgb24`, optionally rescaling with nearest-neighbor sampling.
    fn scale_nv12_to_rgb24(&self, frame: &Frame) -> RsResult<Frame> {
        self.scale_semi_planar_to_packed(frame, PixelFormat::Nv12, 2, false, true)
    }

    /// `Yuv422P` → `Rgba`, optionally rescaling with nearest-neighbor sampling.
    fn scale_yuv422p_to_rgba(&self, frame: &Frame) -> RsResult<Frame> {
        self.scale_semi_planar_to_packed(frame, PixelFormat::Yuv422P, 3, true, false)
    }

    /// Shared implementation for the `Nv12` / `Yuv422P` → packed RGB(A) paths.
    ///
    /// `alpha` selects 4 bytes/pixel RGBA versus 3 bytes/pixel RGB24; `interleaved_uv`
    /// selects NV12 semi-planar chroma over planar 4:2:2 chroma.
    fn scale_semi_planar_to_packed(
        &self,
        frame: &Frame,
        expected: PixelFormat,
        planes: usize,
        alpha: bool,
        interleaved_uv: bool,
    ) -> RsResult<Frame> {
        Self::require_source_format(frame, expected)?;
        if frame.data.len() < planes || frame.linesize.len() < planes {
            return Err(RsError::InvalidData(
                format!("{:?} frame requires {} planes", expected, planes).into(),
            ));
        }

        let (use_w, use_h) = self.source_geometry(frame);
        let dst_w = self.config.dst_width as usize;
        let dst_h = self.config.dst_height as usize;
        let use_w = use_w.max(1);
        let use_h = use_h.max(1);

        let chroma_w = use_w.div_ceil(2);
        // 4:2:0 halves chroma vertically, 4:2:2 keeps full height.
        let chroma_h = if interleaved_uv {
            use_h.div_ceil(2)
        } else {
            use_h
        };
        let uv_bytes = if interleaved_uv { 2 } else { 1 };

        let format_name = format!("{expected:?}");
        Self::validate_plane(
            &frame.data[0],
            frame.linesize[0],
            use_w,
            use_h,
            1,
            &format!("{format_name} Y"),
        )?;
        Self::validate_plane(
            &frame.data[1],
            frame.linesize[1],
            chroma_w,
            chroma_h,
            uv_bytes,
            &format!("{format_name} UV"),
        )?;
        if !interleaved_uv {
            Self::validate_plane(
                &frame.data[2],
                frame.linesize[2],
                chroma_w,
                chroma_h,
                1,
                &format!("{format_name} V"),
            )?;
        }

        let y_stride = frame.linesize[0];
        let uv_stride = frame.linesize[1];
        let y_need = y_stride
            .checked_mul(use_h.saturating_sub(1))
            .and_then(|o| o.checked_add(use_w));
        let uv_need = uv_stride
            .checked_mul(chroma_h.saturating_sub(1))
            .and_then(|o| o.checked_add(chroma_w.checked_mul(uv_bytes)?));
        let v_need = if interleaved_uv {
            Some(0)
        } else {
            let v_stride = frame.linesize[2];
            v_stride
                .checked_mul(use_h.saturating_sub(1))
                .and_then(|o| o.checked_add(chroma_w))
        };

        let y_plane = &frame.data[0];
        let uv_plane = &frame.data[1];
        match (y_need, uv_need, v_need) {
            (Some(yn), Some(uvn), Some(vn))
                if y_plane.len() >= yn
                    && uv_plane.len() >= uvn
                    && (interleaved_uv || frame.data[2].len() >= vn) => {}
            _ => {
                return Err(RsError::InvalidData(
                    format!(
                        "{:?} plane buffers are too small for declared size/linesize",
                        expected
                    )
                    .into(),
                ));
            }
        }
        let v_plane: &[u8] = if interleaved_uv {
            &[]
        } else {
            &frame.data[2]
        };
        let v_stride = if interleaved_uv {
            0
        } else {
            frame.linesize[2]
        };

        // BT.601 limited range, matching the Yuv420P paths.
        let limited = self.color.src_range == ColorRange::Limited;
        let bpp = if alpha { 4 } else { 3 };
        // Same destination check as the other output paths: `dst_w * bpp` can
        // overflow `usize` for a caller-supplied destination size.
        Self::validate_output_size(dst_w, dst_h, bpp)?;
        let out_stride = dst_w * bpp;
        let mut out = vec![0u8; out_stride * dst_h];

        for dy in 0..dst_h {
            let sy = (dy as u64 * use_h as u64 / dst_h as u64) as usize;
            let sy = sy.min(use_h - 1);
            let cy = if interleaved_uv { sy / 2 } else { sy };
            for dx in 0..dst_w {
                let sx = (dx as u64 * use_w as u64 / dst_w as u64) as usize;
                let sx = sx.min(use_w - 1);
                let cx = sx / 2;

                let yv = y_plane[sy * y_stride + sx];
                let (u, v) = if interleaved_uv {
                    (
                        uv_plane[cy * uv_stride + cx * 2],
                        uv_plane[cy * uv_stride + cx * 2 + 1],
                    )
                } else {
                    (uv_plane[cy * uv_stride + cx], v_plane[cy * v_stride + cx])
                };

                let (r, g, b) = yuv_to_rgb_bt601(yv, u, v, limited);
                let off = dy * out_stride + dx * bpp;
                out[off] = r;
                out[off + 1] = g;
                out[off + 2] = b;
                if alpha {
                    out[off + 3] = 255;
                }
            }
        }

        Ok(self.packed_video_frame(frame, out, out_stride, dst_w, dst_h))
    }

    /// `Rgb24` → `Rgba`, optionally rescaling with nearest-neighbor sampling.
    fn scale_rgb24_to_rgba(&self, frame: &Frame) -> RsResult<Frame> {
        // Rejecting the mismatch matters: a `Bgr24` frame fed to an `Rgb24`
        // scaler produced a perfectly plausible RGBA image with red and blue
        // swapped, which no later stage can detect.
        Self::require_source_format(frame, PixelFormat::Rgb24)?;
        let (use_w, use_h) = self.source_geometry(frame);
        if frame.data.is_empty() || frame.linesize.is_empty() {
            return Err(RsError::InvalidData("RGB24 frame has no plane data".into()));
        }
        let dst_w = self.config.dst_width as usize;
        let dst_h = self.config.dst_height as usize;
        let use_w = use_w.max(1);
        let use_h = use_h.max(1);

        let src = &frame.data[0];
        let src_stride = frame.linesize[0];
        // `linesize` must cover one full pixel row, otherwise consecutive rows
        // overlap (a 3-byte stride for a 2-pixel RGB24 row duplicated pixels).
        Self::validate_plane(src, src_stride, use_w, use_h, 3, "RGB24")?;
        Self::validate_output_size(dst_w, dst_h, 4)?;
        let need = src_stride
            .checked_mul(use_h.saturating_sub(1))
            .and_then(|o| o.checked_add(use_w.checked_mul(3)?));
        match need {
            Some(n) if src.len() >= n => {}
            _ => {
                return Err(RsError::InvalidData(
                    "RGB24 plane buffer is too small for declared size/linesize".into(),
                ));
            }
        }

        let out_stride = dst_w * 4;
        let mut out = vec![0u8; out_stride * dst_h];
        for dy in 0..dst_h {
            let sy = (dy as u64 * use_h as u64 / dst_h as u64) as usize;
            let sy = sy.min(use_h - 1);
            for dx in 0..dst_w {
                let sx = (dx as u64 * use_w as u64 / dst_w as u64) as usize;
                let sx = sx.min(use_w - 1);
                let s = sy * src_stride + sx * 3;
                let d = dy * out_stride + dx * 4;
                out[d] = src[s];
                out[d + 1] = src[s + 1];
                out[d + 2] = src[s + 2];
                out[d + 3] = 255;
            }
        }

        Ok(self.packed_video_frame(frame, out, out_stride, dst_w, dst_h))
    }

    /// `Yuv420P` → `Yuv420P` with nearest-neighbor resampling of all three planes.
    ///
    /// Previously this returned [`RsError::Unsupported`], so any pipeline that
    /// scaled luma-resolution video in place (for example the bundled
    /// `pipeline` example) failed. Chroma planes are halved in both directions,
    /// so their target size is `ceil(dst/2)` and each source chroma sample is
    /// replicated over the corresponding 2×2 output block.
    fn scale_yuv420p_to_yuv420p(&self, frame: &Frame) -> RsResult<Frame> {
        Self::require_source_format(frame, PixelFormat::Yuv420P)?;
        let (use_w, use_h) = self.source_geometry(frame);
        let use_w = use_w.max(1);
        let use_h = use_h.max(1);
        let (y_plane, u_plane, v_plane, y_stride, u_stride, v_stride) =
            self.validate_yuv420p_planes(frame, use_w, use_h)?;

        let dst_w = self.config.dst_width as usize;
        let dst_h = self.config.dst_height as usize;
        let dst_cw = dst_w.div_ceil(2);
        let dst_ch = dst_h.div_ceil(2);
        let src_cw = use_w.div_ceil(2);
        let src_ch = use_h.div_ceil(2);

        // The configured destination size is caller-supplied: make sure the
        // three output planes can actually be allocated before trying.
        Self::validate_output_size(dst_w, dst_h, 1)?;
        Self::validate_output_size(dst_cw, dst_ch, 1)?;

        // Allocations go through checked arithmetic so pathological sizes report
        // an error instead of aborting on allocation overflow.
        let y_len = dst_w
            .checked_mul(dst_h)
            .ok_or_else(|| RsError::InvalidData("YUV420P luma plane size overflow".into()))?;
        let c_len = dst_cw
            .checked_mul(dst_ch)
            .ok_or_else(|| RsError::InvalidData("YUV420P chroma plane size overflow".into()))?;
        let mut y_out = vec![0u8; y_len];
        let mut u_out = vec![0u8; c_len];
        let mut v_out = vec![0u8; c_len];

        // Precompute the source-column mapping once; it is shared by every row
        // and by both chroma planes.
        let x_map: Vec<usize> = (0..dst_w)
            .map(|dx| ((dx as u64 * use_w as u64 / dst_w as u64) as usize).min(use_w - 1))
            .collect();
        let cx_map: Vec<usize> = (0..dst_cw)
            .map(|dx| ((dx as u64 * src_cw as u64 / dst_cw as u64) as usize).min(src_cw - 1))
            .collect();

        for dy in 0..dst_h {
            let sy = ((dy as u64 * use_h as u64 / dst_h as u64) as usize).min(use_h - 1);
            let src_row = sy * y_stride;
            let dst_row = dy * dst_w;
            for (dx, &sx) in x_map.iter().enumerate() {
                y_out[dst_row + dx] = y_plane[src_row + sx];
            }
        }

        for dy in 0..dst_ch {
            let sy = ((dy as u64 * src_ch as u64 / dst_ch as u64) as usize).min(src_ch - 1);
            let u_row = sy * u_stride;
            let v_row = sy * v_stride;
            let dst_row = dy * dst_cw;
            for (dx, &cx) in cx_map.iter().enumerate() {
                u_out[dst_row + dx] = u_plane[u_row + cx];
                v_out[dst_row + dx] = v_plane[v_row + cx];
            }
        }

        let mut result = clone_frame_metadata(frame, vec![y_out, u_out, v_out], vec![
            dst_w, dst_cw, dst_cw,
        ]);
        result.width = dst_w;
        result.height = dst_h;
        result.pixel_format = PixelFormat::Yuv420P;
        result.sample_rate = 0;
        result.channels = 0;
        result.samples = 0;
        if result.time_base.den == 0 {
            result.time_base = Rational::new(1, 1000);
        }
        Ok(result)
    }
}

/// BT.601 inverse-matrix coefficients in 16.16 fixed point.
///
/// The limited-range set folds the studio-swing expansion (Y `255/219`,
/// UV `255/224`) into the matrix so a pixel conversion is three multiply-adds
/// and three shifts instead of a dozen floating-point operations.
const COEF_Y_LTD: i32 = 76_309; // 255 / 219                  * 65536
const COEF_RV_LTD: i32 = 104_597; // 1.402    * (255 / 224)    * 65536
const COEF_GU_LTD: i32 = 25_675; // 0.344136 * (255 / 224)     * 65536
const COEF_GV_LTD: i32 = 53_279; // 0.714136 * (255 / 224)     * 65536
const COEF_BU_LTD: i32 = 132_201; // 1.772    * (255 / 224)    * 65536

const COEF_RV_FULL: i32 = 91_881; // 1.402    * 65536
const COEF_GU_FULL: i32 = 22_553; // 0.344136 * 65536
const COEF_GV_FULL: i32 = 46_802; // 0.714136 * 65536
const COEF_BU_FULL: i32 = 116_130; // 1.772    * 65536

/// Fixed-point rounding bias (half of the 16.16 fraction).
const ROUND_BIAS: i32 = 1 << 15;

/// Clamp a 16.16 fixed-point colour value to an 8-bit channel.
#[inline]
fn clamp_fixed_u8(v: i32) -> u8 {
    let v = (v + ROUND_BIAS) >> 16;
    if v <= 0 {
        0
    } else if v >= 255 {
        255
    } else {
        v as u8
    }
}

/// BT.601 YUV → RGB conversion (integer fixed point).
///
/// Limited range uses studio swing (Y 16–235, UV 16–240).
/// Full range treats Y/U/V as 0–255 with neutral chroma at 128.
///
/// This is the hot path for every YUV frame shown on screen, so it avoids
/// floating point entirely: each channel is one multiply-add per input
/// component plus a shift, with ±0.5 LSB accuracy versus the exact matrix.
#[inline]
fn yuv_to_rgb_bt601(y: u8, u: u8, v: u8, limited: bool) -> (u8, u8, u8) {
    let d = u as i32 - 128;
    let e = v as i32 - 128;
    let (y_scaled, rv, gu, gv, bu) = if limited {
        let y_scaled = (y as i32 - 16).max(0) * COEF_Y_LTD;
        (y_scaled, COEF_RV_LTD, COEF_GU_LTD, COEF_GV_LTD, COEF_BU_LTD)
    } else {
        (
            y as i32 * 65536,
            COEF_RV_FULL,
            COEF_GU_FULL,
            COEF_GV_FULL,
            COEF_BU_FULL,
        )
    };

    let r = y_scaled + rv * e;
    let g = y_scaled - gu * d - gv * e;
    let b = y_scaled + bu * d;
    (clamp_fixed_u8(r), clamp_fixed_u8(g), clamp_fixed_u8(b))
}

fn clone_frame_metadata(frame: &Frame, data: Vec<Vec<u8>>, linesize: Vec<usize>) -> Frame {
    Frame {
        data,
        linesize,
        width: frame.width,
        height: frame.height,
        pixel_format: frame.pixel_format,
        sample_format: frame.sample_format,
        sample_rate: frame.sample_rate,
        channels: frame.channels,
        samples: frame.samples,
        pts: frame.pts,
        duration: frame.duration,
        time_base: frame.time_base,
        key_frame: frame.key_frame,
        pict_type: frame.pict_type,
    }
}

/// Convert a YUV420P [`Frame`] into packed BGR24 (`Vec<u8>`, 3 bytes/pixel, B,G,R order).
///
/// This mirrors the YUV → RGB math used by the `Yuv420P` → `Rgba` path in [`Scaler::scale`]
/// (BT.601, limited range by default): the same `yuv_to_rgb_bt601` conversion and clamping are
/// applied per pixel. The only differences are that output is 3 bytes/pixel written in
/// **B,G,R** order (no alpha byte), matching FFmpeg's `bgr24` layout.
pub fn yuv420p_frame_to_bgr24(frame: &Frame) -> RsResult<Vec<u8>> {
    if frame.pixel_format != PixelFormat::Yuv420P {
        return Err(RsError::InvalidData(
            format!(
                "yuv420p_frame_to_bgr24 expects Yuv420P, got {:?}",
                frame.pixel_format
            )
            .into(),
        ));
    }
    if frame.data.len() < 3 || frame.linesize.len() < 3 {
        return Err(RsError::InvalidData(
            "YUV420P frame requires 3 planes (Y, U, V)".into(),
        ));
    }

    let w = frame.width;
    let h = frame.height;
    if w == 0 || h == 0 {
        return Err(RsError::InvalidData(
            "yuv420p_frame_to_bgr24 requires non-zero width/height".into(),
        ));
    }

    let y_plane = &frame.data[0];
    let u_plane = &frame.data[1];
    let v_plane = &frame.data[2];
    let y_stride = frame.linesize[0];
    let u_stride = frame.linesize[1];
    let v_stride = frame.linesize[2];

    // Chroma dimensions for 4:2:0 (matches the Yuv420P → Rgba path).
    let chroma_w = w.div_ceil(2);
    let chroma_h = h.div_ceil(2);

    // Validate plane capacity (allow extra padding in linesize), same as scale_yuv420p_to_packed.
    let y_need = y_stride
        .checked_mul(h.saturating_sub(1))
        .and_then(|o| o.checked_add(w));
    let u_need = u_stride
        .checked_mul(chroma_h.saturating_sub(1))
        .and_then(|o| o.checked_add(chroma_w));
    let v_need = v_stride
        .checked_mul(chroma_h.saturating_sub(1))
        .and_then(|o| o.checked_add(chroma_w));
    match (y_need, u_need, v_need) {
        (Some(yn), Some(un), Some(vn))
            if y_plane.len() >= yn && u_plane.len() >= un && v_plane.len() >= vn => {}
        _ => {
            return Err(RsError::InvalidData(
                "YUV420P plane buffers are too small for declared size/linesize".into(),
            ));
        }
    }

    // BT.601 limited range, matching the default Scaler Yuv420P → Rgba conversion.
    let limited = true;
    let mut out = vec![0u8; w * h * 3];

    for y in 0..h {
        let cy = y / 2;
        for x in 0..w {
            let cx = x / 2;
            let yv = y_plane[y * y_stride + x];
            let u = u_plane[cy * u_stride + cx];
            let v = v_plane[cy * v_stride + cx];

            let (r, g, b) = yuv_to_rgb_bt601(yv, u, v, limited);
            let off = (y * w + x) * 3;
            // BGR order, no alpha.
            out[off] = b;
            out[off + 1] = g;
            out[off + 2] = r;
        }
    }

    Ok(out)
}

/// Convert a YUV420P [`Frame`] into packed RGB24 (`Vec<u8>`, 3 bytes/pixel, R,G,B order).
///
/// This mirrors [`yuv420p_frame_to_bgr24`] exactly — the same `yuv_to_rgb_bt601` BT.601
/// (limited range by default) conversion, plane extraction, and chroma-offset logic — but writes
/// the three bytes per pixel in **R,G,B** order (no alpha byte), matching FFmpeg's `rgb24` layout.
pub fn yuv420p_frame_to_rgb24(frame: &Frame) -> RsResult<Vec<u8>> {
    if frame.pixel_format != PixelFormat::Yuv420P {
        return Err(RsError::InvalidData(
            format!(
                "yuv420p_frame_to_rgb24 expects Yuv420P, got {:?}",
                frame.pixel_format
            )
            .into(),
        ));
    }
    if frame.data.len() < 3 || frame.linesize.len() < 3 {
        return Err(RsError::InvalidData(
            "YUV420P frame requires 3 planes (Y, U, V)".into(),
        ));
    }

    let w = frame.width;
    let h = frame.height;
    if w == 0 || h == 0 {
        return Err(RsError::InvalidData(
            "yuv420p_frame_to_rgb24 requires non-zero width/height".into(),
        ));
    }

    let y_plane = &frame.data[0];
    let u_plane = &frame.data[1];
    let v_plane = &frame.data[2];
    let y_stride = frame.linesize[0];
    let u_stride = frame.linesize[1];
    let v_stride = frame.linesize[2];

    // Chroma dimensions for 4:2:0 (matches the Yuv420P → Rgba path).
    let chroma_w = w.div_ceil(2);
    let chroma_h = h.div_ceil(2);

    // Validate plane capacity (allow extra padding in linesize), same as scale_yuv420p_to_packed.
    let y_need = y_stride
        .checked_mul(h.saturating_sub(1))
        .and_then(|o| o.checked_add(w));
    let u_need = u_stride
        .checked_mul(chroma_h.saturating_sub(1))
        .and_then(|o| o.checked_add(chroma_w));
    let v_need = v_stride
        .checked_mul(chroma_h.saturating_sub(1))
        .and_then(|o| o.checked_add(chroma_w));
    match (y_need, u_need, v_need) {
        (Some(yn), Some(un), Some(vn))
            if y_plane.len() >= yn && u_plane.len() >= un && v_plane.len() >= vn => {}
        _ => {
            return Err(RsError::InvalidData(
                "YUV420P plane buffers are too small for declared size/linesize".into(),
            ));
        }
    }

    // BT.601 limited range, matching the default Scaler Yuv420P → Rgba conversion.
    let limited = true;
    let mut out = vec![0u8; w * h * 3];

    for y in 0..h {
        let cy = y / 2;
        for x in 0..w {
            let cx = x / 2;
            let yv = y_plane[y * y_stride + x];
            let u = u_plane[cy * u_stride + cx];
            let v = v_plane[cy * v_stride + cx];

            let (r, g, b) = yuv_to_rgb_bt601(yv, u, v, limited);
            let off = (y * w + x) * 3;
            // RGB order, no alpha.
            out[off] = r;
            out[off + 1] = g;
            out[off + 2] = b;
        }
    }

    Ok(out)
}

/// Convert an NV12 [`Frame`] into packed RGBA (`Vec<u8>`, 4 bytes/pixel, R,G,B,A order).
///
/// NV12 is a semi-planar 4:2:0 format: plane 0 is the full-resolution Y (luma) plane and
/// plane 1 is interleaved UV (a `U` byte followed by a `V` byte per chroma sample). This reuses
/// the same `yuv_to_rgb_bt601` BT.601 (limited range by default) conversion as the YUV420P
/// paths, the same chroma-offset logic, and the same plane-capacity validation style.
pub fn nv12_frame_to_rgba(frame: &Frame) -> RsResult<Vec<u8>> {
    if frame.pixel_format != PixelFormat::Nv12 {
        return Err(RsError::InvalidData(
            format!(
                "nv12_frame_to_rgba expects Nv12, got {:?}",
                frame.pixel_format
            )
            .into(),
        ));
    }
    if frame.data.len() < 2 || frame.linesize.len() < 2 {
        return Err(RsError::InvalidData(
            "NV12 frame requires 2 planes (Y, UV)".into(),
        ));
    }

    let w = frame.width;
    let h = frame.height;
    if w == 0 || h == 0 {
        return Err(RsError::InvalidData(
            "nv12_frame_to_rgba requires non-zero width/height".into(),
        ));
    }

    let y_plane = &frame.data[0];
    let uv_plane = &frame.data[1];
    let y_stride = frame.linesize[0];
    let uv_stride = frame.linesize[1];

    // Chroma dimensions for 4:2:0 (matches the Yuv420P paths).
    let chroma_w = w.div_ceil(2);
    let chroma_h = h.div_ceil(2);

    // Validate plane capacity (allow extra padding in linesize), same as yuv420p_frame_to_rgb24.
    let y_need = y_stride
        .checked_mul(h.saturating_sub(1))
        .and_then(|o| o.checked_add(w));
    let uv_need = uv_stride
        .checked_mul(chroma_h.saturating_sub(1))
        .and_then(|o| o.checked_add(chroma_w * 2));
    match (y_need, uv_need) {
        (Some(yn), Some(uvn)) if y_plane.len() >= yn && uv_plane.len() >= uvn => {}
        _ => {
            return Err(RsError::InvalidData(
                "NV12 plane buffers are too small for declared size/linesize".into(),
            ));
        }
    }

    // BT.601 limited range, matching the default Scaler Yuv420P → Rgba conversion.
    let limited = true;
    let mut out = vec![0u8; w * h * 4];

    for y in 0..h {
        let cy = y / 2;
        for x in 0..w {
            let cx = x / 2;
            let yv = y_plane[y * y_stride + x];
            let u = uv_plane[cy * uv_stride + cx * 2];
            let v = uv_plane[cy * uv_stride + cx * 2 + 1];

            let (r, g, b) = yuv_to_rgb_bt601(yv, u, v, limited);
            let off = (y * w + x) * 4;
            out[off] = r;
            out[off + 1] = g;
            out[off + 2] = b;
            out[off + 3] = 255;
        }
    }

    Ok(out)
}

/// Convert a YUV422P [`Frame`] into packed RGBA (`Vec<u8>`, 4 bytes/pixel, R,G,B,A order).
///
/// YUV422P is a planar 4:2:2 format: plane 0 is the full-resolution Y (luma) plane, plane 1 is the
/// full-height U (chroma) plane, and plane 2 is the full-height V (chroma) plane. Only the
/// horizontal chroma is halved: for each pixel `x` the chroma sample is at `cx = x / 2`, while the
/// vertical chroma index equals the row (`cy = y`). This reuses the same `yuv_to_rgb_bt601`
/// BT.601 (limited range by default) conversion, chroma-offset logic, and plane-capacity validation
/// style as the `yuv420p_frame_to_rgb24` / `nv12_frame_to_rgba` paths.
pub fn yuv422p_frame_to_rgba(frame: &Frame) -> RsResult<Vec<u8>> {
    if frame.pixel_format != PixelFormat::Yuv422P {
        return Err(RsError::InvalidData(
            format!(
                "yuv422p_frame_to_rgba expects Yuv422P, got {:?}",
                frame.pixel_format
            )
            .into(),
        ));
    }
    if frame.data.len() < 3 || frame.linesize.len() < 3 {
        return Err(RsError::InvalidData(
            "YUV422P frame requires 3 planes (Y, U, V)".into(),
        ));
    }

    let w = frame.width;
    let h = frame.height;
    if w == 0 || h == 0 {
        return Err(RsError::InvalidData(
            "yuv422p_frame_to_rgba requires non-zero width/height".into(),
        ));
    }

    let y_plane = &frame.data[0];
    let u_plane = &frame.data[1];
    let v_plane = &frame.data[2];
    let y_stride = frame.linesize[0];
    let u_stride = frame.linesize[1];
    let v_stride = frame.linesize[2];

    // Chroma dimensions for 4:2:2 (half horizontal only, full vertical — matches FFmpeg).
    let chroma_w = w.div_ceil(2);
    let chroma_h = h;

    // Validate plane capacity (allow extra padding in linesize), same as yuv420p_frame_to_rgb24.
    let y_need = y_stride
        .checked_mul(h.saturating_sub(1))
        .and_then(|o| o.checked_add(w));
    let u_need = u_stride
        .checked_mul(chroma_h.saturating_sub(1))
        .and_then(|o| o.checked_add(chroma_w));
    let v_need = v_stride
        .checked_mul(chroma_h.saturating_sub(1))
        .and_then(|o| o.checked_add(chroma_w));
    match (y_need, u_need, v_need) {
        (Some(yn), Some(un), Some(vn))
            if y_plane.len() >= yn && u_plane.len() >= un && v_plane.len() >= vn => {}
        _ => {
            return Err(RsError::InvalidData(
                "YUV422P plane buffers are too small for declared size/linesize".into(),
            ));
        }
    }

    // BT.601 limited range, matching the default Scaler Yuv420P → Rgba conversion.
    let limited = true;
    let mut out = vec![0u8; w * h * 4];

    for y in 0..h {
        // 4:2:2 keeps full vertical chroma resolution.
        let cy = y;
        for x in 0..w {
            let cx = x / 2;
            let yv = y_plane[y * y_stride + x];
            let u = u_plane[cy * u_stride + cx];
            let v = v_plane[cy * v_stride + cx];

            let (r, g, b) = yuv_to_rgb_bt601(yv, u, v, limited);
            let off = (y * w + x) * 4;
            out[off] = r;
            out[off + 1] = g;
            out[off + 2] = b;
            out[off + 3] = 255;
        }
    }

    Ok(out)
}

/// Convert a packed RGB24 [`Frame`] into RGBA (`Vec<u8>`, 4 bytes/pixel, alpha 255).
///
/// RGB24 is a packed 3-bytes/pixel format written in **R,G,B** order (no alpha). This expands the
/// scale toolbox — the inverse of the `Rgb24` packing used by FFmpeg's `rgb24` layout — by copying
/// each pixel's three bytes forward and appending an opaque alpha byte (`255`). It reuses the same
/// `.into()`-on-`RsError::InvalidData` validation style as the `yuv420p_frame_to_rgb24` /
/// `nv12_frame_to_rgba` paths, but with no color conversion (RGB24 is already in RGB order).
pub fn rgb24_frame_to_rgba(frame: &Frame) -> RsResult<Vec<u8>> {
    if frame.pixel_format != PixelFormat::Rgb24 {
        return Err(RsError::InvalidData(
            format!(
                "rgb24_frame_to_rgba expects Rgb24, got {:?}",
                frame.pixel_format
            )
            .into(),
        ));
    }
    let w = frame.width;
    let h = frame.height;
    if w == 0 || h == 0 {
        return Err(RsError::InvalidData(
            "rgb24_frame_to_rgba requires non-zero size".into(),
        ));
    }
    if frame.data.is_empty() {
        return Err(RsError::InvalidData("RGB24 frame has no plane data".into()));
    }

    let src = &frame.data[0];
    let mut out = vec![0u8; w * h * 4];
    for i in 0..w * h {
        let s = i * 3;
        let d = i * 4;
        out[d] = src[s];
        out[d + 1] = src[s + 1];
        out[d + 2] = src[s + 2];
        out[d + 3] = 255;
    }
    Ok(out)
}

/// Build a correctly sized synthetic YUV420P frame (not using broken `Frame::new_video` plane sizes).
#[cfg(test)]
pub(crate) fn make_yuv420p_frame(width: usize, height: usize, y: u8, u: u8, v: u8) -> Frame {
    let y_size = width * height;
    let chroma_w = width.div_ceil(2);
    let chroma_h = height.div_ceil(2);
    let c_size = chroma_w * chroma_h;
    Frame {
        data: vec![vec![y; y_size], vec![u; c_size], vec![v; c_size]],
        linesize: vec![width, chroma_w, chroma_w],
        width,
        height,
        pixel_format: PixelFormat::Yuv420P,
        sample_format: rsmpeg_util::SampleFormat::None,
        sample_rate: 0,
        channels: 0,
        samples: 0,
        pts: Some(0),
        duration: 1,
        time_base: Rational::new(1, 25),
        key_frame: true,
        pict_type: rsmpeg_codec::PictureType::I,
    }
}

/// Build a correctly sized synthetic NV12 frame with interleaved UV plane
/// (not using broken `Frame::new_video` plane sizes). The UV plane is filled with
/// repeated `[u, v]` pairs.
#[cfg(test)]
pub(crate) fn make_nv12_frame(width: usize, height: usize, y: u8, u: u8, v: u8) -> Frame {
    let y_size = width * height;
    let uv_size = width * (height / 2);
    let mut uv = vec![0u8; uv_size];
    for i in 0..uv_size / 2 {
        uv[i * 2] = u;
        uv[i * 2 + 1] = v;
    }
    Frame {
        data: vec![vec![y; y_size], uv],
        linesize: vec![width, width],
        width,
        height,
        pixel_format: PixelFormat::Nv12,
        sample_format: rsmpeg_util::SampleFormat::None,
        sample_rate: 0,
        channels: 0,
        samples: 0,
        pts: Some(0),
        duration: 1,
        time_base: Rational::new(1, 25),
        key_frame: true,
        pict_type: rsmpeg_codec::PictureType::I,
    }
}

/// Build a correctly sized synthetic YUV422P frame (not using broken `Frame::new_video` plane sizes).
/// 4:2:2 keeps full vertical chroma, so each chroma plane is `chroma_w * height` bytes.
#[cfg(test)]
pub(crate) fn make_yuv422p_frame(width: usize, height: usize, y: u8, u: u8, v: u8) -> Frame {
    let y_size = width * height;
    let chroma_w = width.div_ceil(2);
    let c_size = chroma_w * height;
    Frame {
        data: vec![vec![y; y_size], vec![u; c_size], vec![v; c_size]],
        linesize: vec![width, chroma_w, chroma_w],
        width,
        height,
        pixel_format: PixelFormat::Yuv422P,
        sample_format: rsmpeg_util::SampleFormat::None,
        sample_rate: 0,
        channels: 0,
        samples: 0,
        pts: Some(0),
        duration: 1,
        time_base: Rational::new(1, 25),
        key_frame: true,
        pict_type: rsmpeg_codec::PictureType::I,
    }
}

/// Build a correctly sized synthetic RGB24 frame with uniform (r, g, b) pixels
/// (mirrors `make_yuv420p_frame` / `make_nv12_frame` for the packed Rgb24 format).
#[cfg(test)]
pub(crate) fn make_rgb24_frame(width: usize, height: usize, r: u8, g: u8, b: u8) -> Frame {
    let size = width * height * 3;
    let mut data = vec![0u8; size];
    for i in 0..width * height {
        let o = i * 3;
        data[o] = r;
        data[o + 1] = g;
        data[o + 2] = b;
    }
    Frame {
        data: vec![data],
        linesize: vec![width * 3],
        width,
        height,
        pixel_format: PixelFormat::Rgb24,
        sample_format: rsmpeg_util::SampleFormat::None,
        sample_rate: 0,
        channels: 0,
        samples: 0,
        pts: Some(0),
        duration: 1,
        time_base: Rational::new(1, 25),
        key_frame: true,
        pict_type: rsmpeg_codec::PictureType::I,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::colorspace::{ColorConversion, ColorRange, ColorSpace};

    #[test]
    fn test_scaler_creation() {
        let config = ScalerConfig::new(
            1920,
            1080,
            PixelFormat::Yuv420P,
            1280,
            720,
            PixelFormat::Yuv420P,
        );
        let scaler = Scaler::new(config).unwrap();
        assert_eq!(scaler.config().src_width, 1920);
        assert_eq!(scaler.config().dst_width, 1280);
    }

    #[test]
    fn test_scaler_with_options() {
        let config =
            ScalerConfig::new(640, 480, PixelFormat::Yuv420P, 320, 240, PixelFormat::Rgb24)
                .with_interpolation(InterpolationMethod::Lanczos);
        let scaler = Scaler::new(config).unwrap();
        assert_eq!(scaler.config().interpolation, InterpolationMethod::Lanczos);
    }

    #[test]
    fn test_scaler_scale_output_rgb24() {
        let config =
            ScalerConfig::new(640, 480, PixelFormat::Yuv420P, 320, 240, PixelFormat::Rgb24);
        let scaler = Scaler::new(config).unwrap();
        // Manual planes — Frame::new_video sizes are wrong for YUV420P chroma.
        let frame = make_yuv420p_frame(640, 480, 16, 128, 128);
        let out = scaler.scale(&frame).unwrap();
        assert_eq!(out.width, 320);
        assert_eq!(out.height, 240);
        assert_eq!(out.pixel_format, PixelFormat::Rgb24);
        assert_eq!(out.data[0].len(), 320 * 240 * 3);
        assert_eq!(out.linesize[0], 320 * 3);
    }

    #[test]
    fn test_yuv420p_to_rgba_buffer_len_and_black() {
        let w = 8usize;
        let h = 6usize;
        let config = ScalerConfig::new(
            w as u32,
            h as u32,
            PixelFormat::Yuv420P,
            w as u32,
            h as u32,
            PixelFormat::Rgba,
        );
        let scaler = Scaler::new(config).unwrap();
        // Limited-range black: Y=16, U=V=128
        let frame = make_yuv420p_frame(w, h, 16, 128, 128);
        let out = scaler.scale(&frame).unwrap();
        assert_eq!(out.data[0].len(), w * h * 4);
        // First pixel ≈ black
        assert!(out.data[0][0] <= 2, "R={}", out.data[0][0]);
        assert!(out.data[0][1] <= 2, "G={}", out.data[0][1]);
        assert!(out.data[0][2] <= 2, "B={}", out.data[0][2]);
        assert_eq!(out.data[0][3], 255);
    }

    #[test]
    fn test_yuv420p_to_rgba_approx_red() {
        let w = 4usize;
        let h = 4usize;
        let config = ScalerConfig::new(
            w as u32,
            h as u32,
            PixelFormat::Yuv420P,
            w as u32,
            h as u32,
            PixelFormat::Rgba,
        );
        let scaler = Scaler::new(config).unwrap();
        // Classic BT.601 limited red ≈ Y=81, U=90, V=240
        let frame = make_yuv420p_frame(w, h, 81, 90, 240);
        let out = scaler.scale(&frame).unwrap();
        let r = out.data[0][0];
        let g = out.data[0][1];
        let b = out.data[0][2];
        assert!(r > 200, "expected strong red, got R={r}");
        assert!(g < 40, "expected low green, got G={g}");
        assert!(b < 40, "expected low blue, got B={b}");
    }

    #[test]
    fn test_yuv420p_gradient_rgba_len() {
        let w = 16usize;
        let h = 8usize;
        let chroma_w = w.div_ceil(2);
        let chroma_h = h.div_ceil(2);
        let mut y = vec![0u8; w * h];
        for row in 0..h {
            for col in 0..w {
                y[row * w + col] = (16 + (col * 219 / (w - 1))) as u8;
            }
        }
        let frame = Frame {
            data: vec![
                y,
                vec![128u8; chroma_w * chroma_h],
                vec![128u8; chroma_w * chroma_h],
            ],
            linesize: vec![w, chroma_w, chroma_w],
            width: w,
            height: h,
            pixel_format: PixelFormat::Yuv420P,
            sample_format: rsmpeg_util::SampleFormat::None,
            sample_rate: 0,
            channels: 0,
            samples: 0,
            pts: None,
            duration: 0,
            time_base: Rational::new(1, 1000),
            key_frame: false,
            pict_type: rsmpeg_codec::PictureType::None,
        };
        let config = ScalerConfig::new(
            w as u32,
            h as u32,
            PixelFormat::Yuv420P,
            w as u32,
            h as u32,
            PixelFormat::Rgba,
        );
        let scaler = Scaler::new(config).unwrap();
        let out = scaler.scale(&frame).unwrap();
        assert_eq!(out.data[0].len(), w * h * 4);
        // Left ≈ dark, right ≈ bright (luma gradient, neutral chroma)
        let left = out.data[0][0];
        let right = out.data[0][(w - 1) * 4];
        assert!(right > left + 100, "left={left} right={right}");
    }

    #[test]
    fn test_full_range_white() {
        let w = 2usize;
        let h = 2usize;
        let config = ScalerConfig::new(
            w as u32,
            h as u32,
            PixelFormat::Yuv420P,
            w as u32,
            h as u32,
            PixelFormat::Rgba,
        )
        .with_color_conversion(ColorConversion::new(
            ColorSpace::BT601,
            ColorSpace::RGB,
            ColorRange::Full,
            ColorRange::Full,
        ));
        let scaler = Scaler::new(config).unwrap();
        let frame = make_yuv420p_frame(w, h, 255, 128, 128);
        let out = scaler.scale(&frame).unwrap();
        assert!(out.data[0][0] >= 250);
        assert!(out.data[0][1] >= 250);
        assert!(out.data[0][2] >= 250);
    }

    #[test]
    fn test_reuses_config_across_frames() {
        let config = ScalerConfig::new(4, 4, PixelFormat::Yuv420P, 4, 4, PixelFormat::Rgba);
        let scaler = Scaler::new(config).unwrap();
        let f1 = make_yuv420p_frame(4, 4, 16, 128, 128);
        let f2 = make_yuv420p_frame(4, 4, 235, 128, 128);
        let o1 = scaler.scale(&f1).unwrap();
        let o2 = scaler.scale(&f2).unwrap();
        assert_eq!(o1.data[0].len(), 4 * 4 * 4);
        assert_eq!(o2.data[0].len(), 4 * 4 * 4);
        assert!(o2.data[0][0] > o1.data[0][0]);
    }

    #[test]
    fn test_yuv420p_to_bgr24_buffer_len_and_black() {
        let w = 2usize;
        let h = 2usize;
        // Y=0 (limited-range black), neutral chroma U=V=128 → near-black.
        let frame = make_yuv420p_frame(w, h, 0, 128, 128);
        let out = yuv420p_frame_to_bgr24(&frame).unwrap();
        assert_eq!(out.len(), w * h * 3);
        // First pixel ≈ black. Order is B,G,R, but all channels are small.
        assert!(out[0] < 16, "B={}", out[0]);
        assert!(out[1] < 16, "G={}", out[1]);
        assert!(out[2] < 16, "R={}", out[2]);
    }

    #[test]
    fn test_yuv420p_to_bgr24_rejects_wrong_format() {
        let mut frame = make_yuv420p_frame(2, 2, 16, 128, 128);
        // Not a Yuv420P frame — must be rejected.
        frame.pixel_format = PixelFormat::Rgb24;
        let res = yuv420p_frame_to_bgr24(&frame);
        assert!(res.is_err());
    }

    #[test]
    fn test_yuv420p_to_rgb24_buffer_len_and_black() {
        let w = 2usize;
        let h = 2usize;
        // Y=0 (limited-range black), neutral chroma U=V=128 → near-black.
        let frame = make_yuv420p_frame(w, h, 0, 128, 128);
        let out = yuv420p_frame_to_rgb24(&frame).unwrap();
        assert_eq!(out.len(), w * h * 3);
        // First pixel ≈ black. Order is R,G,B, but all channels are small.
        assert!(out[0] < 16, "R={}", out[0]);
        assert!(out[1] < 16, "G={}", out[1]);
        assert!(out[2] < 16, "B={}", out[2]);
    }

    #[test]
    fn test_yuv420p_to_rgb24_rejects_wrong_format() {
        let mut frame = make_yuv420p_frame(2, 2, 16, 128, 128);
        // Not a Yuv420P frame — must be rejected.
        frame.pixel_format = PixelFormat::Rgb24;
        let res = yuv420p_frame_to_rgb24(&frame);
        assert!(res.is_err());
    }

    #[test]
    fn test_nv12_to_rgba_buffer_len_and_black() {
        let w = 2usize;
        let h = 2usize;
        // Y=0 (limited-range black), neutral chroma U=V=128 → near-black.
        let frame = make_nv12_frame(w, h, 0, 128, 128);
        let out = nv12_frame_to_rgba(&frame).unwrap();
        assert_eq!(out.len(), w * h * 4);
        // First pixel ≈ black. Order is R,G,B,A; all color channels small, alpha 255.
        assert!(out[0] < 16, "R={}", out[0]);
        assert!(out[1] < 16, "G={}", out[1]);
        assert!(out[2] < 16, "B={}", out[2]);
        assert_eq!(out[3], 255);
    }

    #[test]
    fn test_nv12_to_rgba_rejects_wrong_format() {
        let mut frame = make_nv12_frame(2, 2, 16, 128, 128);
        // Not an NV12 frame — must be rejected.
        frame.pixel_format = PixelFormat::Rgb24;
        let res = nv12_frame_to_rgba(&frame);
        assert!(res.is_err());
    }

    #[test]
    fn test_yuv422p_to_rgba_buffer_len_and_black() {
        let w = 2usize;
        let h = 2usize;
        // Y=0 (limited-range black), neutral chroma U=V=128 → near-black.
        let frame = make_yuv422p_frame(w, h, 0, 128, 128);
        let out = yuv422p_frame_to_rgba(&frame).unwrap();
        assert_eq!(out.len(), w * h * 4);
        // First pixel ≈ black. Order is R,G,B,A; all color channels small, alpha 255.
        assert!(out[0] < 16, "R={}", out[0]);
        assert!(out[1] < 16, "G={}", out[1]);
        assert!(out[2] < 16, "B={}", out[2]);
        assert_eq!(out[3], 255);
    }

    #[test]
    fn test_yuv422p_to_rgba_rejects_wrong_format() {
        let mut frame = make_yuv422p_frame(2, 2, 16, 128, 128);
        // Not a Yuv422P frame — must be rejected.
        frame.pixel_format = PixelFormat::Rgb24;
        let res = yuv422p_frame_to_rgba(&frame);
        assert!(res.is_err());
    }

    #[test]
    fn test_rgb24_to_rgba_roundtrip() {
        // 2x2 RGB24 frame with known bytes, packed R,G,B order (3 bytes/pixel).
        let rgb = vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120];
        let frame = Frame {
            data: vec![rgb],
            linesize: vec![6],
            width: 2,
            height: 2,
            pixel_format: PixelFormat::Rgb24,
            sample_format: rsmpeg_util::SampleFormat::None,
            sample_rate: 0,
            channels: 0,
            samples: 0,
            pts: Some(0),
            duration: 1,
            time_base: Rational::new(1, 25),
            key_frame: true,
            pict_type: rsmpeg_codec::PictureType::I,
        };
        let out = rgb24_frame_to_rgba(&frame).unwrap();
        // 4 bytes/pixel, alpha 255.
        assert_eq!(out.len(), 16);
        assert_eq!(&out[0..4], &[10, 20, 30, 255]);
        assert_eq!(&out[4..8], &[40, 50, 60, 255]);
        assert_eq!(&out[8..12], &[70, 80, 90, 255]);
        assert_eq!(&out[12..16], &[100, 110, 120, 255]);
    }

    #[test]
    fn test_rgb24_to_rgba_rejects_wrong_format() {
        let mut frame = make_rgb24_frame(2, 2, 16, 128, 128);
        // Not an Rgb24 frame — must be rejected.
        frame.pixel_format = PixelFormat::Yuv420P;
        let res = rgb24_frame_to_rgba(&frame);
        assert!(res.is_err());
    }

    // ── Fixed-point BT.601 conversion ────────────────────────────────

    /// Reference implementation: the exact float BT.601 inverse matrix that the
    /// fixed-point path replaced. Used to prove the integer version stays within
    /// ±1 LSB across the whole 8-bit Y/U/V cube.
    fn reference_yuv_to_rgb(y: u8, u: u8, v: u8, limited: bool) -> (u8, u8, u8) {
        let (yf, uf, vf) = if limited {
            (
                ((y as f32) - 16.0).max(0.0) * (255.0 / 219.0),
                ((u as f32) - 128.0) * (255.0 / 224.0),
                ((v as f32) - 128.0) * (255.0 / 224.0),
            )
        } else {
            (y as f32, (u as f32) - 128.0, (v as f32) - 128.0)
        };
        let r = yf + 1.402 * vf;
        let g = yf - 0.344136 * uf - 0.714136 * vf;
        let b = yf + 1.772 * uf;
        let cl = |x: f32| -> u8 {
            if x <= 0.0 {
                0
            } else if x >= 255.0 {
                255
            } else {
                (x + 0.5) as u8
            }
        };
        (cl(r), cl(g), cl(b))
    }

    #[test]
    fn fixed_point_matches_float_within_one_lsb() {
        // Sweep the full cube with a stride that still hits the extremes: 17^3
        // samples covers 0, 17, …, 255 in every component.
        for limited in [true, false] {
            let mut worst = 0i32;
            for y in (0..=255u16).step_by(17) {
                for u in (0..=255u16).step_by(17) {
                    for v in (0..=255u16).step_by(17) {
                        let got = yuv_to_rgb_bt601(y as u8, u as u8, v as u8, limited);
                        let want = reference_yuv_to_rgb(y as u8, u as u8, v as u8, limited);
                        for (g, w) in [(got.0, want.0), (got.1, want.1), (got.2, want.2)] {
                            worst = worst.max((g as i32 - w as i32).abs());
                        }
                    }
                }
            }
            assert!(
                worst <= 1,
                "fixed-point conversion drifted by {worst} LSB (limited={limited})"
            );
        }
    }

    #[test]
    fn fixed_point_neutral_chroma_is_grey() {
        for limited in [true, false] {
            for y in 0..=255u8 {
                let (r, g, b) = yuv_to_rgb_bt601(y, 128, 128, limited);
                assert_eq!(r, g, "Y={y} limited={limited}: R != G");
                assert_eq!(g, b, "Y={y} limited={limited}: G != B");
            }
        }
    }

    #[test]
    fn fixed_point_limited_range_endpoints() {
        // Studio-swing black and white must map to 0 and 255.
        assert_eq!(yuv_to_rgb_bt601(16, 128, 128, true), (0, 0, 0));
        assert_eq!(yuv_to_rgb_bt601(235, 128, 128, true), (255, 255, 255));
    }

    #[test]
    fn fixed_point_full_range_endpoints() {
        assert_eq!(yuv_to_rgb_bt601(0, 128, 128, false), (0, 0, 0));
        assert_eq!(yuv_to_rgb_bt601(255, 128, 128, false), (255, 255, 255));
    }

    #[test]
    fn fixed_point_saturates_on_extreme_chroma() {
        // Studio-swing red (Y=81, U=90, V=240) must stay strongly red and, most
        // importantly, clamp rather than wrap when a channel overflows.
        let (r, g, b) = yuv_to_rgb_bt601(81, 90, 240, true);
        assert!(r > 200 && g < 40 && b < 40, "got ({r}, {g}, {b})");

        // Deliberately out-of-gamut chroma: an i32 overflow would wrap and show
        // up as a wildly different value, so compare against the float reference
        // (which clamps) at the corners of the cube.
        for limited in [true, false] {
            for (y, u, v) in [
                (255u8, 0u8, 255u8),
                (0, 255, 0),
                (255, 255, 255),
                (0, 0, 0),
                (255, 0, 0),
                (0, 0, 255),
            ] {
                let got = yuv_to_rgb_bt601(y, u, v, limited);
                let want = reference_yuv_to_rgb(y, u, v, limited);
                for (g, w) in [(got.0, want.0), (got.1, want.1), (got.2, want.2)] {
                    assert!(
                        (g as i32 - w as i32).abs() <= 1,
                        "YUV({y},{u},{v}) limited={limited}: got ({}, {}, {}) want {:?}",
                        got.0,
                        got.1,
                        got.2,
                        want
                    );
                }
            }
        }
    }

    #[test]
    fn scaler_rejects_zero_dimensions() {
        let ok = ScalerConfig::new(4, 4, PixelFormat::Yuv420P, 4, 4, PixelFormat::Rgba);
        assert!(Scaler::new(ok).is_ok());

        for (sw, sh, dw, dh) in [(0, 4, 4, 4), (4, 0, 4, 4), (4, 4, 0, 4), (4, 4, 4, 0)] {
            let cfg = ScalerConfig::new(sw, sh, PixelFormat::Yuv420P, dw, dh, PixelFormat::Rgba);
            assert!(
                Scaler::new(cfg).is_err(),
                "expected error for src={sw}x{sh} dst={dw}x{dh}"
            );
        }
    }

    #[test]
    fn scale_rejects_truncated_planes() {
        let cfg = ScalerConfig::new(8, 8, PixelFormat::Yuv420P, 8, 8, PixelFormat::Rgba);
        let scaler = Scaler::new(cfg).unwrap();
        // Declares 8x8 but only provides tiny planes.
        let frame = Frame {
            data: vec![vec![0u8; 4], vec![0u8; 1], vec![0u8; 1]],
            linesize: vec![8, 4, 4],
            width: 8,
            height: 8,
            pixel_format: PixelFormat::Yuv420P,
            sample_format: rsmpeg_util::SampleFormat::None,
            sample_rate: 0,
            channels: 0,
            samples: 0,
            pts: None,
            duration: 0,
            time_base: Rational::new(1, 1000),
            key_frame: false,
            pict_type: rsmpeg_codec::PictureType::None,
        };
        assert!(scaler.scale(&frame).is_err());
    }

    #[test]
    fn scale_identity_copies_pixels() {
        let cfg = ScalerConfig::new(4, 4, PixelFormat::Rgba, 4, 4, PixelFormat::Rgba);
        let scaler = Scaler::new(cfg).unwrap();
        let mut frame = Frame::new_video(4, 4, PixelFormat::Rgba);
        for (i, b) in frame.data[0].iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let out = scaler.scale(&frame).unwrap();
        assert_eq!(out.data[0], frame.data[0]);
        assert_eq!(out.width, 4);
        assert_eq!(out.height, 4);
    }

    #[test]
    fn scale_yuv420p_same_format_downscales_planes() {
        // Regression: `Yuv420P -> Yuv420P` used to return Unsupported, which made
        // `rsmpeg/examples/pipeline.rs` panic. It must now resample every plane.
        let cfg = ScalerConfig::new(8, 6, PixelFormat::Yuv420P, 4, 3, PixelFormat::Yuv420P);
        let scaler = Scaler::new(cfg).unwrap();
        let frame = make_yuv420p_frame(8, 6, 16, 128, 128);
        let out = scaler.scale(&frame).expect("Yuv420P -> Yuv420P must be supported");
        assert_eq!(out.width, 4);
        assert_eq!(out.height, 3);
        assert_eq!(out.pixel_format, PixelFormat::Yuv420P);
        assert_eq!(out.data.len(), 3);
        assert_eq!(out.data[0].len(), 4 * 3);
        assert_eq!(out.data[1].len(), 2 * 2);
        assert_eq!(out.data[2].len(), 2 * 2);
        assert_eq!(out.linesize, vec![4, 2, 2]);
        // Limited-range black stays black.
        assert!(out.data[0].iter().all(|&v| v == 16));
    }

    #[test]
    fn scale_yuv420p_to_yuv420p_keeps_every_plane_sized_consistently() {
        // Every plane of the output must be large enough for its own linesize,
        // otherwise a second scale() call on the result would see truncated planes.
        let cfg = ScalerConfig::new(16, 8, PixelFormat::Yuv420P, 5, 5, PixelFormat::Yuv420P);
        let scaler = Scaler::new(cfg).unwrap();
        let frame = make_yuv420p_frame(16, 8, 235, 128, 128);
        let out = scaler.scale(&frame).unwrap();

        let w = out.width;
        let h = out.height;
        let cw = w.div_ceil(2);
        let ch = h.div_ceil(2);
        assert!(out.data[0].len() >= out.linesize[0] * (h - 1) + w);
        assert!(out.data[1].len() >= out.linesize[1] * (ch - 1) + cw);
        assert!(out.data[2].len() >= out.linesize[2] * (ch - 1) + cw);
    }

    #[test]
    fn scale_yuv420p_to_bgr24_swaps_red_and_blue() {
        let cfg = ScalerConfig::new(4, 2, PixelFormat::Yuv420P, 4, 2, PixelFormat::Bgr24);
        let scaler = Scaler::new(cfg).unwrap();
        let frame = make_yuv420p_frame(4, 2, 81, 90, 240); // BT.601 limited red
        let out = scaler.scale(&frame).unwrap();
        assert_eq!(out.pixel_format, PixelFormat::Bgr24);
        assert_eq!(out.data[0].len(), 4 * 2 * 3);
        let (b, g, r) = (out.data[0][0], out.data[0][1], out.data[0][2]);
        assert!(r > 200, "BGR blue channel must be low, got B={b}");
        assert!(g < 40, "BGR green channel must be low, got G={g}");
        assert!(b < 40, "BGR red channel must be high, got R={b} (byte0={r})");
    }

    #[test]
    fn scale_nv12_to_rgba_downscales() {
        let cfg = ScalerConfig::new(8, 6, PixelFormat::Nv12, 4, 3, PixelFormat::Rgba);
        let scaler = Scaler::new(cfg).unwrap();
        let frame = make_nv12_frame(8, 6, 16, 128, 128);
        let out = scaler.scale(&frame).unwrap();
        assert_eq!(out.pixel_format, PixelFormat::Rgba);
        assert_eq!(out.data[0].len(), 4 * 3 * 4);
        assert!(out.data[0][0] <= 2);
        assert_eq!(out.data[0][3], 255);
    }

    #[test]
    fn scale_nv12_to_rgb24_downscales() {
        let cfg = ScalerConfig::new(8, 6, PixelFormat::Nv12, 2, 2, PixelFormat::Rgb24);
        let scaler = Scaler::new(cfg).unwrap();
        let frame = make_nv12_frame(8, 6, 81, 90, 240);
        let out = scaler.scale(&frame).unwrap();
        assert_eq!(out.pixel_format, PixelFormat::Rgb24);
        assert_eq!(out.data[0].len(), 2 * 2 * 3);
        assert!(out.data[0][0] > 200, "R={}", out.data[0][0]);
    }

    #[test]
    fn scale_yuv422p_to_rgba_uses_full_height_chroma() {
        let cfg = ScalerConfig::new(8, 4, PixelFormat::Yuv422P, 8, 4, PixelFormat::Rgba);
        let scaler = Scaler::new(cfg).unwrap();
        let frame = make_yuv422p_frame(8, 4, 16, 128, 128);
        let out = scaler.scale(&frame).unwrap();
        assert_eq!(out.pixel_format, PixelFormat::Rgba);
        assert_eq!(out.data[0].len(), 8 * 4 * 4);
        assert!(out.data[0][0] <= 2);
    }

    #[test]
    fn scale_rgb24_to_rgba_preserves_colors_and_adds_alpha() {
        let cfg = ScalerConfig::new(4, 4, PixelFormat::Rgb24, 2, 2, PixelFormat::Rgba);
        let scaler = Scaler::new(cfg).unwrap();
        let frame = make_rgb24_frame(4, 4, 10, 20, 30);
        let out = scaler.scale(&frame).unwrap();
        assert_eq!(out.pixel_format, PixelFormat::Rgba);
        assert_eq!(out.data[0].len(), 2 * 2 * 4);
        assert_eq!(&out.data[0][0..4], &[10, 20, 30, 255]);
    }

    #[test]
    fn scale_rejects_frame_with_mismatched_pixel_format() {
        // A scaler configured for Nv12 must not index a Yuv420P frame's planes as
        // semi-planar; it reports InvalidData instead of reading garbage.
        let cfg = ScalerConfig::new(4, 4, PixelFormat::Nv12, 4, 4, PixelFormat::Rgba);
        let scaler = Scaler::new(cfg).unwrap();
        let frame = make_yuv420p_frame(4, 4, 16, 128, 128);
        let err = scaler.scale(&frame).unwrap_err();
        assert!(matches!(err, RsError::InvalidData(_)), "got {err:?}");
    }

    #[test]
    fn scale_rejects_truncated_nv12_and_rgb24_planes() {
        let cfg = ScalerConfig::new(8, 8, PixelFormat::Nv12, 8, 8, PixelFormat::Rgba);
        let scaler = Scaler::new(cfg).unwrap();
        let mut nv12 = make_nv12_frame(8, 8, 16, 128, 128);
        nv12.data[1].truncate(2);
        assert!(scaler.scale(&nv12).is_err());
        // A frame missing the UV plane entirely must also error, not panic.
        let mut missing = make_nv12_frame(8, 8, 16, 128, 128);
        missing.data.truncate(1);
        assert!(scaler.scale(&missing).is_err());

        let cfg = ScalerConfig::new(8, 8, PixelFormat::Rgb24, 8, 8, PixelFormat::Rgba);
        let scaler = Scaler::new(cfg).unwrap();
        let mut rgb = make_rgb24_frame(8, 8, 1, 2, 3);
        rgb.data[0].truncate(5);
        assert!(scaler.scale(&rgb).is_err());
        let mut no_planes = make_rgb24_frame(8, 8, 1, 2, 3);
        no_planes.data.clear();
        assert!(scaler.scale(&no_planes).is_err());
        let mut no_linesize = make_rgb24_frame(8, 8, 1, 2, 3);
        no_linesize.linesize.clear();
        assert!(scaler.scale(&no_linesize).is_err());
    }

    #[test]
    fn rgb24_scaler_rejects_a_bgr24_frame() {
        // Review finding: a BGR red pixel (0,0,255) fed to an Rgb24→Rgba scaler
        // came out as RGBA blue, because only the config was checked.
        let mut bgr = Frame::new_video(1, 1, PixelFormat::Bgr24);
        bgr.data = vec![vec![0, 0, 255]];
        bgr.linesize = vec![3];
        let scaler =
            Scaler::new(ScalerConfig::new(1, 1, PixelFormat::Rgb24, 1, 1, PixelFormat::Rgba))
                .unwrap();
        let err = scaler
            .scale(&bgr)
            .expect_err("a Bgr24 frame must not be read as Rgb24");
        assert!(matches!(err, RsError::InvalidData(_)), "got {err:?}");

        // The correctly labelled frame still converts, with the colours intact.
        let mut rgb = Frame::new_video(1, 1, PixelFormat::Rgb24);
        rgb.data = vec![vec![255, 0, 0]];
        rgb.linesize = vec![3];
        let out = scaler.scale(&rgb).unwrap();
        assert_eq!(out.data[0], vec![255, 0, 0, 255]);
    }

    #[test]
    fn rgb24_scaler_rejects_stride_shorter_than_a_pixel_row() {
        // Review finding: 2x2 RGB24 with linesize 3 made row 1 start inside row
        // 0, duplicating pixels (a 2-pixel row needs 6 bytes).
        let mut overlap = Frame::new_video(2, 2, PixelFormat::Rgb24);
        overlap.data = vec![vec![1, 2, 3, 4, 5, 6, 7, 8, 9]];
        overlap.linesize = vec![3];
        let scaler =
            Scaler::new(ScalerConfig::new(2, 2, PixelFormat::Rgb24, 2, 2, PixelFormat::Rgba))
                .unwrap();
        let err = scaler
            .scale(&overlap)
            .expect_err("a stride shorter than the pixel row must be rejected");
        assert!(matches!(err, RsError::InvalidData(_)), "got {err:?}");

        // With a correct stride the same data converts and rows stay distinct.
        overlap.data = vec![vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]];
        overlap.linesize = vec![6];
        let out = scaler.scale(&overlap).unwrap();
        assert_eq!(out.data[0].len(), 2 * 2 * 4);
        assert_eq!(&out.data[0][0..4], &[1, 2, 3, 255]);
        assert_eq!(&out.data[0][4..8], &[4, 5, 6, 255]);
        assert_eq!(&out.data[0][8..12], &[7, 8, 9, 255]);
        assert_eq!(&out.data[0][12..16], &[10, 11, 12, 255]);
    }

    #[test]
    fn planar_paths_reject_stride_shorter_than_a_plane_row() {
        // YUV420P with linesize 2 for an 8-wide luma plane: rows would overlap.
        let cfg = ScalerConfig::new(8, 4, PixelFormat::Yuv420P, 8, 4, PixelFormat::Rgba);
        let scaler = Scaler::new(cfg).unwrap();
        let mut frame = make_yuv420p_frame(8, 4, 16, 128, 128);
        frame.linesize[0] = 2;
        assert!(scaler.scale(&frame).is_err(), "luma stride must cover a row");

        // Chroma stride smaller than the chroma row.
        let mut frame = make_yuv420p_frame(8, 4, 16, 128, 128);
        frame.linesize[1] = 1;
        assert!(scaler.scale(&frame).is_err(), "chroma stride must cover a row");

        // Same for the Yuv420P → Yuv420P path.
        let cfg = ScalerConfig::new(8, 4, PixelFormat::Yuv420P, 4, 2, PixelFormat::Yuv420P);
        let scaler = Scaler::new(cfg).unwrap();
        let mut frame = make_yuv420p_frame(8, 4, 16, 128, 128);
        frame.linesize[2] = 1;
        assert!(scaler.scale(&frame).is_err());

        // NV12 / Yuv422P paths.
        let cfg = ScalerConfig::new(8, 4, PixelFormat::Nv12, 4, 2, PixelFormat::Rgba);
        let scaler = Scaler::new(cfg).unwrap();
        let mut nv12 = make_nv12_frame(8, 4, 16, 128, 128);
        nv12.linesize[1] = 2; // needs 8 bytes for 4 interleaved chroma pairs
        assert!(scaler.scale(&nv12).is_err());

        let cfg = ScalerConfig::new(8, 4, PixelFormat::Yuv422P, 4, 2, PixelFormat::Rgba);
        let scaler = Scaler::new(cfg).unwrap();
        let mut yuv422 = make_yuv422p_frame(8, 4, 16, 128, 128);
        yuv422.linesize[1] = 1;
        assert!(scaler.scale(&yuv422).is_err());
    }

    #[test]
    fn wrong_frame_format_is_rejected_on_every_source_path() {
        // A frame carrying a different format than the config must never be
        // reinterpreted with the configured layout.
        let cases = [
            (
                ScalerConfig::new(4, 4, PixelFormat::Yuv420P, 4, 4, PixelFormat::Rgba),
                make_nv12_frame(4, 4, 16, 128, 128),
            ),
            (
                ScalerConfig::new(4, 4, PixelFormat::Nv12, 4, 4, PixelFormat::Rgba),
                make_yuv420p_frame(4, 4, 16, 128, 128),
            ),
            (
                ScalerConfig::new(4, 4, PixelFormat::Yuv422P, 4, 4, PixelFormat::Rgba),
                make_yuv420p_frame(4, 4, 16, 128, 128),
            ),
        ];
        for (cfg, frame) in cases {
            let scaler = Scaler::new(cfg).unwrap();
            let err = scaler
                .scale(&frame)
                .expect_err("mismatched frame format must be rejected");
            assert!(matches!(err, RsError::InvalidData(_)), "got {err:?}");
        }

        // Yuv420P → Yuv420P with a *different* size goes through the resampling
        // path (a same-size pair is the documented no-op copy).
        let scaler =
            Scaler::new(ScalerConfig::new(4, 4, PixelFormat::Yuv420P, 2, 2, PixelFormat::Yuv420P))
                .unwrap();
        let nv12 = make_nv12_frame(4, 4, 16, 128, 128);
        let err = scaler
            .scale(&nv12)
            .expect_err("mismatched frame format must be rejected when resizing");
        assert!(matches!(err, RsError::InvalidData(_)), "got {err:?}");
    }

    #[test]
    fn identity_path_rejects_a_mismatched_source_format() {
        // Second review round: same-size Rgb24 -> Rgb24 used to accept a Bgr24
        // frame and hand it straight back, colours silently swapped.
        let mut bgr = Frame::new_video(1, 1, PixelFormat::Bgr24);
        bgr.data = vec![vec![0, 0, 255]];
        bgr.linesize = vec![3];
        let scaler =
            Scaler::new(ScalerConfig::new(1, 1, PixelFormat::Rgb24, 1, 1, PixelFormat::Rgb24))
                .unwrap();
        let err = scaler
            .scale(&bgr)
            .expect_err("identity path must validate the frame's own format");
        assert!(matches!(err, RsError::InvalidData(_)), "got {err:?}");

        // The correctly labelled frame still round-trips unchanged.
        let mut rgb = Frame::new_video(1, 1, PixelFormat::Rgb24);
        rgb.data = vec![vec![255, 0, 0]];
        rgb.linesize = vec![3];
        let out = scaler.scale(&rgb).unwrap();
        assert_eq!(out.pixel_format, PixelFormat::Rgb24);
        assert_eq!(out.data[0], vec![255, 0, 0]);
    }

    #[test]
    fn identity_path_rejects_overlapping_or_truncated_planes() {
        let scaler =
            Scaler::new(ScalerConfig::new(2, 2, PixelFormat::Rgb24, 2, 2, PixelFormat::Rgb24))
                .unwrap();
        // Overlapping rows (linesize 3 for a 6-byte row).
        let mut overlap = Frame::new_video(2, 2, PixelFormat::Rgb24);
        overlap.data = vec![vec![1, 2, 3, 4, 5, 6, 7, 8, 9]];
        overlap.linesize = vec![3];
        assert!(scaler.scale(&overlap).is_err(), "short stride must be rejected");

        // Truncated plane.
        let mut truncated = Frame::new_video(2, 2, PixelFormat::Rgb24);
        truncated.data[0].truncate(5);
        assert!(scaler.scale(&truncated).is_err(), "short plane must be rejected");

        // Zero-sized frame.
        let empty = Frame::new_video(0, 0, PixelFormat::Rgb24);
        assert!(scaler.scale(&empty).is_err());

        // Correct frame still passes through.
        let ok = Frame::new_video(2, 2, PixelFormat::Rgb24);
        assert!(scaler.scale(&ok).is_ok());
    }

    #[test]
    fn review_identity_rejects_mismatched_geometry() {
        // Third review round: a 1x1 frame handed to a 2x2 scaler came back as
        // 1x1, silently violating the requested destination size.
        let rgb = Frame::new_video(1, 1, PixelFormat::Rgb24);
        let scaler =
            Scaler::new(ScalerConfig::new(2, 2, PixelFormat::Rgb24, 2, 2, PixelFormat::Rgb24))
                .unwrap();
        let err = scaler.scale(&rgb).expect_err("geometry must match the config");
        assert!(matches!(err, RsError::InvalidData(_)), "got {err:?}");

        // A frame that does match still round-trips.
        let ok = Frame::new_video(2, 2, PixelFormat::Rgb24);
        let out = scaler.scale(&ok).unwrap();
        assert_eq!((out.width, out.height), (2, 2));
    }

    #[test]
    fn review_gray16_identity_rejects_an_eight_bit_stride() {
        // 16-bit formats need a 2-bytes-per-pixel stride; an 8-bit stride was
        // accepted and copied verbatim.
        let mut gray = Frame::new_video(2, 2, PixelFormat::Gray16);
        assert_eq!(gray.linesize[0], 4, "Frame::new_video gives a 16-bit stride");
        gray.linesize[0] = 2;
        gray.data[0].truncate(4);
        let scaler =
            Scaler::new(ScalerConfig::new(2, 2, PixelFormat::Gray16, 2, 2, PixelFormat::Gray16))
                .unwrap();
        let err = scaler.scale(&gray).expect_err("8-bit stride must be rejected");
        assert!(matches!(err, RsError::InvalidData(_)), "got {err:?}");

        // The correctly sized frame is accepted.
        let good = Frame::new_video(2, 2, PixelFormat::Gray16);
        assert!(scaler.scale(&good).is_ok());
    }

    #[test]
    fn review_high_depth_yuv_identity_requires_chroma_planes() {
        // 10/12-bit YUV is planar: a frame carrying only luma must be rejected
        // instead of being passed through as a "valid" 1-plane image.
        let mut frame = Frame::new_video(4, 2, PixelFormat::Yuv420P10);
        assert_eq!(frame.data.len(), 3);
        frame.data.truncate(1);
        frame.linesize.truncate(1);
        let scaler = Scaler::new(ScalerConfig::new(
            4,
            2,
            PixelFormat::Yuv420P10,
            4,
            2,
            PixelFormat::Yuv420P10,
        ))
        .unwrap();
        let err = scaler.scale(&frame).expect_err("chroma planes are required");
        assert!(matches!(err, RsError::InvalidData(_)), "got {err:?}");

        // 16-bit sample widths are enforced per plane, too.
        let mut narrow = Frame::new_video(4, 2, PixelFormat::Yuv420P10);
        narrow.linesize[0] = 4; // needs 8 bytes for 4 16-bit samples
        assert!(scaler.scale(&narrow).is_err());

        let good = Frame::new_video(4, 2, PixelFormat::Yuv420P10);
        assert!(scaler.scale(&good).is_ok());
    }

    #[test]
    fn identity_capacity_uses_real_plane_lengths() {
        // The old estimate multiplied the first plane's length by the pixel
        // count; for 1920x1080 RGB24 that is ~12.9e12 "bytes" and would reject a
        // perfectly valid frame (and overflow on 32-bit).
        let frame = Frame::new_video(1920, 1080, PixelFormat::Rgb24);
        let scaler =
            Scaler::new(ScalerConfig::new(1920, 1080, PixelFormat::Rgb24, 1920, 1080, PixelFormat::Rgb24))
                .unwrap();
        let out = scaler.scale(&frame).expect("valid identity copy");
        assert_eq!(out.data[0].len(), 1920 * 1080 * 3);
    }

    #[test]
    fn every_output_path_checks_the_destination_size() {
        // A destination that overflows `usize` when multiplied out must be
        // reported by *all* paths, including Nv12/Yuv422P -> packed which the
        // second review found unchecked.
        let huge = u32::MAX;
        let nv12 = make_nv12_frame(4, 4, 16, 128, 128);
        let scaler = Scaler::new(ScalerConfig::new(4, 4, PixelFormat::Nv12, huge, huge, PixelFormat::Rgba))
            .unwrap();
        assert!(matches!(scaler.scale(&nv12), Err(RsError::InvalidData(_))));

        let rgb = make_nv12_frame(4, 4, 16, 128, 128);
        let scaler = Scaler::new(ScalerConfig::new(4, 4, PixelFormat::Nv12, huge, huge, PixelFormat::Rgb24))
            .unwrap();
        assert!(matches!(scaler.scale(&rgb), Err(RsError::InvalidData(_))));

        let yuv422 = make_yuv422p_frame(4, 4, 16, 128, 128);
        let scaler = Scaler::new(ScalerConfig::new(4, 4, PixelFormat::Yuv422P, huge, huge, PixelFormat::Rgba))
            .unwrap();
        assert!(matches!(scaler.scale(&yuv422), Err(RsError::InvalidData(_))));

        let yuv420 = make_yuv420p_frame(4, 4, 16, 128, 128);
        let scaler = Scaler::new(ScalerConfig::new(4, 4, PixelFormat::Yuv420P, huge, huge, PixelFormat::Bgr24))
            .unwrap();
        assert!(matches!(scaler.scale(&yuv420), Err(RsError::InvalidData(_))));

        let scaler = Scaler::new(ScalerConfig::new(4, 4, PixelFormat::Yuv420P, huge, huge, PixelFormat::Yuv420P))
            .unwrap();
        assert!(matches!(scaler.scale(&yuv420), Err(RsError::InvalidData(_))));
    }

    #[test]
    fn output_size_overflow_is_reported_not_allocated() {
        // A gigantic destination must report an error instead of attempting an
        // impossible allocation (usize overflow on 32-bit, OOM everywhere).
        let cfg = ScalerConfig::new(
            4,
            4,
            PixelFormat::Rgb24,
            u32::MAX,
            u32::MAX,
            PixelFormat::Rgba,
        );
        let scaler = Scaler::new(cfg).unwrap();
        let frame = make_rgb24_frame(4, 4, 1, 2, 3);
        let err = scaler
            .scale(&frame)
            .expect_err("an unallocatable output size must be rejected");
        assert!(matches!(err, RsError::InvalidData(_)), "got {err:?}");
    }


    #[test]
    fn yuv420p_to_rgb24_rejects_truncated_planes() {
        let mut frame = make_yuv420p_frame(4, 4, 16, 128, 128);
        frame.data[0].truncate(2);
        assert!(yuv420p_frame_to_rgb24(&frame).is_err());
        assert!(yuv420p_frame_to_bgr24(&frame).is_err());
    }

    #[test]
    fn downscale_nearest_neighbor_picks_correct_source_pixels() {
        // 4x4 luma ramp downscaled to 2x2: nearest mapping (dx*4/2 = 0, 2) means
        // the output must sample source columns 0 and 2.
        let w = 4usize;
        let h = 4usize;
        let mut y = vec![0u8; w * h];
        for row in 0..h {
            for col in 0..w {
                // 16 (black) for cols 0-1, 235 (white) for cols 2-3.
                y[row * w + col] = if col < 2 { 16 } else { 235 };
            }
        }
        let frame = Frame {
            data: vec![y, vec![128u8; 4], vec![128u8; 4]],
            linesize: vec![w, 2, 2],
            width: w,
            height: h,
            pixel_format: PixelFormat::Yuv420P,
            sample_format: rsmpeg_util::SampleFormat::None,
            sample_rate: 0,
            channels: 0,
            samples: 0,
            pts: None,
            duration: 0,
            time_base: Rational::new(1, 25),
            key_frame: false,
            pict_type: rsmpeg_codec::PictureType::None,
        };
        let cfg = ScalerConfig::new(4, 4, PixelFormat::Yuv420P, 2, 2, PixelFormat::Rgba);
        let scaler = Scaler::new(cfg).unwrap();
        let out = scaler.scale(&frame).unwrap();
        assert_eq!(out.data[0].len(), 2 * 2 * 4);
        // Left output column comes from a black source pixel, right from white.
        assert!(out.data[0][0] < 16, "left R={}", out.data[0][0]);
        assert!(out.data[0][4] > 240, "right R={}", out.data[0][4]);
    }
}
