use serde::{Deserialize, Serialize};

/// Picture type, equivalent to FFmpeg's AVPictureType.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PictureType {
    None,
    I,
    P,
    B,
    Si,
    Sp,
}

impl PictureType {
    pub fn name(self) -> &'static str {
        match self {
            PictureType::None => "none",
            PictureType::I => "I",
            PictureType::P => "P",
            PictureType::B => "B",
            PictureType::Si => "SI",
            PictureType::Sp => "SP",
        }
    }
}
