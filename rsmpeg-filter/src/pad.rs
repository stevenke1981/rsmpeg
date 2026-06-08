use rsmpeg_util::MediaType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadDirection {
    Input,
    Output,
}

/// Filter pad descriptor (equivalent to AVFilterPad).
#[derive(Debug, Clone)]
pub struct Pad {
    pub name: String,
    pub direction: PadDirection,
    pub media_type: MediaType,
}

impl Pad {
    pub fn input(name: &str, media_type: MediaType) -> Self {
        Pad {
            name: name.to_string(),
            direction: PadDirection::Input,
            media_type,
        }
    }

    pub fn output(name: &str, media_type: MediaType) -> Self {
        Pad {
            name: name.to_string(),
            direction: PadDirection::Output,
            media_type,
        }
    }
}
