use rsmpeg_util::ChannelLayout;

/// Channel remapping matrix (N input → N output channel weights).
#[derive(Debug, Clone)]
pub struct ChannelMapping {
    pub src_layout: ChannelLayout,
    pub dst_layout: ChannelLayout,
    /// Linear mapping matrix as [output_channel][input_channel] weights.
    pub matrix: Vec<Vec<f64>>,
    /// Number of input channels.
    pub nb_input_channels: usize,
    /// Number of output channels.
    pub nb_output_channels: usize,
}

impl ChannelMapping {
    /// Build the default remixing matrix from `src_layout` to `dst_layout`.
    ///
    /// Channels are ordered by their [`ChannelLayout`] bit (for 5.1: FL, FR,
    /// FC, LFE, BL, BR).
    ///
    /// - A channel present in both layouts is passed through.
    /// - A channel missing from the destination is folded into its neighbours:
    ///   centre and surround channels reach the front pair at `1/√2`, back and
    ///   side channels substitute for each other, and LFE is dropped.
    /// - A mono source feeds the front left and right channels at unity gain.
    /// - A mono destination is the sum of the stereo downmix's two rows.
    ///
    /// The whole matrix is then scaled down, if needed, so that no row's weights
    /// add up to more than one: full-scale input cannot clip, and the balance
    /// between output channels is preserved. Stereo → mono is therefore
    /// `0.5·L + 0.5·R`.
    ///
    /// A destination channel that no source channel reaches has an all-zero row.
    pub fn new(src_layout: ChannelLayout, dst_layout: ChannelLayout) -> Self {
        let nb_in = src_layout.channels();
        let nb_out = dst_layout.channels();

        let to_mono = dst_layout == ChannelLayout::MONO && src_layout != ChannelLayout::MONO;
        let target = if to_mono {
            ChannelLayout::STEREO
        } else {
            dst_layout
        };
        let outputs: Vec<ChannelLayout> = layout_channels(target).collect();
        let mut matrix = vec![vec![0.0_f64; nb_in]; outputs.len()];
        for (in_ch, channel) in layout_channels(src_layout).enumerate() {
            for (destination, weight) in routes(channel, src_layout, target) {
                if let Some(out_ch) = outputs.iter().position(|&c| c == destination) {
                    matrix[out_ch][in_ch] += weight;
                }
            }
        }
        if to_mono {
            let mono = (0..nb_in)
                .map(|in_ch| matrix.iter().map(|row| row[in_ch]).sum())
                .collect();
            matrix = vec![mono];
        }

        let loudest = matrix
            .iter()
            .map(|row| row.iter().sum::<f64>())
            .fold(0.0_f64, f64::max);
        if loudest > 1.0 {
            for weight in matrix.iter_mut().flatten() {
                *weight /= loudest;
            }
        }

        ChannelMapping {
            src_layout,
            dst_layout,
            matrix,
            nb_input_channels: nb_in,
            nb_output_channels: nb_out,
        }
    }

    /// Set a coefficient in the remixing matrix.
    pub fn set_coefficient(&mut self, out_ch: usize, in_ch: usize, coeff: f64) {
        if out_ch < self.nb_output_channels && in_ch < self.nb_input_channels {
            self.matrix[out_ch][in_ch] = coeff;
        }
    }

    /// Get a coefficient from the remixing matrix.
    pub fn coefficient(&self, out_ch: usize, in_ch: usize) -> f64 {
        if out_ch < self.nb_output_channels && in_ch < self.nb_input_channels {
            self.matrix[out_ch][in_ch]
        } else {
            0.0
        }
    }
}

/// The single-channel flags of `layout`, in channel (bit) order.
fn layout_channels(layout: ChannelLayout) -> impl Iterator<Item = ChannelLayout> {
    (0..u64::BITS)
        .map(|bit| 1_u64 << bit)
        .filter(move |mask| layout.bits() & mask != 0)
        .map(ChannelLayout::from_bits_retain)
}

/// Where the source `channel` goes in `dst`, as `(destination channel, weight)`.
fn routes(
    channel: ChannelLayout,
    src: ChannelLayout,
    dst: ChannelLayout,
) -> Vec<(ChannelLayout, f64)> {
    use std::f64::consts::FRAC_1_SQRT_2 as MINUS_3_DB;
    type C = ChannelLayout;

    if dst.contains(channel) {
        return vec![(channel, 1.0)];
    }
    let pair = |left, right, weight| vec![(left, weight), (right, weight)];
    let has_front = dst.contains(C::STEREO);
    // The destination channel standing in for a missing left or right channel.
    let side = |substitute: C, front: C| {
        if dst.contains(substitute) {
            vec![(substitute, 1.0)]
        } else if dst.contains(front) {
            vec![(front, MINUS_3_DB)]
        } else {
            Vec::new()
        }
    };

    if channel == C::FRONT_CENTER && has_front {
        let weight = if src == C::MONO { 1.0 } else { MINUS_3_DB };
        pair(C::FRONT_LEFT, C::FRONT_RIGHT, weight)
    } else if channel == C::FRONT_LEFT_OF_CENTER && dst.contains(C::FRONT_LEFT) {
        vec![(C::FRONT_LEFT, 1.0)]
    } else if channel == C::FRONT_RIGHT_OF_CENTER && dst.contains(C::FRONT_RIGHT) {
        vec![(C::FRONT_RIGHT, 1.0)]
    } else if channel == C::BACK_LEFT {
        side(C::SIDE_LEFT, C::FRONT_LEFT)
    } else if channel == C::BACK_RIGHT {
        side(C::SIDE_RIGHT, C::FRONT_RIGHT)
    } else if channel == C::SIDE_LEFT {
        side(C::BACK_LEFT, C::FRONT_LEFT)
    } else if channel == C::SIDE_RIGHT {
        side(C::BACK_RIGHT, C::FRONT_RIGHT)
    } else if channel == C::BACK_CENTER {
        if dst.contains(C::BACK_LEFT | C::BACK_RIGHT) {
            pair(C::BACK_LEFT, C::BACK_RIGHT, MINUS_3_DB)
        } else if dst.contains(C::SIDE_LEFT | C::SIDE_RIGHT) {
            pair(C::SIDE_LEFT, C::SIDE_RIGHT, MINUS_3_DB)
        } else if has_front {
            pair(C::FRONT_LEFT, C::FRONT_RIGHT, 0.5)
        } else {
            Vec::new()
        }
    } else {
        // LFE, and channels with no sensible neighbour in `dst`.
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_row(row: &[f64], want: &[f64]) {
        assert_eq!(row.len(), want.len(), "{row:?}");
        for (got, want) in row.iter().zip(want) {
            assert!((got - want).abs() < 1e-12, "{row:?}");
        }
    }

    #[test]
    fn test_channel_mapping_creation() {
        let mapping = ChannelMapping::new(ChannelLayout::STEREO, ChannelLayout::MONO);
        assert_eq!(mapping.nb_input_channels, 2);
        assert_eq!(mapping.nb_output_channels, 1);
    }

    #[test]
    fn test_channel_mapping_coefficients() {
        let mut mapping = ChannelMapping::new(ChannelLayout::STEREO, ChannelLayout::MONO);
        // Downmix: L = 0.5*L + 0.5*R
        mapping.set_coefficient(0, 0, 0.5);
        mapping.set_coefficient(0, 1, 0.5);
        assert!((mapping.coefficient(0, 0) - 0.5).abs() < 1e-10);
        assert!((mapping.coefficient(0, 1) - 0.5).abs() < 1e-10);
    }

    #[test]
    fn identical_layouts_are_the_identity() {
        for layout in [
            ChannelLayout::MONO,
            ChannelLayout::STEREO,
            ChannelLayout::_5POINT1,
            ChannelLayout::_7POINT1,
        ] {
            let mapping = ChannelMapping::new(layout, layout);
            for (out, row) in mapping.matrix.iter().enumerate() {
                for (input, &weight) in row.iter().enumerate() {
                    assert_eq!(weight, f64::from(u8::from(out == input)), "{layout:?}");
                }
            }
        }
    }

    #[test]
    fn stereo_to_mono_averages_both_channels() {
        let mapping = ChannelMapping::new(ChannelLayout::STEREO, ChannelLayout::MONO);
        assert_row(&mapping.matrix[0], &[0.5, 0.5]);
    }

    #[test]
    fn mono_feeds_both_front_channels_at_unity() {
        let mapping = ChannelMapping::new(ChannelLayout::MONO, ChannelLayout::STEREO);
        assert_row(&mapping.matrix[0], &[1.0]);
        assert_row(&mapping.matrix[1], &[1.0]);
    }

    #[test]
    fn five_point_one_to_stereo_folds_centre_and_back_without_lfe() {
        let mapping = ChannelMapping::new(ChannelLayout::_5POINT1, ChannelLayout::STEREO);
        let k = std::f64::consts::FRAC_1_SQRT_2;
        let front = 1.0 / (1.0 + 2.0 * k);
        let fold = k * front;
        // Input order: FL, FR, FC, LFE, BL, BR.
        assert_row(&mapping.matrix[0], &[front, 0.0, fold, 0.0, fold, 0.0]);
        assert_row(&mapping.matrix[1], &[0.0, front, fold, 0.0, 0.0, fold]);
    }

    #[test]
    fn five_point_one_to_mono_is_normalised_and_symmetric() {
        let mapping = ChannelMapping::new(ChannelLayout::_5POINT1, ChannelLayout::MONO);
        let row = &mapping.matrix[0];
        assert!((row.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        assert_eq!(row[0], row[1]);
        assert_eq!(row[4], row[5]);
        assert_eq!(row[3], 0.0, "LFE is dropped");
        assert!(row[2] > row[0], "centre reaches both sides of the downmix");
    }

    #[test]
    fn seven_point_one_to_five_point_one_merges_sides_into_backs() {
        let mapping = ChannelMapping::new(ChannelLayout::_7POINT1, ChannelLayout::_5POINT1);
        // Input order: FL, FR, FC, LFE, BL, BR, SL, SR.
        assert_row(
            &mapping.matrix[4],
            &[0.0, 0.0, 0.0, 0.0, 0.5, 0.0, 0.5, 0.0],
        );
        assert_row(
            &mapping.matrix[0],
            &[0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        );
    }

    #[test]
    fn upmix_leaves_unfed_rows_empty() {
        let mapping = ChannelMapping::new(ChannelLayout::STEREO, ChannelLayout::_5POINT1);
        assert_row(&mapping.matrix[0], &[1.0, 0.0]);
        assert_row(&mapping.matrix[1], &[0.0, 1.0]);
        for row in &mapping.matrix[2..] {
            assert_row(row, &[0.0, 0.0]);
        }
    }
}
