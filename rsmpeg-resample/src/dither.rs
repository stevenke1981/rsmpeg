/// Dithering methods for audio sample format conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DitherMethod {
    None,
    Rectangular,
    Triangular,
    TriangularHf,
    Shibata,
    ModifiedShibata,
    ImprovedEWeighted,
}

impl DitherMethod {
    pub fn name(&self) -> &'static str {
        match self {
            DitherMethod::None => "none",
            DitherMethod::Rectangular => "rectangular",
            DitherMethod::Triangular => "triangular",
            DitherMethod::TriangularHf => "triangular_hf",
            DitherMethod::Shibata => "shibata",
            DitherMethod::ModifiedShibata => "modified_shibata",
            DitherMethod::ImprovedEWeighted => "improved_e_weighted",
        }
    }
}

/// Noise shaping for dithering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoiseShaping {
    None,
    Low,
    Medium,
    High,
}

impl NoiseShaping {
    pub fn name(&self) -> &'static str {
        match self {
            NoiseShaping::None => "none",
            NoiseShaping::Low => "low",
            NoiseShaping::Medium => "medium",
            NoiseShaping::High => "high",
        }
    }
}
