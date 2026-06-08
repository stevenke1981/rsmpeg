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
    pub fn new(src_layout: ChannelLayout, dst_layout: ChannelLayout) -> Self {
        let nb_in = src_layout.channels();
        let nb_out = dst_layout.channels();

        // Build identity-like mixing matrix (default: passthrough for matching channels)
        let mut matrix = vec![vec![0.0_f64; nb_in]; nb_out];
        for i in 0..nb_out.min(nb_in) {
            matrix[i][i] = 1.0;
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
