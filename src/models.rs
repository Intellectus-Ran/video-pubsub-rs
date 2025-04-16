use dust_dds::topic_definition::type_support::DdsType;

#[derive(Debug, DdsType)]
pub struct FrameData {
    pub index: u64,
    pub byte_data: Vec<u8>,
}

#[derive(Debug, DdsType)]
pub struct VideoMetadata {
    pub fps: f64,
    pub width: u32,
    pub height: u32,
}

pub const DOMAIN_ID: u32 = 22;