pub struct RasterParameters {}

pub struct RasterEntry {
    pub x: i16,
    pub y: i16,
    pub count: u16,
    pub time: u16,
}

pub struct RasterData {
    pub raster_parameters: RasterParameters,

    pub entries: Vec<RasterEntry>,
}
