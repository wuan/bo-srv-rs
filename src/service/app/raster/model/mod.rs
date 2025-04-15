pub(crate) mod global;
pub(crate) mod region;
pub(crate) mod local;

pub(crate) trait RelativeTimeRange {
    fn interval_duration(&self) -> u16;
    fn interval_offset(&self) -> u16;
}

pub(crate) trait RasterQuery {
    fn raster_baselength(&self) -> u16;
    fn count_threshold(&self) -> u8;
}



pub struct RasterParameters {
    pub longitude_start: f64,
    pub longitude_delta: f64,
    pub longitude_bins: u16,
    pub latitude_start: f64,
    pub latitude_delta: f64,
    pub latitude_bins: u16,
}

pub struct RasterEntry {
    pub x: i16,
    pub y: i16,
    pub count: u16,
    pub time: u16,
}

pub struct RasterData {
    pub parameters: RasterParameters,
    pub entries: Vec<RasterEntry>,
}
